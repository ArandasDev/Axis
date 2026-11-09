//! Decodificação dos reports de entrada do DualShock 4 (USB e Bluetooth).

use crate::{DeviceStatus, Report};
use axis_core::{Button, Buttons, InputState, Vec2};

/// Ler este feature report coloca o DS4 no modo completo `0x11` no Bluetooth.
pub const FEATURE_ENABLE_FULL_BT: u8 = 0x05;

/// Converte um eixo de 0..=255 para -1.0..=1.0 (centro em 127.5).
pub(crate) fn axis_u8(v: u8, invert: bool) -> f32 {
    let a = ((v as f32 - 127.5) / 127.5).clamp(-1.0, 1.0);
    if invert { -a } else { a }
}

/// Converte um gatilho de 0..=255 para 0.0..=1.0.
pub(crate) fn trigger_u8(v: u8) -> f32 {
    v as f32 / 255.0
}

/// Decodifica um report de entrada (`0x01` USB/BT reduzido, `0x11` BT completo).
pub fn parse(buf: &[u8]) -> Option<Report> {
    let (d, has_battery) = match *buf.first()? {
        0x01 if buf.len() >= 10 => (&buf[1..], buf.len() >= 31),
        0x11 if buf.len() >= 33 => (&buf[3..], true),
        _ => return None,
    };

    let mut buttons = Buttons::default();
    let (up, right, down, left) = match d[4] & 0x0F {
        0 => (true, false, false, false),
        1 => (true, true, false, false),
        2 => (false, true, false, false),
        3 => (false, true, true, false),
        4 => (false, false, true, false),
        5 => (false, false, true, true),
        6 => (false, false, false, true),
        7 => (true, false, false, true),
        _ => (false, false, false, false),
    };
    buttons.set(Button::DpadUp, up);
    buttons.set(Button::DpadRight, right);
    buttons.set(Button::DpadDown, down);
    buttons.set(Button::DpadLeft, left);
    for (mask, b) in [
        (0x10, Button::West),
        (0x20, Button::South),
        (0x40, Button::East),
        (0x80, Button::North),
    ] {
        buttons.set(b, d[4] & mask != 0);
    }
    for (mask, b) in [
        (0x01, Button::L1),
        (0x02, Button::R1),
        (0x10, Button::Select),
        (0x20, Button::Start),
        (0x40, Button::L3),
        (0x80, Button::R3),
    ] {
        buttons.set(b, d[5] & mask != 0);
    }
    buttons.set(Button::Guide, d[6] & 0x01 != 0);
    buttons.set(Button::Touchpad, d[6] & 0x02 != 0);

    let input = InputState {
        left: Vec2 {
            x: axis_u8(d[0], false),
            y: axis_u8(d[1], true),
        },
        right: Vec2 {
            x: axis_u8(d[2], false),
            y: axis_u8(d[3], true),
        },
        l2: trigger_u8(d[7]),
        r2: trigger_u8(d[8]),
        buttons,
    };

    let status = if has_battery {
        let level = d[29] & 0x0F;
        let cable = d[29] & 0x10 != 0;
        DeviceStatus {
            battery_percent: Some((level * 10).min(100)),
            charging: cable && level < 11,
        }
    } else {
        DeviceStatus::default()
    };

    Some(Report { input, status })
}

/// MAC (ordem de exibição) a partir do feature report `0x12`; `None` se inválido ou zerado.
pub fn mac_from_feature_0x12(buf: &[u8]) -> Option<[u8; 6]> {
    if buf.len() < 7 || buf[0] != 0x12 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, b) in buf[1..7].iter().rev().enumerate() {
        mac[i] = *b;
    }
    if mac == [0; 6] { None } else { Some(mac) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceStatus;
    use axis_core::Button;

    /// Monta um report USB 0x01 de 64 bytes em repouso (sticks 128, d-pad solto).
    fn usb_rest() -> [u8; 64] {
        let mut b = [0u8; 64];
        b[0] = 0x01;
        b[1] = 128;
        b[2] = 128;
        b[3] = 128;
        b[4] = 128;
        b[5] = 0x08;
        b
    }

    #[test]
    fn rest_report_is_centered_and_released() {
        let r = parse(&usb_rest()).unwrap();
        assert!(r.input.left.x.abs() < 0.01 && r.input.left.y.abs() < 0.01);
        assert_eq!(r.input.buttons.0, 0);
        assert_eq!(r.input.l2, 0.0);
    }

    #[test]
    fn stick_down_is_negative_y_and_right_is_positive_x() {
        let mut b = usb_rest();
        b[1] = 255;
        b[2] = 255;
        let r = parse(&b).unwrap();
        assert_eq!(r.input.left.x, 1.0);
        assert_eq!(r.input.left.y, -1.0);
    }

    #[test]
    fn face_buttons_dpad_and_triggers_map_to_abstract_buttons() {
        let mut b = usb_rest();
        b[5] = 0x20 | 0x02; // ✕ + d-pad direita
        b[6] = 0x01 | 0x20; // L1 + Options
        b[7] = 0x01 | 0x02; // PS + touchpad
        b[8] = 255;
        b[9] = 51; // L2 cheio, R2 20%
        let r = parse(&b).unwrap();
        for btn in [
            Button::South,
            Button::DpadRight,
            Button::L1,
            Button::Start,
            Button::Guide,
            Button::Touchpad,
        ] {
            assert!(r.input.buttons.is_pressed(btn), "{btn:?}");
        }
        assert!(!r.input.buttons.is_pressed(Button::DpadUp));
        assert_eq!(r.input.l2, 1.0);
        assert!((r.input.r2 - 0.2).abs() < 1e-6);
    }

    #[test]
    fn diagonal_dpad_sets_two_directions_and_out_of_range_is_released() {
        let mut b = usb_rest();
        b[5] = 0x07; // NO
        let r = parse(&b).unwrap();
        assert!(
            r.input.buttons.is_pressed(Button::DpadUp)
                && r.input.buttons.is_pressed(Button::DpadLeft)
        );
        b[5] = 0x0B; // valor inválido de clone
        assert_eq!(parse(&b).unwrap().input.buttons.0, 0);
    }

    #[test]
    fn battery_and_charging_are_decoded_and_clamped() {
        let mut b = usb_rest();
        b[30] = 0x10 | 0x05; // cabo, nível 5
        let s = parse(&b).unwrap().status;
        assert_eq!(
            s,
            DeviceStatus {
                battery_percent: Some(50),
                charging: true
            }
        );
        b[30] = 0x10 | 0x0F; // nível fora da faixa
        assert_eq!(
            parse(&b).unwrap().status,
            DeviceStatus {
                battery_percent: Some(100),
                charging: false
            }
        );
    }

    #[test]
    fn bluetooth_full_report_uses_offset_three() {
        let mut b = [0u8; 78];
        b[0] = 0x11;
        b[3] = 255;
        b[4] = 0;
        b[5] = 128;
        b[6] = 128;
        b[7] = 0x08;
        let r = parse(&b).unwrap();
        assert_eq!(r.input.left.x, 1.0);
        assert_eq!(r.input.left.y, 1.0);
    }

    #[test]
    fn bluetooth_reduced_report_has_no_battery() {
        let mut b = [0u8; 10];
        b[0] = 0x01;
        b[1] = 128;
        b[2] = 128;
        b[3] = 128;
        b[4] = 128;
        b[5] = 0x08;
        assert_eq!(parse(&b).unwrap().status.battery_percent, None);
    }

    #[test]
    fn short_or_unknown_reports_are_rejected() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x01, 128, 128]).is_none());
        assert!(parse(&[0x11; 20]).is_none());
        let mut b = usb_rest();
        b[0] = 0x12;
        assert!(parse(&b).is_none());
    }

    #[test]
    fn mac_from_usb_feature_is_reversed() {
        let mut f = [0u8; 16];
        f[0] = 0x12;
        f[1..7].copy_from_slice(&[0xB6, 0x84, 0x00, 0x00, 0x42, 0x41]);
        assert_eq!(
            mac_from_feature_0x12(&f),
            Some([0x41, 0x42, 0x00, 0x00, 0x84, 0xB6])
        );
        assert_eq!(mac_from_feature_0x12(&[0x12, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(mac_from_feature_0x12(&[0x12, 1, 2]), None);
    }
}
