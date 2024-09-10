use bincode::{Decode, Encode};

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct Header {
    pub duration: f32,
    pub num_top_keys: u16,
    pub num_middle_keys: u16,
    pub num_right_keys: u16,
    pub num_bottom_left_keys: u16,
    pub num_bottom_right_keys: u16,
}

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct BezierPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct Color {
    pub next: u8,
    pub bezier_in: BezierPoint,
    pub bezier_out: BezierPoint,
}

#[derive(Debug, Encode, Decode, Clone, Copy)]
pub struct Key {
    pub time: f32,
    pub red: Option<Color>,
    pub green: Option<Color>,
    pub blue: Option<Color>,
}
