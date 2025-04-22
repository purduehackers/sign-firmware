#![feature(type_alias_impl_trait)]

use build_time::build_time_utc;
use chrono::{Datelike, Local, Timelike, Weekday};
use chrono_tz::US::Eastern;
use embassy_time::Timer;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        gpio::{Gpio15, Gpio36, Input, Output, PinDriver},
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
use lightning_time::{LightningTime, LightningTimeColors};
use log::info;
use palette::rgb::Rgb;
use sign_firmware::{
    net::{connect_to_network, self_update},
    Block, Leds,
};

extern crate alloc;

async fn amain(
    mut leds: Leds,
    mut wifi: AsyncWifi<EspWifi<'static>>,
    #[allow(unused_variables)] button_switch: PinDriver<'static, Gpio36, Input>,
    #[allow(unused_variables)]
    #[allow(unused_mut)]
    mut button_led: PinDriver<'static, Gpio15, Output>,
) {
    // Red before wifi
    leds.set_all_colors(Rgb::new(255, 0, 0));

    connect_to_network(&mut wifi)
        .await
        .expect("wifi connection");

    // Check for update
    self_update(&mut leds).await.expect("Self-update to work");

    #[cfg(feature = "interactive")]
    let mut interactive_state = interactive::InteractiveState {
        last_led_change: Local::now(),
        last_time: LightningTime::from(Local::now().with_timezone(&Eastern).time()),
        button_pressed: false,
    };
    loop {
        let time = LightningTime::from(Local::now().with_timezone(&Eastern).time());

        #[cfg(feature = "interactive")]
        interactive::interactive(
            &mut interactive_state,
            &mut button_led,
            &button_switch,
            time,
        )
        .await;

        set_colors(&time.colors(), &mut leds);

        // Weekly self-update check
        if Local::now().weekday() == Weekday::Sat
            && Local::now().hour() == 3
            && Local::now().minute() == 0
            && Local::now().second() == 0
        {
            self_update(&mut leds).await.expect("Self-update to work");
        }

        Timer::after_millis(10).await;
    }
}

#[cfg(feature = "interactive")]
mod interactive {
    use super::*;
    use chrono::Duration;
    use esp_idf_svc::hal::gpio::Level;
    use sign_firmware::printer;

    pub async fn interactive(
        InteractiveState {
            last_led_change,
            last_time,
            button_pressed,
        }: &mut InteractiveState,
        button_led: &mut PinDriver<'static, Gpio15, Output>,
        button_switch: &PinDriver<'static, Gpio36, Input>,
        time: LightningTime,
    ) {
        fn midnight(time: &LightningTime) -> bool {
            time.bolts == 0 && time.zaps == 0 && time.sparks == 0 && time.charges == 0
        }

        if midnight(&time) && !midnight(last_time) {
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

        match (button_switch.get_level(), *button_pressed) {
            (Level::High, false) => {
                if let Err(e) = printer::post_event(printer::PrinterEvent::ButtonPressed).await {
                    log::error!("BUTTON PRESSED: Printer error: {e}");
                }
                *button_pressed = true;
            }
            (Level::Low, true) => {
                *button_pressed = false;
            }
            _ => {}
        }

        if Local::now() - *last_led_change > Duration::seconds(1) {
            button_led.toggle().unwrap();
            *last_led_change = Local::now();
        }

        *last_time = time;
    }

    pub struct InteractiveState {
        pub last_led_change: chrono::DateTime<Local>,
        pub last_time: LightningTime,
        pub button_pressed: bool,
    }
}

fn set_colors(colors: &LightningTimeColors, leds: &mut Leds) {
    leds.set_color(colors.bolt, Block::BottomLeft);

    for block in [Block::Top, Block::Center] {
        leds.set_color(colors.zap, block);
    }

    for block in [Block::Right, Block::BottomRight] {
        leds.set_color(colors.spark, block);
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

    // The buttonled and button switch pins are reversed from the original board schematic since pin 36 is input only (oops)
    let button_led = PinDriver::output(peripherals.pins.gpio15).unwrap();
    let button_switch = PinDriver::input(peripherals.pins.gpio36).unwrap();

    let leds = Leds::create(leds);

    std::thread::Builder::new()
        .stack_size(60_000)
        .spawn(|| {
            io::vfs::initialize_eventfd(5).unwrap();
            block_on(amain(leds, wifi, button_switch, button_led))
        })
        .unwrap()
        .join()
        .unwrap();
}
