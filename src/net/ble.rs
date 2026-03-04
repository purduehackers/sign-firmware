use esp32_nimble::{uuid128, BLEAdvertisementData, BLEDevice, NimbleProperties};
use log::info;
use std::sync::mpsc;

use super::config::WifiNetwork;

const SERVICE_UUID: esp32_nimble::utilities::BleUuid =
    uuid128!("d3a37e64-7a4c-4c3f-b929-1a5c0e4f7e30");
const CHAR_UUID: esp32_nimble::utilities::BleUuid =
    uuid128!("d3a37e64-7a4c-4c3f-b929-1a5c0e4f7e31");

/// Start BLE advertising as "PH-Sign" and block until a phone/laptop writes
/// WiFi credentials (`{"ssid":"...","password":"..."}`) to the GATT characteristic.
pub fn ble_provision() -> anyhow::Result<WifiNetwork> {
    info!("Starting BLE provisioning...");

    let ble_device = BLEDevice::take();
    let server = ble_device.get_server();
    server.advertise_on_disconnect(false);

    let service = server.create_service(SERVICE_UUID);
    let characteristic = service.lock().create_characteristic(
        CHAR_UUID,
        NimbleProperties::WRITE | NimbleProperties::WRITE_NO_RSP,
    );

    let (tx, rx) = mpsc::sync_channel::<WifiNetwork>(1);

    characteristic.lock().on_write(move |args| {
        let data = args.recv_data();
        match serde_json::from_slice::<WifiNetwork>(data) {
            Ok(network) => {
                info!("BLE: received WiFi credentials for '{}'", network.ssid);
                let _ = tx.try_send(network);
            }
            Err(e) => {
                log::error!("BLE: invalid WiFi JSON: {e}");
                args.reject();
            }
        }
    });

    let mut adv_data = BLEAdvertisementData::new();
    adv_data.name("PH-Sign").add_service_uuid(SERVICE_UUID);

    let mut advertising = ble_device.get_advertising().lock();
    advertising
        .set_data(&mut adv_data)
        .map_err(|e| anyhow::anyhow!("BLE adv data: {e}"))?;
    advertising
        .start()
        .map_err(|e| anyhow::anyhow!("BLE adv start: {e}"))?;
    drop(advertising);

    info!("BLE advertising as 'PH-Sign', waiting for credentials...");

    let network = rx.recv().map_err(|e| anyhow::anyhow!("BLE channel recv: {e}"))?;

    // Clean up BLE
    ble_device
        .get_advertising()
        .lock()
        .stop()
        .map_err(|e| anyhow::anyhow!("BLE adv stop: {e}"))?;
    BLEDevice::deinit_full().map_err(|e| anyhow::anyhow!("BLE deinit: {e}"))?;

    info!("BLE provisioning complete");
    Ok(network)
}
