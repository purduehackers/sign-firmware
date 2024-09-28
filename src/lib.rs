// pub mod eeprom;
// pub mod schema;
#![feature(atomic_bool_fetch_not)]

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_idf_svc::{
    hal::{
        gpio::{AnyOutputPin, Output, PinDriver},
        interrupt::free,
        task::watchdog::WatchdogSubscription,
    },
    sys::EspError,
};
use std::{
    net::TcpStream,
    sync::{atomic::AtomicBool, mpsc::Receiver},
};
use std::{
    os::fd::{AsRawFd, IntoRawFd},
    sync::mpsc::Sender,
};

pub struct Leds {
    pub buffer: [u8; 15],
    _sender: Sender<[u8; 15]>,
    b_safe: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Block {
    Center = 0,
    BottomLeft,
    BottomRight,
    Right,
    Top,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Color {
    Red = 0,
    Green,
    Blue,
}

impl Block {
    fn channel_for_color(&self, color: Color) -> usize {
        (*self as usize * 3) + color as usize
    }
}

const GAMMA_LUT: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4,
    4, 4, 5, 5, 5, 5, 6, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9, 10, 10, 11, 11, 11, 12, 12, 13, 13, 14,
    14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 23, 23, 24, 24, 25, 26, 26, 27,
    28, 28, 29, 30, 30, 31, 32, 32, 33, 34, 35, 35, 36, 37, 38, 38, 39, 40, 41, 42, 42, 43, 44, 45,
    46, 47, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
    68, 69, 70, 71, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 84, 85, 86, 87, 88, 89, 91, 92, 93, 94,
    95, 97, 98, 99, 100, 102, 103, 104, 105, 107, 108, 109, 111, 112, 113, 115, 116, 117, 119, 120,
    121, 123, 124, 126, 127, 128, 130, 131, 133, 134, 136, 137, 139, 140, 142, 143, 145, 146, 148,
    149, 151, 152, 154, 155, 157, 158, 160, 162, 163, 165, 166, 168, 170, 171, 173, 175, 176, 178,
    180, 181, 183, 185, 186, 188, 190, 192, 193, 195, 197, 199, 200, 202, 204, 206, 207, 209, 211,
    213, 215, 217, 218, 220, 222, 224, 226, 228, 230, 232, 233, 235, 237, 239, 241, 243, 245, 247,
    249, 251, 253, 255,
];

static _PWM_CONTROL: Channel<CriticalSectionRawMutex, [u8; 15], 1> = Channel::new();

impl Leds {
    pub fn create(sender: Sender<[u8; 15]>) -> Leds {
        Leds {
            buffer: [0; 15],
            _sender: sender,
            b_safe: true,
        }
    }

    pub async fn set_color(&mut self, color: palette::Srgb<u8>, block: Block) {
        let r = block.channel_for_color(Color::Red);
        self.buffer[r] = GAMMA_LUT[color.red as usize];

        let g = block.channel_for_color(Color::Green);
        self.buffer[g] = GAMMA_LUT[color.green as usize];

        let b = block.channel_for_color(Color::Blue);
        self.buffer[b] = GAMMA_LUT[color.blue as usize];
    }

    pub async fn swap(&mut self) {
        // self.sender.send(self.buffer).unwrap();
        unsafe {
            if self.b_safe {
                A_BUF = self.buffer;
            } else {
                B_BUF = self.buffer;
            }

            self.b_safe = !FLAG.fetch_not(std::sync::atomic::Ordering::Relaxed);
        }
        // PWM_CONTROL.send(self.buffer).await;
    }
}

pub static FLAG: AtomicBool = AtomicBool::new(true);
pub static mut A_BUF: [u8; 15] = [0; 15];
pub static mut B_BUF: [u8; 15] = [0; 15];

// #[embassy_executor::task]
pub fn leds_software_pwm<'d>(
    mut led_pins: [PinDriver<'static, AnyOutputPin, Output>; 15],
    mut wd: WatchdogSubscription<'d>,
    _rx: Receiver<[u8; 15]>,
) {
    let mut timer_value: u8 = 0;
    let mut last_buffer = [0_u8; 15];

    // Update at 120hz
    // let mut ticker = Ticker::every(Duration::from_hz(256 * 200));

    // let pwm_receiver = PWM_CONTROL.receiver();

    loop {
        // if last_buffer[0] == 0 {
        //     last_buffer = pwm_receiver.try_receive().unwrap_or(last_buffer);
        // }
        // if let Ok(buf) = rx.try_recv() {
        //     last_buffer = buf;
        // }

        // last_buffer = rx.try_recv().unwrap_or(last_buffer);
        free(|| {
            last_buffer = unsafe {
                if FLAG.fetch_and(true, std::sync::atomic::Ordering::Relaxed) {
                    B_BUF
                } else {
                    A_BUF
                }
            };
            // let guard = unsafe { critical_section::acquire() };

            for n in 0..15 {
                if timer_value == 0 {
                    led_pins[n].set_high().unwrap();
                }

                if last_buffer[n] == timer_value {
                    led_pins[n].set_low().unwrap();
                }
            }

            timer_value += 1;
        });
        wd.feed().unwrap();
        // unsafe { critical_section::release(guard) };

        // ticker.next().await;
    }
}

pub struct EspTlsSocket(Option<async_io::Async<TcpStream>>);

impl EspTlsSocket {
    pub const fn new(socket: async_io::Async<TcpStream>) -> Self {
        Self(Some(socket))
    }

    pub fn handle(&self) -> i32 {
        self.0.as_ref().unwrap().as_raw_fd()
    }

    pub fn poll_readable(
        &self,
        ctx: &mut core::task::Context,
    ) -> core::task::Poll<Result<(), esp_idf_svc::sys::EspError>> {
        self.0
            .as_ref()
            .unwrap()
            .poll_readable(ctx)
            .map_err(|_| EspError::from_infallible::<{ esp_idf_svc::sys::ESP_FAIL }>())
    }

    pub fn poll_writeable(
        &self,
        ctx: &mut core::task::Context,
    ) -> core::task::Poll<Result<(), esp_idf_svc::sys::EspError>> {
        self.0
            .as_ref()
            .unwrap()
            .poll_writable(ctx)
            .map_err(|_| EspError::from_infallible::<{ esp_idf_svc::sys::ESP_FAIL }>())
    }

    fn release(&mut self) -> Result<(), esp_idf_svc::sys::EspError> {
        let socket = self.0.take().unwrap();
        socket.into_inner().unwrap().into_raw_fd();

        Ok(())
    }
}

impl esp_idf_svc::tls::Socket for EspTlsSocket {
    fn handle(&self) -> i32 {
        EspTlsSocket::handle(self)
    }

    fn release(&mut self) -> Result<(), esp_idf_svc::sys::EspError> {
        EspTlsSocket::release(self)
    }
}

impl esp_idf_svc::tls::PollableSocket for EspTlsSocket {
    fn poll_readable(
        &self,
        ctx: &mut core::task::Context,
    ) -> core::task::Poll<Result<(), esp_idf_svc::sys::EspError>> {
        EspTlsSocket::poll_readable(self, ctx)
    }

    fn poll_writable(
        &self,
        ctx: &mut core::task::Context,
    ) -> core::task::Poll<Result<(), esp_idf_svc::sys::EspError>> {
        EspTlsSocket::poll_writeable(self, ctx)
    }
}
