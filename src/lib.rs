// pub mod eeprom;
// pub mod schema;
pub mod net;
#[cfg(feature = "interactive")]
pub mod printer;

use anyhow::anyhow;
use esp_idf_svc::{hal::ledc::LedcDriver, sys::EspError};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, IntoRawFd};

#[macro_export]
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

pub fn convert_error(e: EspError) -> anyhow::Error {
    anyhow!("Bad exit code {e}")
}

pub struct Leds {
    channels: [LedcDriver<'static>; 15],
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

impl Leds {
    pub fn create(channels: [LedcDriver<'static>; 15]) -> Leds {
        Leds { channels }
    }

    pub fn set_color(&mut self, color: palette::Srgb<u8>, block: Block) {
        let r = block.channel_for_color(Color::Red);
        self.channels[r]
            .set_duty(GAMMA_LUT[color.red as usize] as u32)
            .unwrap();

        let g = block.channel_for_color(Color::Green);
        self.channels[g]
            .set_duty(GAMMA_LUT[color.green as usize] as u32)
            .unwrap();

        let b = block.channel_for_color(Color::Blue);
        self.channels[b]
            .set_duty(GAMMA_LUT[color.blue as usize] as u32)
            .unwrap();
    }

    pub fn set_all_colors(&mut self, color: palette::Srgb<u8>) {
        for block in [
            Block::BottomLeft,
            Block::BottomRight,
            Block::Center,
            Block::Right,
            Block::Top,
        ] {
            self.set_color(color, block);
        }
    }
}

/// Allows for an async version of the TLS socket
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
        let _ = socket.into_inner().unwrap().into_raw_fd();

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
