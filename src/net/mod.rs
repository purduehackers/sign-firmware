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
    network: NetworkSetupInfo,
) -> anyhow::Result<()> {
    network.print_debug();

    match &network {
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
    // First: try NVS-stored WiFi networks (provisioned via WebSocket)
    let nvs_networks: Vec<NetworkSetupInfo> = device_config
        .get_wifi_networks()
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

    for network in nvs_networks.into_iter().chain(bootstrap_networks) {
        wifi.start().await.map_err(convert_error)?;
        match try_connect_to_network(wifi, network).await {
            Ok(()) => {
                break;
            }
            Err(_) => {
                wifi.stop().await.map_err(convert_error)?;
            }
        }
    }

    if !wifi.is_started()? {
        anyhow::bail!("No network connection found!");
    }

    info!("Wi-Fi connected!");

    Ok(())
}

const WS_URL: &str = "wss://api.purduehackers.com/sign/ws";
const PROVISION_URL: &str = "https://api.purduehackers.com/sign/provision";

pub async fn provision_device(config: &mut DeviceConfig) -> anyhow::Result<()> {
    if config.get_device_key().is_some() {
        info!("Device already provisioned");
        return Ok(());
    }

    info!("No device key found, provisioning...");

    if let Some(new_key) = option_env!("PROVISION_KEY") {
        info!("Found provisioning key in env.");
        return config.set_device_key(new_key);
    }

    let resp = http::http_post(
        PROVISION_URL,
        &[("Content-Type", "application/json")],
        b"{}",
    )
    .await?;

    if resp.status != 200 {
        anyhow::bail!("Provisioning failed with status {}", resp.status);
    }

    #[derive(serde::Deserialize)]
    struct ProvisionResponse {
        key: String,
    }

    let body_str = core::str::from_utf8(&resp.body)?;
    let provision: ProvisionResponse = serde_json::from_str(body_str)?;

    config.set_device_key(&provision.key)?;
    info!("Device provisioned successfully");

    Ok(())
}

pub async fn ws_listen(key: String, config: std::sync::Arc<std::sync::Mutex<DeviceConfig>>) {
    loop {
        info!("Connecting to WebSocket...");
        match ws::WebSocket::connect(WS_URL).await {
            Ok(mut ws_conn) => {
                let auth = serde_json::json!({ "type": "auth", "key": key }).to_string();
                if let Err(e) = ws_conn.send(&ws::WsMessage::Text(auth)).await {
                    log::error!("WebSocket auth failed: {e}");
                } else {
                    info!("WebSocket authenticated");
                    loop {
                        match ws_conn.recv().await {
                            Ok(ws::WsMessage::Text(text)) => {
                                if let Err(e) =
                                    handle_ws_command(&text, &mut ws_conn, &config).await
                                {
                                    log::error!("Error handling WS command: {e}");
                                }
                            }
                            Ok(ws::WsMessage::Close) => {
                                info!("WebSocket closed by server");
                                break;
                            }
                            Ok(_) => {}
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

        info!("Reconnecting WebSocket in 5s...");
        embassy_time::Timer::after_millis(5000).await;
    }
}

async fn handle_ws_command(
    text: &str,
    ws_conn: &mut ws::WebSocket,
    config: &std::sync::Arc<std::sync::Mutex<DeviceConfig>>,
) -> anyhow::Result<()> {
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
            ws_conn
                .send(&ws::WsMessage::Text(resp.to_string()))
                .await?;
        }
        "set_wifi" => {
            let networks: Vec<WifiNetwork> = serde_json::from_value(msg["networks"].clone())?;
            config.lock().unwrap().set_wifi_networks(&networks)?;
            let resp = serde_json::json!({
                "type": "wifi_ack",
                "request_id": request_id,
            });
            ws_conn
                .send(&ws::WsMessage::Text(resp.to_string()))
                .await?;
        }
        other => {
            info!("Unknown WS command: {other}");
        }
    }

    Ok(())
}
