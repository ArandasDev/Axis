use axis_core::{Button, InputState};
use vigem_client::{XButtons, XGamepad};

fn axis(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn trigger(t: f32) -> u8 {
    (t.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn xinput_bit(b: Button) -> u16 {
    match b {
        Button::South => 0x1000,
        Button::East => 0x2000,
        Button::West => 0x4000,
        Button::North => 0x8000,
        Button::L1 => 0x0100,
        Button::R1 => 0x0200,
        Button::L3 => 0x0040,
        Button::R3 => 0x0080,
        Button::Start => 0x0010,
        Button::Select => 0x0020,
        Button::Guide => 0x0400,
        Button::DpadUp => 0x0001,
        Button::DpadDown => 0x0002,
        Button::DpadLeft => 0x0004,
        Button::DpadRight => 0x0008,
        Button::Touchpad => 0,
    }
}

/// Converts normalized input into an XInput gamepad report (+y up, same as XInput).
pub fn to_xgamepad(input: &InputState) -> XGamepad {
    let raw = Button::ALL
        .iter()
        .filter(|b| input.buttons.is_pressed(**b))
        .fold(0u16, |acc, b| acc | xinput_bit(*b));
    XGamepad {
        buttons: XButtons { raw },
        left_trigger: trigger(input.l2),
        right_trigger: trigger(input.r2),
        thumb_lx: axis(input.left.x),
        thumb_ly: axis(input.left.y),
        thumb_rx: axis(input.right.x),
        thumb_ry: axis(input.right.y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axis_core::{Button, InputState, Vec2};

    #[test]
    fn full_deflection_maps_to_extremes_with_rounding() {
        let i = InputState {
            left: Vec2 { x: 1.0, y: -1.0 },
            right: Vec2 { x: 0.5, y: 0.0 },
            l2: 1.0,
            r2: 0.5,
            ..InputState::default()
        };
        let g = to_xgamepad(&i);
        assert_eq!((g.thumb_lx, g.thumb_ly), (32767, -32767));
        assert_eq!(g.thumb_rx, 16384); // 16383.5 arredonda para cima
        assert_eq!((g.left_trigger, g.right_trigger), (255, 128));
    }

    #[test]
    fn rest_is_exact_zero() {
        let g = to_xgamepad(&InputState::default());
        assert_eq!(
            (
                g.thumb_lx,
                g.thumb_ly,
                g.thumb_rx,
                g.thumb_ry,
                g.left_trigger,
                g.right_trigger,
                g.buttons.raw
            ),
            (0, 0, 0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn buttons_map_to_xinput_bits_and_touchpad_is_ignored() {
        let mut i = InputState::default();
        for b in [
            Button::South,
            Button::North,
            Button::Select,
            Button::Guide,
            Button::DpadLeft,
            Button::Touchpad,
        ] {
            i.buttons.set(b, true);
        }
        assert_eq!(
            to_xgamepad(&i).buttons.raw,
            0x1000 | 0x8000 | 0x0020 | 0x0400 | 0x0004
        );
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let i = InputState {
            left: Vec2 { x: 3.0, y: 0.0 },
            l2: -1.0,
            ..InputState::default()
        };
        let g = to_xgamepad(&i);
        assert_eq!((g.thumb_lx, g.left_trigger), (32767, 0));
    }
}
