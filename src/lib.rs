#![no_std]
#![feature(const_mut_refs)]

use core::{
    mem::MaybeUninit,
    num::NonZeroU32,
    ops::{Deref, DerefMut},
};

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_idf_svc::{
    hal::{
        delay,
        gpio::{AnyOutputPin, Output, OutputPin, PinDriver},
        prelude::Peripherals,
        task::notification::Notification,
        timer::{config, TimerDriver},
    },
    sys::EspError,
};

pub mod eeprom;
pub mod schema;

pub struct Leds {
    pub buffer: [u8; 15],
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

static mut LED_DATA_BUFFER: [u8; 15] = [
    0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8,
];

static mut LED_PINS: [PinDriver<'_, AnyOutputPin, Output>; 15] =
    unsafe { MaybeUninit::zeroed().assume_init() };

static mut TIMER_VALUE: u8 = 0;

impl Leds {
    pub fn create() -> Result<Leds, EspError> {
        let peripherals = Peripherals::take()?;

        let timer_conf = config::Config::new().auto_reload(true);
        let mut timer = TimerDriver::new(peripherals.timer00, &timer_conf)?;

        unsafe {
            LED_PINS = [
                PinDriver::output(peripherals.pins.gpio1.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio2.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio4.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio5.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio6.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio7.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio8.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio9.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio10.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio11.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio12.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio13.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio14.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio15.downgrade_output())?,
                PinDriver::output(peripherals.pins.gpio16.downgrade_output())?,
            ];

            timer.subscribe(move || {
                log::info!("LED timer triggered, let's rock and roll");

                Self::set_leds(TIMER_VALUE);

                unsafe {
                    TIMER_VALUE += 1;
                }

                log::info!("Done updating LEDs");
            })?;
        }

        timer.enable_interrupt()?;

        timer.enable_alarm(true)?;
        // Update Leds at 120hz
        timer.set_alarm(timer.tick_hz() / (256 * 120))?;

        timer.enable(true)?;

        Ok(Leds {
            buffer: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        })
    }

    pub async fn set_color(&mut self, color: palette::Srgb<u8>, block: Block) {
        let r = block.channel_for_color(Color::Red);
        self.buffer[r] = GAMMA_LUT[color.red as usize];

        let g = block.channel_for_color(Color::Green);
        self.buffer[g] = GAMMA_LUT[color.green as usize];

        let b = block.channel_for_color(Color::Blue);
        self.buffer[b] = GAMMA_LUT[color.blue as usize];

        unsafe {
            LED_DATA_BUFFER = self.buffer.clone();
        }
    }

    pub fn set_leds(timer_value: u8) {
        for n in 0..15 {
            if timer_value == 0 {
                unsafe {
                    let _ = LED_PINS[n].set_high();
                };
            } else if unsafe { LED_DATA_BUFFER[n] } >= timer_value {
                unsafe {
                    let _ = LED_PINS[n].set_low();
                };
            }
        }
    }
}
