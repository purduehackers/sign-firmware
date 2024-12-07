#![feature(type_alias_impl_trait)]

use build_time::build_time_utc;
use chrono_tz::US::Eastern;
use embassy_time::Timer;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        gpio::{Level, PinDriver},
        ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
        peripherals::Peripherals,
        task::block_on,
    },
    io,
    nvs::EspDefaultNvsPartition,
    ota::EspOta,
    sntp,
    timer::EspTaskTimerService,
    wifi::{AsyncWifi, EspWifi},
};
use lightning_time::LightningTime;
use log::info;
use palette::rgb::Rgb;
use sign_firmware::{
    net::{connect_to_network, self_update},
    printer, Block, Leds,
};

extern crate alloc;

fn midnight(time: &LightningTime) -> bool {
    time.bolts == 0 && time.zaps == 0 && time.sparks == 0 && time.charges == 0
}

async fn amain(mut leds: Leds, mut wifi: AsyncWifi<EspWifi<'static>>) {
    // Red before wifi
    leds.set_all_colors(Rgb::new(255, 0, 0));

    connect_to_network(&mut wifi)
        .await
        .expect("wifi connection");

    // Blue before update
    leds.set_all_colors(Rgb::new(0, 0, 255));

    // Check for update
    self_update(&mut leds).await.expect("Self-update to work");

    let peripherals = Peripherals::take().unwrap();

    // The buttonled and button switch pins are reversed from the original board schematic since pin 36 is input only (oops)
    let _button_led = PinDriver::output(peripherals.pins.gpio15).unwrap();
    let button_switch = PinDriver::input(peripherals.pins.gpio36).unwrap();

    let mut last_time =
        LightningTime::from(chrono::offset::Local::now().with_timezone(&Eastern).time());
    loop {
        let time = LightningTime::from(chrono::offset::Local::now().with_timezone(&Eastern).time());

        if midnight(&time) && !midnight(&last_time) {
            if let Err(e) = printer::post_event(printer::PrinterEvent::Zero).await {
                log::error!("ZERO: Printer error: {e}");
            }
        } else if time.bolts != last_time.bolts {
            if let Err(e) = printer::post_event(printer::PrinterEvent::NewBolt(time.bolts)).await {
                log::error!("BOLT: Printer error: {e}");
            }
        } else if time.zaps != last_time.zaps {
            if let Err(e) = printer::post_event(printer::PrinterEvent::NewZap(time.zaps)).await {
                log::error!("ZAP: Printer error: {e}");
            }
        }
        if matches!(button_switch.get_level(), Level::High) {
            if let Err(e) = printer::post_event(printer::PrinterEvent::ButtonPressed).await {
                log::error!("BUTTON PRESSED: Printer error: {e}");
            }
        }

        last_time = time;

        let colors = time.colors();

        leds.set_color(colors.bolt, Block::BottomLeft);

        for block in [Block::Top, Block::Center] {
            leds.set_color(colors.zap, block);
        }

        for block in [Block::Right, Block::BottomRight] {
            leds.set_color(colors.spark, block);
        }

        Timer::after_millis(1000).await;
    }
}

fn main() {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    info!(
        "Purdue Hackers Sign Firmware v.{}.{}.{} (Built {})",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
        build_time_utc!()
    );

    EspOta::new()
        .expect("ESP OTA")
        .mark_running_slot_valid()
        .expect("running slot valid");

    let peripherals = Peripherals::take().unwrap();

    let sys_loop = EspSystemEventLoop::take().unwrap();
    let nvs = EspDefaultNvsPartition::take().unwrap();

    let wifi = AsyncWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs)).unwrap(),
        sys_loop,
        EspTaskTimerService::new().unwrap(),
    )
    .expect("wifi init");

    info!("Wi-Fi initialized");

    let _sntp = sntp::EspSntp::new_default().unwrap();

    info!("SNTP initialized");

    let low_driver = LedcTimerDriver::new(
        peripherals.ledc.timer0,
        &TimerConfig::default().resolution(esp_idf_svc::hal::ledc::Resolution::Bits8),
    )
    .expect("timer driver");

    let high_driver = LedcTimerDriver::new(
        peripherals.hledc.timer0,
        &TimerConfig::default().resolution(esp_idf_svc::hal::ledc::Resolution::Bits8),
    )
    .expect("high speed timer driver");

    let leds = [
        LedcDriver::new(
            peripherals.ledc.channel0,
            &low_driver,
            peripherals.pins.gpio23,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel1,
            &low_driver,
            peripherals.pins.gpio22,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel2,
            &low_driver,
            peripherals.pins.gpio21,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel3,
            &low_driver,
            peripherals.pins.gpio19,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel4,
            &low_driver,
            peripherals.pins.gpio18,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel5,
            &low_driver,
            peripherals.pins.gpio5,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel6,
            &low_driver,
            peripherals.pins.gpio17,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.ledc.channel7,
            &low_driver,
            peripherals.pins.gpio16,
        )
        .unwrap(),
        // High speed
        LedcDriver::new(
            peripherals.hledc.channel0,
            &high_driver,
            peripherals.pins.gpio4,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.hledc.channel1,
            &high_driver,
            peripherals.pins.gpio33,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.hledc.channel2,
            &high_driver,
            peripherals.pins.gpio25,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.hledc.channel3,
            &high_driver,
            peripherals.pins.gpio26,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.hledc.channel4,
            &high_driver,
            peripherals.pins.gpio12,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.hledc.channel5,
            &high_driver,
            peripherals.pins.gpio14,
        )
        .unwrap(),
        LedcDriver::new(
            peripherals.hledc.channel6,
            &high_driver,
            peripherals.pins.gpio27,
        )
        .unwrap(),
    ];

    let leds = Leds::create(leds);

    std::thread::Builder::new()
        .stack_size(60_000)
        .spawn(|| {
            io::vfs::initialize_eventfd(5).unwrap();
            block_on(amain(leds, wifi))
        })
        .unwrap()
        .join()
        .unwrap();
}
