use embassy_time::Timer;
use esp_idf_svc::{
    hal::{
        delay::FreeRtos,
        prelude::Peripherals,
        task::{self, block_on},
    },
    wifi::{
        AsyncWifi, AuthMethod, ClientConfiguration, Configuration, EspWifi, PmfConfiguration,
        ScanMethod,
    },
};
use sign_firmware::{Block, Leds};

#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_username: &'static str,
    #[default("")]
    wifi_password: &'static str,
}

fn main() {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    block_on(amain());
}

async fn amain() {
    let Ok(mut leds) = Leds::create() else {
        log::error!("LEDs are fucked up, goodbye.");

        panic!()
    };

    log::info!("Hello, world!");

    // leds.set_color(palette::Srgb::new(255, 255, 255), Block::Center);

    loop {
        Timer::after_millis(100).await
    }
}

async fn connect_to_wifi(
    wifi: &mut AsyncWifi<EspWifi<'static>>,
    cfg: &Config,
) -> anyhow::Result<()> {
    // esp_idf_svc::sys::esp_eap_client_set_identity();
    Ok(())
}
