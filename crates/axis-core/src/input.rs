use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    pub fn scale(self, k: f32) -> Vec2 {
        Vec2 {
            x: self.x * k,
            y: self.y * k,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Button {
    South,
    East,
    West,
    North,
    L1,
    R1,
    L3,
    R3,
    Start,
    Select,
    Guide,
    Touchpad,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl Button {
    pub const COUNT: usize = 16;
    pub const ALL: [Button; 16] = [
        Button::South,
        Button::East,
        Button::West,
        Button::North,
        Button::L1,
        Button::R1,
        Button::L3,
        Button::R3,
        Button::Start,
        Button::Select,
        Button::Guide,
        Button::Touchpad,
        Button::DpadUp,
        Button::DpadDown,
        Button::DpadLeft,
        Button::DpadRight,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons(pub u32);

impl Buttons {
    pub fn set(&mut self, b: Button, pressed: bool) {
        let mask = 1u32 << (b as u8);
        if pressed {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
    }

    pub fn is_pressed(self, b: Button) -> bool {
        self.0 & (1u32 << (b as u8)) != 0
    }
}

/// Normalized controller state: axes in [-1, 1] with +y up, triggers in [0, 1].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputState {
    pub left: Vec2,
    pub right: Vec2,
    pub l2: f32,
    pub r2: f32,
    pub buttons: Buttons,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_set_and_query_are_independent() {
        let mut b = Buttons::default();
        b.set(Button::South, true);
        b.set(Button::DpadRight, true);
        assert!(b.is_pressed(Button::South));
        assert!(b.is_pressed(Button::DpadRight));
        assert!(!b.is_pressed(Button::East));
        b.set(Button::South, false);
        assert!(!b.is_pressed(Button::South));
        assert_eq!(b.0, 1 << (Button::DpadRight as u8));
    }

    #[test]
    fn button_all_matches_count_and_order() {
        assert_eq!(Button::ALL.len(), Button::COUNT);
        for (i, b) in Button::ALL.iter().enumerate() {
            assert_eq!(*b as usize, i);
        }
    }

    #[test]
    fn vec2_length_and_scale() {
        let v = Vec2 { x: 3.0, y: 4.0 };
        assert_eq!(v.length(), 5.0);
        assert_eq!(v.scale(0.5), Vec2 { x: 1.5, y: 2.0 });
    }
}
