pub mod ble;
pub mod config;
pub mod http;
pub mod self_update;
pub mod ws;

use core::str::FromStr;
use std::net::TcpStream;
use std::net::ToSocketAddrs;

use async_io_mini::Async;
use dotenvy_macro::dotenv;
use esp_idf_svc::tls::EspAsyncTls;
use esp_idf_svc::wifi::{AsyncWifi, ClientConfiguration, Configuration, EspWifi};
use log::info;
use url::Url;

use crate::{anyesp, convert_error, EspTlsSocket};

pub use config::{DeviceConfig, WifiNetwork};
pub use self_update::self_update;

pub async fn generate_tls(url: &str) -> anyhow::Result<EspAsyncTls<EspTlsSocket>> {
    let url = Url::from_str(url)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("No host in URL"))?;
    let addr = format!("{host}:443")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("DNS resolution failed for {host}"))?;

    let socket = Async::<TcpStream>::connect(addr).await?;
    let mut tls = EspAsyncTls::adopt(EspTlsSocket::new(socket))?;
    tls.negotiate(host, &esp_idf_svc::tls::Config::new())
        .await?;

    Ok(tls)
}

enum NetworkSetupInfo {
    Enterprise {
        ssid: String,
        email: String,
        username: String,
        password: String,
    },
    Personal {
        ssid: String,
        password: String,
    },
}

impl NetworkSetupInfo {
    fn print_debug(&self) {
        info!(
            "Attempting to connect to '{}'",
            match self {
                Self::Enterprise { ssid, .. } => ssid.as_str(),
                Self::Personal { ssid, .. } => ssid.as_str(),
            }
        )
    }
}

impl From<WifiNetwork> for NetworkSetupInfo {
    fn from(net: WifiNetwork) -> Self {
        match net.network_type {
            config::NetworkType::Enterprise => NetworkSetupInfo::Enterprise {
                ssid: net.ssid,
                email: net.enterprise_email.unwrap_or_default(),
                username: net.enterprise_username.unwrap_or_default(),
                password: net.password,
            },
            config::NetworkType::Personal => NetworkSetupInfo::Personal {
                ssid: net.ssid,
                password: net.password,
            },
        }
    }
}

async fn try_connect_to_network(
    wifi: &mut AsyncWifi<EspWifi<'static>>,
    network: &NetworkSetupInfo,
) -> anyhow::Result<()> {
    network.print_debug();

    match network {
        NetworkSetupInfo::Enterprise {
            ssid,
            email,
            username,
            password,
        } => {
            let config = Configuration::Client(ClientConfiguration {
                ssid: ssid.as_str().try_into().unwrap(),
                password: "".try_into().unwrap(),
                auth_method: esp_idf_svc::wifi::AuthMethod::WPA2Enterprise,
                ..Default::default()
            });

            wifi.set_configuration(&config).map_err(convert_error)?;

            unsafe {
                use esp_idf_svc::sys::*;
                anyesp!(esp_wifi_set_mode(wifi_mode_t_WIFI_MODE_STA))?;
                anyesp!(esp_eap_client_set_identity(
                    email.as_ptr(),
                    email.len() as i32
                ))?;
                anyesp!(esp_eap_client_set_username(
                    username.as_ptr(),
                    username.len() as i32
                ))?;
                anyesp!(esp_eap_client_set_password(
                    password.as_ptr(),
                    password.len() as i32
                ))?;
                anyesp!(esp_eap_client_set_ttls_phase2_method(
                    esp_eap_ttls_phase2_types_ESP_EAP_TTLS_PHASE2_MSCHAPV2
                ))?;
                anyesp!(esp_wifi_sta_enterprise_enable())?;
                anyesp!(esp_wifi_set_ps(
                    esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE
                ))?;
            }
        }
        NetworkSetupInfo::Personal { ssid, password } => {
            let config = Configuration::Client(ClientConfiguration {
                ssid: ssid.as_str().try_into().unwrap(),
                password: password.as_str().try_into().unwrap(),
                auth_method: esp_idf_svc::wifi::AuthMethod::WPAWPA2Personal,
                ..Default::default()
            });

            wifi.set_configuration(&config).map_err(convert_error)?;

            unsafe {
                use esp_idf_svc::sys::*;
                anyesp!(esp_wifi_set_mode(wifi_mode_t_WIFI_MODE_STA))?;
                anyesp!(esp_wifi_set_ps(
                    esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE
                ))?;
            }
        }
    }

    wifi.wifi_mut().connect().map_err(convert_error)?;
    wifi.wifi_wait(
        |this| this.wifi().is_connected().map(|s| !s),
        Some(std::time::Duration::from_secs(10)),
    )
    .await?;

    wifi.wait_netif_up().await.map_err(convert_error)?;

    Ok(())
}

pub async fn connect_to_network(
    wifi: &mut AsyncWifi<EspWifi<'static>>,
    device_config: &DeviceConfig,
) -> anyhow::Result<()> {
    connect_to_network_with(wifi, device_config.get_wifi_networks()).await
}

pub async fn connect_to_network_with(
    wifi: &mut AsyncWifi<EspWifi<'static>>,
    nvs_networks: Vec<WifiNetwork>,
) -> anyhow::Result<()> {
    let nvs_networks: Vec<NetworkSetupInfo> = nvs_networks
        .into_iter()
        .map(NetworkSetupInfo::from)
        .collect();

    if !nvs_networks.is_empty() {
        info!(
            "Found {} provisioned WiFi network(s) in NVS",
            nvs_networks.len()
        );
    }

    // Then: fall back to compiled-in bootstrap networks
    let bootstrap_networks = [
        NetworkSetupInfo::Enterprise {
            ssid: dotenv!("PAL3_SSID").to_string(),
            email: dotenv!("PAL3_EMAIL").to_string(),
            username: dotenv!("PAL3_USERNAME").to_string(),
            password: dotenv!("PAL3_PASSWORD").to_string(),
        },
        NetworkSetupInfo::Personal {
            ssid: dotenv!("JACK_SSID").to_string(),
            password: dotenv!("JACK_PASSWORD").to_string(),
        },
    ];

    let networks: Vec<NetworkSetupInfo> =
        nvs_networks.into_iter().chain(bootstrap_networks).collect();

    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        for network in networks.iter() {
            wifi.start().await.map_err(convert_error)?;
            match try_connect_to_network(wifi, network).await {
                Ok(()) => {
                    info!("Wi-Fi connected!");
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("Connection attempt failed: {e}");
                    wifi.stop().await.map_err(convert_error)?;
                }
            }
        }

        if attempt < MAX_ATTEMPTS {
            let backoff_secs = 2u64.pow(attempt - 1);
            log::warn!(
                "All networks failed (attempt {attempt}/{MAX_ATTEMPTS}), retrying in {backoff_secs}s..."
            );
            embassy_time::Timer::after_secs(backoff_secs).await;
        }
    }

    anyhow::bail!("No network connection found after {MAX_ATTEMPTS} attempts!");
}

/// Overridable at build time so a bench sign can point at a preview
/// deployment: `SIGN_API_BASE=my-preview.vercel.app cargo run`.
const API_HOST: &str = match option_env!("SIGN_API_BASE") {
    Some(host) => host,
    None => "api.purduehackers.com",
};

/// Idle time before the firmware probes the connection with a ping.
const KEEPALIVE_IDLE_SECS: u64 = 30;
/// Idle time after which the connection is declared dead. A silently
/// dropped TCP connection produces no error — without this the receive
/// loop would hang forever.
const KEEPALIVE_DEAD_SECS: u64 = 45;
const BACKOFF_CAP_SECS: u64 = 60;

pub async fn ws_listen(
    config: std::sync::Arc<std::sync::Mutex<DeviceConfig>>,
    leds: crate::script::SharedLeds,
    script_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let mut runner = crate::script::ScriptRunner::new(leds, script_active, events_tx);
    let url = format!("wss://{API_HOST}/sign/ws");
    let mut backoff_secs: u64 = 1;

    loop {
        info!("Connecting to WebSocket...");
        match ws::WebSocket::connect(&url).await {
            Ok(mut ws_conn) => {
                let auth = serde_json::json!({
                    "type": "auth",
                    "key": dotenv!("PHACK_API_KEY"),
                })
                .to_string();
                let status = serde_json::json!({
                    "type": "status",
                    "version": env!("CARGO_PKG_VERSION"),
                })
                .to_string();

                if let Err(e) = ws_conn.send(&ws::WsMessage::Text(auth)).await {
                    log::error!("WebSocket auth failed: {e}");
                } else {
                    let _ = ws_conn.send(&ws::WsMessage::Text(status)).await;
                    info!("WebSocket connected, auth sent");
                    let mut idle_secs: u64 = 0;
                    let mut ping_sent = false;

                    loop {
                        forward_script_events(&events_rx, &mut ws_conn).await;

                        match ws_conn
                            .recv_timeout(embassy_time::Duration::from_secs(1))
                            .await
                        {
                            Ok(Some(ws::WsMessage::Text(text))) => {
                                idle_secs = 0;
                                ping_sent = false;
                                match handle_ws_command(&text, &mut ws_conn, &config, &mut runner)
                                    .await
                                {
                                    // Benign traffic proves the server accepted
                                    // us. A server `error` (bad key) must NOT
                                    // reset the backoff, or a rejected sign
                                    // would hammer the API at the floor rate.
                                    Ok(true) => backoff_secs = 1,
                                    Ok(false) => {}
                                    Err(e) => log::error!("Error handling WS command: {e}"),
                                }
                            }
                            Ok(Some(ws::WsMessage::Close)) => {
                                info!("WebSocket closed by server");
                                break;
                            }
                            Ok(Some(_)) => {
                                idle_secs = 0;
                                ping_sent = false;
                            }
                            Ok(None) => {
                                idle_secs += 1;
                                if idle_secs >= KEEPALIVE_DEAD_SECS {
                                    log::warn!(
                                        "No traffic for {idle_secs}s, reconnecting WebSocket"
                                    );
                                    break;
                                }
                                if idle_secs >= KEEPALIVE_IDLE_SECS && !ping_sent {
                                    let ping = serde_json::json!({ "type": "ping" }).to_string();
                                    if ws_conn.send(&ws::WsMessage::Text(ping)).await.is_err() {
                                        break;
                                    }
                                    ping_sent = true;
                                }
                            }
                            Err(e) => {
                                log::error!("WebSocket error: {e}");
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("WebSocket connection failed: {e}");
            }
        }

        // The running script keeps rendering while the link is down; the
        // server re-converges it on reconnect.
        let jitter_ms = if backoff_secs > 0 {
            let mut random = [0u8; 4];
            unsafe {
                esp_idf_svc::sys::esp_fill_random(random.as_mut_ptr() as *mut core::ffi::c_void, 4);
            }
            u32::from_le_bytes(random) as u64 % (backoff_secs * 250)
        } else {
            0
        };
        info!("Reconnecting WebSocket in {backoff_secs}s (+{jitter_ms}ms)...");
        embassy_time::Timer::after_millis(backoff_secs * 1000 + jitter_ms).await;
        backoff_secs = (backoff_secs * 2).min(BACKOFF_CAP_SECS);
    }
}

async fn forward_script_events(
    events: &std::sync::mpsc::Receiver<crate::script::ScriptEvent>,
    ws_conn: &mut ws::WebSocket,
) {
    use crate::script::ScriptEvent;

    while let Ok(event) = events.try_recv() {
        let message = match event {
            ScriptEvent::Started { request_id } => {
                serde_json::json!({ "type": "script_ack", "request_id": request_id })
            }
            ScriptEvent::Rejected {
                request_id,
                message,
                line,
                position,
            } => serde_json::json!({
                "type": "script_error",
                "request_id": request_id,
                "message": message,
                "line": line,
                "position": position,
            }),
            ScriptEvent::Done => serde_json::json!({ "type": "script_done" }),
            ScriptEvent::Failed { message } => {
                serde_json::json!({ "type": "script_error", "message": message })
            }
        };
        if let Err(e) = ws_conn
            .send(&ws::WsMessage::Text(message.to_string()))
            .await
        {
            log::error!("Failed to report script event: {e}");
        }
    }
}

async fn handle_ws_command(
    text: &str,
    ws_conn: &mut ws::WebSocket,
    config: &std::sync::Arc<std::sync::Mutex<DeviceConfig>>,
    runner: &mut crate::script::ScriptRunner,
) -> anyhow::Result<bool> {
    let msg: serde_json::Value = serde_json::from_str(text)?;
    let msg_type = msg["type"].as_str().unwrap_or("");
    let request_id = msg["request_id"].as_str().unwrap_or("");

    match msg_type {
        "get_wifi" => {
            let networks = config.lock().unwrap().get_wifi_networks();
            let resp = serde_json::json!({
                "type": "wifi_networks",
                "request_id": request_id,
                "networks": networks,
            });
            ws_conn.send(&ws::WsMessage::Text(resp.to_string())).await?;
        }
        "set_wifi" => {
            let networks: Vec<WifiNetwork> = serde_json::from_value(msg["networks"].clone())?;
            config.lock().unwrap().set_wifi_networks(&networks)?;
            let resp = serde_json::json!({
                "type": "wifi_ack",
                "request_id": request_id,
            });
            ws_conn.send(&ws::WsMessage::Text(resp.to_string())).await?;
        }
        "set_script" => {
            let script = msg["script"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("set_script without a script"))?;
            if script.len() > script_env::MAX_SCRIPT_BYTES {
                let resp = serde_json::json!({
                    "type": "script_error",
                    "request_id": request_id,
                    "message": format!(
                        "script is {} bytes; the sign accepts at most {}",
                        script.len(),
                        script_env::MAX_SCRIPT_BYTES
                    ),
                });
                ws_conn.send(&ws::WsMessage::Text(resp.to_string())).await?;
            } else {
                // The ack or error comes back through the event channel
                // once the script thread has compiled it.
                runner.start(request_id.to_string(), script.to_string());
            }
        }
        "clear_script" => {
            runner.stop();
            let resp = serde_json::json!({
                "type": "script_ack",
                "request_id": request_id,
            });
            ws_conn.send(&ws::WsMessage::Text(resp.to_string())).await?;
        }
        "pong" => {}
        "error" => {
            log::error!("Server error: {}", msg["message"].as_str().unwrap_or("?"));
            return Ok(false);
        }
        other => {
            info!("Unknown WS command: {other}");
        }
    }

    Ok(true)
}
