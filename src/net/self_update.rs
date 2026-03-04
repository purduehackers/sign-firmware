use core::str::FromStr;

use embassy_time::with_timeout;
use esp_idf_svc::hal::reset::restart;
use esp_idf_svc::io::Write;
use esp_idf_svc::ota::EspOta;
use log::info;
use palette::rgb::Rgb;

use crate::Leds;

use super::http;

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

pub async fn self_update(leds: &mut Leds) -> anyhow::Result<()> {
    leds.set_all_colors(Rgb::new(0, 0, 255));

    info!("Checking for self-update");

    let resp = http::http_get(
        "https://api.github.com/repos/purduehackers/sign-firmware/releases/latest",
        &[],
    )
    .await?;

    let body_str = core::str::from_utf8(&resp.body)?;
    let manifest: GithubResponse =
        serde_json::from_str(body_str.trim().trim_end_matches(char::from(0)))?;

    let local = semver::Version::new(
        env!("CARGO_PKG_VERSION_MAJOR").parse()?,
        env!("CARGO_PKG_VERSION_MINOR").parse()?,
        env!("CARGO_PKG_VERSION_PATCH").parse()?,
    );

    let remote = semver::Version::from_str(&manifest.tag_name[1..])?;

    if remote > local {
        info!("New release found! Downloading and updating");
        leds.set_all_colors(Rgb::new(0, 255, 0));

        let asset_name = if IS_INTERACTIVE {
            "sign-firmware.bin"
        } else {
            "sign-firmware-passive.bin"
        };

        let url = manifest
            .assets
            .into_iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| anyhow::anyhow!("Release missing asset {asset_name}"))?
            .browser_download_url;

        let tls = http::follow_redirect_stream(&url, &[]).await?;

        let mut body = [0u8; 8192];
        let mut ota = EspOta::new()?;
        let mut update = ota.initiate_update()?;

        let mut chunk = 0_usize;
        loop {
            let read =
                with_timeout(embassy_time::Duration::from_secs(10), tls.read(&mut body)).await;

            match read {
                Ok(Ok(read)) => {
                    info!("[CHUNK {chunk:>4}] Read {read:>4}");
                    update.write_all(&body[..read])?;
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

        update.finish()?.activate()?;

        restart();
    } else {
        info!("Already on latest version.");
    }

    Ok(())
}
