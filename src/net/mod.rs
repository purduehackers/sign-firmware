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
        ssid: &'static str,
        email: &'static str,
        username: &'static str,
        password: &'static str,
    },
    Personal {
        ssid: &'static str,
        password: &'static str,
    },
}

impl NetworkSetupInfo {
    fn print_debug(&self) {
        info!(
            "Attempting to connect to '{}'",
            match self {
                Self::Enterprise { ssid, .. } => ssid,
                Self::Personal { ssid, .. } => ssid,
            }
        )
    }
}

async fn try_connect_to_network(
    wifi: &mut AsyncWifi<EspWifi<'static>>,
    network: NetworkSetupInfo,
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
                ssid: ssid.try_into().unwrap(),
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
                ssid: ssid.try_into().unwrap(),
                password: password.try_into().unwrap(),
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

pub async fn connect_to_network(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    let pal3 = NetworkSetupInfo::Enterprise {
        ssid: dotenv!("PAL3_SSID"),
        email: dotenv!("PAL3_EMAIL"),
        username: dotenv!("PAL3_USERNAME"),
        password: dotenv!("PAL3_PASSWORD"),
    };

    let hotspot = NetworkSetupInfo::Personal {
        ssid: dotenv!("JACK_SSID"),
        password: dotenv!("JACK_PASSWORD"),
    };

    for network in [pal3, hotspot] {
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
