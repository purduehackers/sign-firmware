use esp_idf_svc::{
    hal::{prelude::Peripherals, task::block_on},
    wifi::{
        AsyncWifi, AuthMethod, ClientConfiguration, Configuration, EspWifi, PmfConfiguration,
        ScanMethod,
    },
};

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

    log::info!("Hello, world!");

    let p = Peripherals::take().unwrap();

    let pins = p.pins;
    block_on(amain());
}

async fn amain() {}

async fn connect_to_wifi(
    wifi: &mut AsyncWifi<EspWifi<'static>>,
    cfg: &Config,
) -> anyhow::Result<()> {
    // esp_idf_svc::sys::esp_eap_client_set_identity();
    Ok(())
}
