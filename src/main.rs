#![feature(type_alias_impl_trait)]

use anyhow::anyhow;
use async_io::Async;
use build_time::build_time_utc;
use chrono_tz::US::Eastern;
use dotenvy_macro::dotenv;
use embassy_time::{with_timeout, Timer};
use std::{net::TcpStream, sync::mpsc::channel, time::Duration};
// use esp_backtrace as _;
// use esp_hal_embassy::{Executor, InterruptExecutor};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        cpu::Core,
        gpio::{OutputPin, PinDriver},
        peripherals::Peripherals,
        reset::restart,
        task::{
            block_on,
            thread::ThreadSpawnConfiguration,
            watchdog::{TWDTConfig, TWDTDriver},
        },
    },
    io::{self, asynch::Read, Write},
    nvs::EspDefaultNvsPartition,
    ota::EspOta,
    sntp,
    sys::EspError,
    timer::EspTaskTimerService,
    tls::EspAsyncTls,
    wifi::{AsyncWifi, ClientConfiguration, Configuration, EspWifi},
};
use http::Request;
// use esp_wifi::wifi::{
//     ClientConfiguration, Configuration, EapClientConfiguration, TtlsPhase2Method, WifiController,
//     WifiDevice, WifiEvent, WifiStaDevice, WifiState,
// };
use lightning_time::LightningTime;
use log::{debug, info};
use sign_firmware::{leds_software_pwm, Block, EspTlsSocket, Leds};
use url::Url;

extern crate alloc;
use core::str::FromStr;
use std::net::ToSocketAddrs;

macro_rules! anyesp {
    ($err: expr) => {{
        let res = $err;
        if res != ::esp_idf_svc::sys::ESP_OK {
            Err(::anyhow::anyhow!("Bad exit code {res}"))
        } else {
            Ok(())
        }
    }};
}

#[derive(Debug, serde::Deserialize)]
struct GithubResponse {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct GithubAsset {
    browser_download_url: String,
}

async fn generate_tls(url: &str) -> anyhow::Result<EspAsyncTls<EspTlsSocket>> {
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

fn create_raw_request<T>(request: http::Request<T>) -> String {
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

async fn handle_redirect(url: &str) -> anyhow::Result<EspAsyncTls<EspTlsSocket>> {
    let request = Request::builder()
        .method("GET")
        .header("User-Agent", "PHSign/1.0.0")
        .header("Host", "github.com")
        .uri(url)
        .body(())
        .unwrap();

    let mut tls = generate_tls(url).await?;

    let request_text = create_raw_request(request);

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

            let mut tls = generate_tls(location).await?;
            let request_text = create_raw_request(request);

            tls.write_all(request_text.as_bytes())
                .await
                .map_err(convert_error)?;

            return Ok(tls);
        }
    }

    unreachable!("location must be in returned value!")
}

async fn self_update() -> anyhow::Result<()> {
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

        let request_text = create_raw_request(request);

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

        serde_json::from_str(&body[ind + 4..].trim().trim_end_matches(char::from(0)))
            .expect("Valid parse for GitHub manifest")
    };

    let local = semver::Version::new(
        env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap(),
        env!("CARGO_PKG_VERSION_MINOR").parse().unwrap(),
        env!("CARGO_PKG_VERSION_PATCH").parse().unwrap(),
    );

    let remote = semver::Version::from_str(&manifest.tag_name[1..]).expect("valid semver");

    if true {
        info!("New release found! Downloading and updating");
        // Grab new release and update
        let url = manifest
            .assets
            .first()
            .expect("release to contain assets")
            .browser_download_url
            .clone();

        let mut tls = handle_redirect(&url).await?;

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
                // {
                //     let read = consumption_buffer[0] as char;
                //     if read.is_ascii() {
                //         info!("{state:?}: {read}");
                //     } else {
                //         info!("{state:?}: INVALID ASCII");
                //     }
                // }
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
    }

    Ok(())
}

// #[embassy_executor::task]
async fn amain(mut leds: Leds, mut wifi: AsyncWifi<EspWifi<'static>>) {
    // let tls = TlsConfig::new(
    //     const_random::const_random!(u64),
    //     &mut read_buffer,
    //     &mut write_buffer,
    //     TlsVerify::None,
    // );
    // let mut client = HttpClient::new_with_tls(&tcp, &dns, tls);

    // ThreadSpawnConfiguration {
    //     name: None,
    //     stack_size: 60_000,
    //     priority: 24,
    //     inherit: false,
    //     pin_to_core: Some(Core::Core1),
    // }
    // .set()
    // .unwrap();

    // let mut client = Client::wrap(&mut EspHttpConnection::new(&Default::default()).unwrap());
    connect_to_network(&mut wifi)
        .await
        .expect("wifi connection");

    // Check for update
    self_update().await.expect("Self-update to work");

    loop {
        let colors =
            LightningTime::from(chrono::offset::Local::now().with_timezone(&Eastern).time())
                .colors();

        leds.set_color(colors.bolt, Block::BottomLeft).await;

        for block in [Block::Top, Block::Center] {
            leds.set_color(colors.zap, block).await;
        }

        for block in [Block::Right, Block::BottomRight] {
            leds.set_color(colors.spark, block).await;
        }

        leds.swap().await;

        Timer::after_millis(1000).await;
    }
}

// static mut APP_CORE_STACK: Stack<8192> = Stack::new();

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

    // let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    // let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);

    // let timg0 = TimerGroup::new(peripherals.TIMG1);

    // let init = esp_wifi::initialize(
    //     esp_wifi::EspWifiInitFor::Wifi,
    //     timg0.timer1,
    //     esp_hal::rng::Rng::new(peripherals.RNG),
    //     peripherals.RADIO_CLK,
    // )
    // .unwrap();

    // let (wifi_interface, controller) =
    //     esp_wifi::wifi::new_with_mode(&init, peripherals.WIFI, WifiStaDevice).unwrap();

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

    // let leds = [
    //     PinDriver::output(peripherals.pins.gpio1.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio2.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio4.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio5.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio6.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio7.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio8.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio9.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio10.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio11.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio12.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio13.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio14.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio17.downgrade_output()).unwrap(),
    //     PinDriver::output(peripherals.pins.gpio18.downgrade_output()).unwrap(),
    // ];

    // static EXECUTOR_CORE_1: StaticCell<InterruptExecutor<1>> = StaticCell::new();
    // let executor_core1 = InterruptExecutor::new(sw_ints.software_interrupt1);
    // let executor_core1 = EXECUTOR_CORE_1.init(executor_core1);

    // let _guard = cpu_control
    //     .start_app_core(unsafe { &mut *addr_of_mut!(APP_CORE_STACK) }, move || {
    //         let spawner = executor_core1.start(Priority::max());

    //         spawner.spawn(leds_software_pwm(leds)).ok();

    //         // Just loop to show that the main thread does not need to poll the executor.
    //         loop {}
    //     })
    //     .unwrap();

    // unsafe {
    //     esp_idf_svc::hal::task::create(
    //         task_handler,
    //         task_name,
    //         stack_size,
    //         task_arg,
    //         priority,
    //         pin_to_core,
    //     )
    //     .unwrap();
    // }

    let (tx, rx) = channel();

    // let config = TWDTConfig {
    //     duration: Duration::from_secs(2),
    //     panic_on_trigger: true,
    //     subscribed_idle_tasks: Core::Core0.into(),
    // };
    // let mut driver = TWDTDriver::new(peripherals.twdt, &config).unwrap();

    // ThreadSpawnConfiguration {
    //     name: None,
    //     stack_size: 8000,
    //     priority: 24,
    //     inherit: false,
    //     pin_to_core: Some(Core::Core1),
    // }
    // .set()
    // .unwrap();
    // std::thread::spawn(move || {
    //     let watchdog = driver.watch_current_task().unwrap();
    //     // let mut leds = leds;
    //     // let mut last_buffer = [0; 15];
    //     // let timer = EspTimerService::new()
    //     //     .unwrap()
    //     //     .timer(move || {
    //     //         leds_software_pwm_timer(&mut leds, last_buffer);
    //     //     })
    //     //     .unwrap();

    //     // timer
    //     //     .every(Duration::from_secs_f64(1.0 / (256.0 * 120.0)))
    //     //     .unwrap();

    //     // loop {
    //     //     last_buffer = rx.try_recv().unwrap_or(last_buffer);
    //     //     watchdog.feed().expect("watchdog ok");
    //     // }
    //     // block_on(leds_software_pwm(leds));
    //     leds_software_pwm(leds, watchdog, rx);
    // });

    let leds = Leds::create(tx);

    // static EXECUTOR_CORE_0: StaticCell<Executor> = StaticCell::new();
    // let executor_core0 = Executor::new();
    // let executor_core0 = EXECUTOR_CORE_0.init(executor_core0);
    // executor_core0.run(|spawner| {
    //     // spawner.spawn(connection(controller)).ok();
    //     // spawner.spawn(net_task(stack)).ok();
    //     spawner.spawn(amain(leds)).ok();
    // });

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

// fn to_anyhow<T>(result: Result<T, EspError>) -> anyhow::Result<T> {
//     match result {
//         Ok(t) => Ok(t),
//         Err(e) => Err(convert_error(e)),
//     }
// }

fn convert_error(e: EspError) -> anyhow::Error {
    anyhow!("Bad exit code {e}")
}

async fn connect_to_network(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> {
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
            dotenv!("WIFI_USERNAME").as_ptr(),
            dotenv!("WIFI_USERNAME").len() as i32
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

    wifi.connect().await.map_err(convert_error)?;

    wifi.wait_netif_up().await.map_err(convert_error)?;

    info!("Wi-Fi connected!");

    Ok(())
}

// #[embassy_executor::task]
// async fn connection(mut controller: WifiController<'static>) {
//     info!("start connection task");
//     debug!("Device capabilities: {:?}", controller.get_capabilities());
//     loop {
//         if matches!(esp_wifi::wifi::get_wifi_state(), WifiState::StaConnected) {
//             // wait until we're no longer connected
//             controller.wait_for_event(WifiEvent::StaDisconnected).await;
//             Timer::after_millis(5000).await
//         }
//         if !matches!(controller.is_started(), Ok(true)) {
//             // Assume we don't need any certs
//             let client_config = Configuration::EapClient(EapClientConfiguration {
//                 ssid: heapless::String::from_str(dotenv!("WIFI_SSID")).unwrap(),
//                 auth_method: esp_wifi::wifi::AuthMethod::WPA2Enterprise,
//                 identity: Some(heapless::String::from_str(dotenv!("WIFI_USERNAME")).unwrap()),
//                 username: Some(heapless::String::from_str(dotenv!("WIFI_USERNAME")).unwrap()),
//                 password: Some(heapless::String::from_str(dotenv!("WIFI_PASSWORD")).unwrap()),
//                 ttls_phase2_method: Some(TtlsPhase2Method::Mschapv2),
//                 ..Default::default()
//             });
//             controller.set_configuration(&client_config).unwrap();
//             info!("Starting wifi");
//             controller.start().await.unwrap();
//             info!("Wifi started!");
//         }
//         info!("About to connect...");

//         match controller.connect().await {
//             Ok(_) => info!("Wifi connected!"),
//             Err(e) => {
//                 error!("Failed to connect to wifi: {e:?}");
//                 Timer::after_millis(5000).await
//             }
//         }
//     }
// }
