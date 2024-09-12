#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, NaiveTime};
use dotenvy_macro::dotenv;
use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    tcp::TcpSocket,
    Ipv4Address, Stack as NetStack, StackResources,
};
use embassy_time::Timer;
use esp_backtrace as _;
use esp_hal::{
    cpu_control::{CpuControl, Stack},
    gpio::{AnyPin, Io, Level, Output},
    interrupt::software::SoftwareInterruptControl,
    interrupt::Priority,
    peripherals::Peripherals,
    prelude::*,
    rtc_cntl::Rtc,
    timer::timg::TimerGroup,
};
use esp_hal_embassy::{Executor, InterruptExecutor};
use esp_wifi::wifi::{
    Configuration, EapClientConfiguration, TtlsPhase2Method, WifiController, WifiDevice, WifiEvent,
    WifiStaDevice, WifiState,
};
use lightning_time::LightningTime;
use log::{debug, error, info};
use reqwless::{
    client::{HttpClient, TlsConfig, TlsVerify},
    request::Method,
};
use sign_firmware::Block;
use sign_firmware::{leds_software_pwm, Leds};
use static_cell::StaticCell;

extern crate alloc;
use core::{ptr::addr_of_mut, str::FromStr};

#[embassy_executor::task]
async fn amain(
    mut leds: Leds,
    rtc: Rtc<'static>,
    stack: &'static NetStack<WifiDevice<'static, WifiStaDevice>>,
) {
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after_millis(500).await;
    }

    info!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            info!("Got IP: {}", config.address);
            break;
        }
        Timer::after_millis(500).await;
    }

    let state: TcpClientState<1, 2048, 2048> = TcpClientState::new();
    let tcp = TcpClient::new(stack, &state);
    let dns = DnsSocket::new(stack);
    let mut read_buffer = [0; 20_000];
    let mut write_buffer = [0; 20_000];
    let tls = TlsConfig::new(
        const_random::const_random!(u64),
        &mut read_buffer,
        &mut write_buffer,
        TlsVerify::None,
    );
    let mut client = HttpClient::new_with_tls(&tcp, &dns, tls);

    let time = {
        let mut req = client
            .request(
                Method::GET,
                "https://worldtimeapi.org/api/timezone/America/New_York",
            )
            .await
            .expect("request to be created");

        let mut headers = [0; 2048];
        let response = req.send(&mut headers).await.expect("request to succeed");

        let body = response.body().read_to_end().await.expect("body to read");

        #[derive(Debug, serde::Deserialize)]
        struct TimeResponse {
            unixtime: i64,
        }

        let time: TimeResponse = serde_json::from_slice(body).expect("parse success");
        DateTime::from_timestamp(time.unixtime, 0)
            .expect("valid time")
            .with_timezone(&FixedOffset::west_opt(4 * 3600).unwrap())
            .naive_local()
    };

    rtc.set_current_time(time);

    loop {
        let colors = LightningTime::from(rtc.current_time().time()).colors();

        leds.set_color(colors.bolt, Block::BottomLeft).await;

        for block in [Block::Top, Block::Center] {
            leds.set_color(colors.zap, block).await;
        }

        for block in [Block::Right, Block::BottomRight] {
            leds.set_color(colors.spark, block).await;
        }

        leds.swap().await;

        Timer::after_millis(100).await;
    }
}

static mut APP_CORE_STACK: Stack<8192> = Stack::new();
static STACK: StaticCell<embassy_net::Stack<WifiDevice<'_, WifiStaDevice>>> = StaticCell::new();
static RESOURCES: StaticCell<embassy_net::StackResources<3>> = StaticCell::new();

#[entry]
fn main() -> ! {
    let peripherals = unsafe { Peripherals::steal() };

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);

    let timg0 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timg0.timer0);

    let init = esp_wifi::initialize(
        esp_wifi::EspWifiInitFor::Wifi,
        timg0.timer1,
        esp_hal::rng::Rng::new(peripherals.RNG),
        peripherals.RADIO_CLK,
    )
    .unwrap();

    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&init, peripherals.WIFI, WifiStaDevice).unwrap();

    let config = embassy_net::Config::dhcpv4(Default::default());

    let stack = &*STACK.init(embassy_net::Stack::new(
        wifi_interface,
        config,
        RESOURCES.init(embassy_net::StackResources::new()),
        const_random::const_random!(u64),
    ));

    let mut cpu_control = CpuControl::new(peripherals.CPU_CTRL);

    let leds = [
        Output::new(io.pins.gpio1.degrade(), Level::Low),
        Output::new(io.pins.gpio2.degrade(), Level::Low),
        Output::new(io.pins.gpio4.degrade(), Level::Low),
        Output::new(io.pins.gpio5.degrade(), Level::Low),
        Output::new(io.pins.gpio6.degrade(), Level::Low),
        Output::new(io.pins.gpio7.degrade(), Level::Low),
        Output::new(io.pins.gpio8.degrade(), Level::Low),
        Output::new(io.pins.gpio9.degrade(), Level::Low),
        Output::new(io.pins.gpio10.degrade(), Level::Low),
        Output::new(io.pins.gpio11.degrade(), Level::Low),
        Output::new(io.pins.gpio12.degrade(), Level::Low),
        Output::new(io.pins.gpio13.degrade(), Level::Low),
        Output::new(io.pins.gpio14.degrade(), Level::Low),
        Output::new(io.pins.gpio17.degrade(), Level::Low),
        Output::new(io.pins.gpio18.degrade(), Level::Low),
    ];

    static EXECUTOR_CORE_1: StaticCell<InterruptExecutor<1>> = StaticCell::new();
    let executor_core1 = InterruptExecutor::new(sw_ints.software_interrupt1);
    let executor_core1 = EXECUTOR_CORE_1.init(executor_core1);

    let _guard = cpu_control
        .start_app_core(unsafe { &mut *addr_of_mut!(APP_CORE_STACK) }, move || {
            let spawner = executor_core1.start(Priority::max());

            spawner.spawn(leds_software_pwm(leds)).ok();

            // Just loop to show that the main thread does not need to poll the executor.
            loop {}
        })
        .unwrap();

    let leds = Leds::create();
    let rtc = Rtc::new(peripherals.LPWR);

    static EXECUTOR_CORE_0: StaticCell<Executor> = StaticCell::new();
    let executor_core0 = Executor::new();
    let executor_core0 = EXECUTOR_CORE_0.init(executor_core0);
    executor_core0.run(|spawner| {
        spawner.spawn(connection(controller)).ok();
        spawner.spawn(net_task(stack)).ok();
        spawner.spawn(amain(leds, rtc, stack)).ok();
    });
}

#[embassy_executor::task]
async fn net_task(stack: &'static NetStack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!("start connection task");
    debug!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        if matches!(esp_wifi::wifi::get_wifi_state(), WifiState::StaConnected) {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after_millis(5000).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            // Assume we don't need any certs
            let client_config = Configuration::EapClient(EapClientConfiguration {
                ssid: heapless::String::from_str(dotenv!("WIFI_SSID")).unwrap(),
                auth_method: esp_wifi::wifi::AuthMethod::WPA2Enterprise,
                identity: Some(heapless::String::from_str(dotenv!("WIFI_USERNAME")).unwrap()),
                username: Some(heapless::String::from_str(dotenv!("WIFI_USERNAME")).unwrap()),
                password: Some(heapless::String::from_str(dotenv!("WIFI_PASSWORD")).unwrap()),
                ttls_phase2_method: Some(TtlsPhase2Method::Mschapv2),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            info!("Starting wifi");
            controller.start().await.unwrap();
            info!("Wifi started!");
        }
        info!("About to connect...");

        match controller.connect().await {
            Ok(_) => info!("Wifi connected!"),
            Err(e) => {
                error!("Failed to connect to wifi: {e:?}");
                Timer::after_millis(5000).await
            }
        }
    }
}
