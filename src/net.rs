use core::str::FromStr;
use std::net::TcpStream;
use std::net::ToSocketAddrs;

use async_io::Async;
use dotenvy_macro::dotenv;
use embassy_time::with_timeout;
use esp_idf_svc::hal::reset::restart;
use esp_idf_svc::io::{self, Write};
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::tls::EspAsyncTls;
use esp_idf_svc::wifi::{AsyncWifi, ClientConfiguration, Configuration, EspWifi};
use http::Request;
use log::info;
use palette::rgb::Rgb;
use url::Url;

use crate::{anyesp, convert_error, EspTlsSocket, Leds};

const IS_INTERACTIVE: bool = cfg!(feature = "interactive");

#[derive(Debug, serde::Deserialize)]
struct GithubResponse {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct GithubAsset {
    browser_download_url: String,
    name: String,
}

pub async fn generate_tls(url: &str) -> anyhow::Result<EspAsyncTls<EspTlsSocket>> {
    let url = Url::from_str(url).unwrap();
    let host = url.host_str().unwrap();
    let addr = format!("{host}:443")
        .to_socket_addrs()
        .unwrap()
        .collect::<Vec<_>>();

    let socket = Async::<TcpStream>::connect(addr[0]).await.unwrap();

    let mut tls = esp_idf_svc::tls::EspAsyncTls::adopt(EspTlsSocket::new(socket)).unwrap();

    tls.negotiate(host, &esp_idf_svc::tls::Config::new())
        .await
        .unwrap();

    Ok(tls)
}

pub fn create_raw_request_no_body<T>(request: &http::Request<T>) -> String {
    let method = request.method();
    let uri = request.uri();
    let headers = request.headers();

    let mut request_text = format!("{} {} HTTP/1.1\r\n", method, uri);
    for (key, value) in headers {
        request_text.push_str(&format!("{}: {}\r\n", key, value.to_str().unwrap()));
    }
    request_text.push_str("\r\n"); // End of headers

    request_text
}

pub fn create_raw_request<T: ToString>(request: &http::Request<T>) -> String {
    let mut text = create_raw_request_no_body(request);

    text.push_str(&request.body().to_string());

    text
}

pub async fn handle_redirect(url: &str) -> anyhow::Result<EspAsyncTls<EspTlsSocket>> {
    let request = Request::builder()
        .method("GET")
        .header("User-Agent", "PHSign/1.0.0")
        .header("Host", "github.com")
        .uri(url)
        .body(())
        .unwrap();

    let mut tls = generate_tls(url).await?;

    let request_text = create_raw_request_no_body(&request);

    tls.write_all(request_text.as_bytes())
        .await
        .map_err(convert_error)?;

    let mut body = [0; 8192];

    let _read = io::utils::asynch::try_read_full(&mut tls, &mut body)
        .await
        .map_err(|(e, _)| e)
        .unwrap();

    let body = String::from_utf8(body.into()).expect("valid UTF8");

    let splits = body.split("\r\n");

    for split in splits {
        if split.to_lowercase().starts_with("location: ") {
            let location = split.split(": ").nth(1).expect("location value");

            let request = Request::builder()
                .method("GET")
                .header("User-Agent", "PHSign/1.0.0")
                .header("Host", "githubusercontent.com")
                .uri(location)
                .body(())
                .unwrap();

            let tls = generate_tls(location).await?;
            let request_text = create_raw_request_no_body(&request);

            tls.write_all(request_text.as_bytes())
                .await
                .map_err(convert_error)?;

            return Ok(tls);
        }
    }

    unreachable!("location must be in returned value!")
}

pub async fn self_update(leds: &mut Leds) -> anyhow::Result<()> {
    info!("Checking for self-update");

    let manifest: GithubResponse = {
        let url = "https://api.github.com/repos/purduehackers/sign-firmware/releases/latest";

        let request = Request::builder()
            .method("GET")
            .header("User-Agent", "PHSign/1.0.0")
            .header("Host", "api.github.com")
            .uri(url)
            .body(())
            .unwrap();

        let mut tls = generate_tls(url).await?;

        let request_text = create_raw_request_no_body(&request);

        tls.write_all(request_text.as_bytes())
            .await
            .map_err(convert_error)?;

        let mut body = [0; 8192];

        let _read = io::utils::asynch::try_read_full(&mut tls, &mut body)
            .await
            .map_err(|(e, _)| e)
            .unwrap();

        let body = String::from_utf8(body.into()).expect("valid UTF8");

        let ind = body.find("\r\n\r\n").expect("body start");

        serde_json::from_str(body[ind + 4..].trim().trim_end_matches(char::from(0)))
            .expect("Valid parse for GitHub manifest")
    };

    let local = semver::Version::new(
        env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap(),
        env!("CARGO_PKG_VERSION_MINOR").parse().unwrap(),
        env!("CARGO_PKG_VERSION_PATCH").parse().unwrap(),
    );

    let remote = semver::Version::from_str(&manifest.tag_name[1..]).expect("valid semver");

    if remote > local {
        info!("New release found! Downloading and updating");
        leds.set_all_colors(Rgb::new(0, 255, 0));
        // Grab new release and update
        let url = manifest
            .assets
            .into_iter()
            .find(|asset| {
                asset.name
                    == if IS_INTERACTIVE {
                        "sign-firmware-interactive.bin"
                    } else {
                        "sign-firmware.bin"
                    }
            })
            .expect("release to contain assets")
            .browser_download_url
            .clone();

        let tls = handle_redirect(&url).await?;

        // Consume until \r\n\r\n (body)
        info!("Consuming headers...");
        {
            #[derive(Debug)]
            enum ParseConsumerState {
                None,
                FirstCR,
                FirstNL,
                SecondCR,
            }

            let mut state = ParseConsumerState::None;

            let mut consumption_buffer = [0; 1];

            loop {
                let read = tls
                    .read(&mut consumption_buffer)
                    .await
                    .map_err(convert_error)
                    .expect("read byte for parse consumer");

                if read == 0 {
                    panic!("Invalid update parse! Reached EOF before valid body");
                }
                state = match state {
                    ParseConsumerState::None => {
                        if consumption_buffer[0] == b'\r' {
                            ParseConsumerState::FirstCR
                        } else {
                            ParseConsumerState::None
                        }
                    }
                    ParseConsumerState::FirstCR => {
                        if consumption_buffer[0] == b'\n' {
                            ParseConsumerState::FirstNL
                        } else {
                            ParseConsumerState::None
                        }
                    }
                    ParseConsumerState::FirstNL => {
                        if consumption_buffer[0] == b'\r' {
                            ParseConsumerState::SecondCR
                        } else {
                            ParseConsumerState::None
                        }
                    }
                    ParseConsumerState::SecondCR => {
                        if consumption_buffer[0] == b'\n' {
                            break;
                        } else {
                            ParseConsumerState::None
                        }
                    }
                }
            }
        }

        info!("Headers consumed");

        let mut body = [0; 8192];

        let mut ota = EspOta::new().expect("ESP OTA success");

        let mut update = ota.initiate_update().expect("update to initialize");

        let mut chunk = 0_usize;
        loop {
            let read =
                with_timeout(embassy_time::Duration::from_secs(10), tls.read(&mut body)).await;

            match read {
                Ok(Ok(read)) => {
                    info!("[CHUNK {chunk:>4}] Read {read:>4}");

                    update.write_all(&body[..read]).expect("write update data");

                    if read == 0 {
                        break;
                    }

                    chunk += 1;
                }
                Ok(Err(e)) => e.panic(),
                Err(_) => break,
            };
        }

        info!("Update completed! Activating...");

        update
            .finish()
            .expect("update finalization to work")
            .activate()
            .expect("activation to work");

        restart();
    } else {
        info!("Already on latest version.");
    }

    Ok(())
}

pub async fn connect_to_network(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    let config = Configuration::Client(ClientConfiguration {
        ssid: dotenv!("WIFI_SSID").try_into().unwrap(),
        password: "".try_into().unwrap(),
        auth_method: esp_idf_svc::wifi::AuthMethod::WPA2Enterprise,
        ..Default::default()
    });

    wifi.set_configuration(&config).map_err(convert_error)?;

    unsafe {
        use esp_idf_svc::sys::*;
        anyesp!(esp_wifi_set_mode(wifi_mode_t_WIFI_MODE_STA))?;
        anyesp!(esp_eap_client_set_identity(
            dotenv!("WIFI_EMAIL").as_ptr(),
            dotenv!("WIFI_EMAIL").len() as i32
        ))?;
        anyesp!(esp_eap_client_set_username(
            dotenv!("WIFI_USERNAME").as_ptr(),
            dotenv!("WIFI_USERNAME").len() as i32
        ))?;
        anyesp!(esp_eap_client_set_password(
            dotenv!("WIFI_PASSWORD").as_ptr(),
            dotenv!("WIFI_PASSWORD").len() as i32
        ))?;
        anyesp!(esp_eap_client_set_ttls_phase2_method(
            esp_eap_ttls_phase2_types_ESP_EAP_TTLS_PHASE2_MSCHAPV2
        ))?;
        anyesp!(esp_wifi_sta_enterprise_enable())?;
    }

    wifi.start().await.map_err(convert_error)?;

    // Connect but with a longer timeout
    wifi.wifi_mut().connect().map_err(convert_error)?;
    wifi.wifi_wait(
        |this| this.wifi().is_connected().map(|s| !s),
        Some(std::time::Duration::from_secs(10)),
    )
    .await?;

    wifi.wait_netif_up().await.map_err(convert_error)?;

    info!("Wi-Fi connected!");

    Ok(())
}
