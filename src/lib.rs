#![no_std]

use core::hint::unreachable_unchecked;

use esp_idf_svc::hal::ledc::LedcDriver;

// use embassy_rp::{
//     peripherals::{
//         PWM_SLICE0, PWM_SLICE1, PWM_SLICE2, PWM_SLICE3, PWM_SLICE4, PWM_SLICE5, PWM_SLICE6,
//         PWM_SLICE7,
//     },
//     pwm::{Config, Pwm, Slice},
// };

pub mod eeprom;
pub mod schema;

pub struct ConfiguredPwm<'a, S: Slice> {
    pub config: Config,
    pub pwm: Pwm<'a, S>,
}

impl<'a, S: Slice> From<Pwm<'a, S>> for ConfiguredPwm<'a, S> {
    fn from(value: Pwm<'a, S>) -> Self {
        Self {
            config: Config::default(),
            pwm: value,
        }
    }
}

pub struct Leds<'a> {
    pub s0: LedcDriver<'a>,
    pub s1: LedcDriver<'a>,
    pub s2: LedcDriver<'a>,
    pub s3: LedcDriver<'a>,
    pub s4: LedcDriver<'a>,
    pub s5: LedcDriver<'a>,
    pub s6: LedcDriver<'a>,
    pub s7: LedcDriver<'a>,
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
enum PwmSubchannel {
    A,
    B,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Color {
    Red = 0,
    Green,
    Blue,
}

impl Block {
    fn channel_for_color(&self, color: Color) -> (u8, PwmSubchannel) {
        let sum = (*self as u8 * 3) + color as u8;
        let channel = sum / 2;
        let subchannel = sum % 2;
        (
            channel,
            match subchannel {
                0 => PwmSubchannel::A,
                1 => PwmSubchannel::B,
                _ => unsafe { unreachable_unchecked() },
            },
        )
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

impl Leds<'_> {
    pub fn set_color(&mut self, color: palette::Srgb<u8>, block: Block) {
        let (r_c, r_sc) = block.channel_for_color(Color::Red);
        self.set_color_channel(color.red, r_c, r_sc);
        let (g_c, g_sc) = block.channel_for_color(Color::Green);
        self.set_color_channel(color.green, g_c, g_sc);
        let (b_c, b_sc) = block.channel_for_color(Color::Blue);
        self.set_color_channel(color.blue, b_c, b_sc);
    }

    fn set_color_channel(&mut self, channel_val: u8, pwm_channel: u8, subchannel: PwmSubchannel) {
        let config = match pwm_channel {
            0 => &mut self.s0.config,
            1 => &mut self.s1.config,
            2 => &mut self.s2.config,
            3 => &mut self.s3.config,
            4 => &mut self.s4.config,
            5 => &mut self.s5.config,
            6 => &mut self.s6.config,
            7 => &mut self.s7.config,
            _ => unsafe { unreachable_unchecked() },
        };
        let channel_val: u16 = (GAMMA_LUT[channel_val as usize]) as u16 * 257;
        match subchannel {
            PwmSubchannel::A => {
                config.compare_a = channel_val;
            }
            PwmSubchannel::B => {
                config.compare_b = channel_val;
            }
        }

        match pwm_channel {
            0 => self.s0.pwm.set_config(config),
            1 => self.s1.pwm.set_config(config),
            2 => self.s2.pwm.set_config(config),
            3 => self.s3.pwm.set_config(config),
            4 => self.s4.pwm.set_config(config),
            5 => self.s5.pwm.set_config(config),
            6 => self.s6.pwm.set_config(config),
            7 => self.s7.pwm.set_config(config),
            _ => unsafe { unreachable_unchecked() },
        }
    }
}
