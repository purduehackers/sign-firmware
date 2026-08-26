use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::info;
use serde::{Deserialize, Serialize};

const NVS_NAMESPACE: &str = "sign_cfg";
const KEY_WIFI_NETWORKS: &str = "wifi_nets";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub password: String,
    #[serde(default = "default_network_type")]
    pub network_type: NetworkType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enterprise_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enterprise_username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkType {
    #[default]
    Personal,
    Enterprise,
    Wep,
}

fn default_network_type() -> NetworkType {
    NetworkType::Personal
}

pub struct DeviceConfig {
    nvs: EspNvs<NvsDefault>,
}

impl DeviceConfig {
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        let nvs = EspNvs::new(partition, NVS_NAMESPACE, true)?;
        info!("NVS namespace '{NVS_NAMESPACE}' opened");
        Ok(Self { nvs })
    }

    pub fn get_wifi_networks(&self) -> Vec<WifiNetwork> {
        let mut buf = [0u8; 2048];
        let blob = match self.nvs.get_blob(KEY_WIFI_NETWORKS, &mut buf) {
            Ok(Some(data)) => data.to_vec(),
            _ => return Vec::new(),
        };
        serde_json::from_slice(&blob).unwrap_or_default()
    }

    pub fn set_wifi_networks(&mut self, networks: &[WifiNetwork]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(networks)?;
        self.nvs.set_blob(KEY_WIFI_NETWORKS, &json)?;
        info!("Stored {} WiFi networks in NVS", networks.len());
        Ok(())
    }

    pub fn add_wifi_network(&mut self, network: &WifiNetwork) -> anyhow::Result<()> {
        let mut networks = self.get_wifi_networks();
        networks.push(network.clone());
        self.set_wifi_networks(&networks)
    }
}
