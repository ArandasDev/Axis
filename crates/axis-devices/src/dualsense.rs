//! Experimental: layout da documentação pública; sem verificação em hardware (spec 4.1.1).

use crate::crc::crc32_bt_output;
use crate::ds4::{axis_u8, dpad_buttons, trigger_u8};
use crate::is_usable_mac;
use crate::{DeviceStatus, OutputState, Report, Transport};
use axis_core::{Button, Buttons, InputState, Vec2};

/// Experimental, da documentação pública: ler este feature report faz o DualSense enviar o
/// report completo `0x31` no Bluetooth (em vez do `0x01` curto).
pub const FEATURE_ENABLE_FULL_BT: u8 = 0x05;

/// Decodifica um report de entrada (`0x01` USB, `0x31` Bluetooth).
pub fn parse(buf: &[u8]) -> Option<Report> {
    let d = match *buf.first()? {
        0x01 if buf.len() >= 64 => &buf[1..],
        0x31 if buf.len() >= 78 => &buf[2..],
        _ => return None,
    };

    let mut buttons = Buttons::default();
    dpad_buttons(d[7] & 0x0F, &mut buttons);
    for (mask, b) in [
        (0x10, Button::West),
        (0x20, Button::South),
        (0x40, Button::East),
        (0x80, Button::North),
    ] {
        buttons.set(b, d[7] & mask != 0);
    }
    for (mask, b) in [
        (0x01, Button::L1),
        (0x02, Button::R1),
        (0x10, Button::Select),
        (0x20, Button::Start),
        (0x40, Button::L3),
        (0x80, Button::R3),
    ] {
        buttons.set(b, d[8] & mask != 0);
    }
    buttons.set(Button::Guide, d[9] & 0x01 != 0);
    buttons.set(Button::Touchpad, d[9] & 0x02 != 0);

    let input = InputState {
        left: Vec2 {
            x: axis_u8(d[0], false),
            y: axis_u8(d[1], true),
        },
        right: Vec2 {
            x: axis_u8(d[2], false),
            y: axis_u8(d[3], true),
        },
        l2: trigger_u8(d[4]),
        r2: trigger_u8(d[5]),
        buttons,
    };

    let level = d[52] & 0x0F;
    let status = DeviceStatus {
        battery_percent: Some((level * 10).min(100)),
        charging: d[52] >> 4 == 1,
    };

    Some(Report { input, status })
}

/// Máscara de 5 bits dos LEDs de jogador (1..=4); outro valor apaga todos.
pub fn player_leds(player: u8) -> u8 {
    match player {
        1 => 0b00100,
        2 => 0b01010,
        3 => 0b10101,
        4 => 0b11011,
        _ => 0,
    }
}

/// Monta o report de saída (vibração, lightbar e LEDs de jogador).
pub fn output_report(transport: Transport, out: &OutputState) -> Vec<u8> {
    let mut p = [0u8; 47];
    p[0] = 0x03;
    p[1] = 0x14;
    p[2] = out.rumble_weak;
    p[3] = out.rumble_strong;
    p[43] = out.player.map_or(0, player_leds);
    p[44..47].copy_from_slice(&out.lightbar);
    match transport {
        Transport::Usb => {
            let mut buf = Vec::with_capacity(48);
            buf.push(0x02);
            buf.extend_from_slice(&p);
            buf
        }
        Transport::Bluetooth => {
            let mut buf = vec![0u8; 78];
            buf[0] = 0x31;
            buf[1] = 0x02;
            buf[2..49].copy_from_slice(&p);
            let crc = crc32_bt_output(0xA2, &buf[..74]);
            buf[74..78].copy_from_slice(&crc.to_le_bytes());
            buf
        }
    }
}

/// MAC (ordem de exibição) a partir do feature report `0x09`; `None` se inválido ou MAC inutilizável.
pub fn mac_from_feature_0x09(buf: &[u8]) -> Option<[u8; 6]> {
    if buf.len() < 7 || buf[0] != 0x09 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, b) in buf[1..7].iter().rev().enumerate() {
        mac[i] = *b;
    }
    is_usable_mac(&mac).then_some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb_rest() -> [u8; 64] {
        let mut b = [0u8; 64];
        b[0] = 0x01;
        b[1] = 128;
        b[2] = 128;
        b[3] = 128;
        b[4] = 128;
        b[8] = 0x08;
        b
    }

    #[test]
    fn usb_report_maps_axes_triggers_and_buttons() {
        let mut b = usb_rest();
        b[1] = 255;
        b[5] = 255;
        b[6] = 0;
        b[8] = 0x80 | 0x04;
        b[9] = 0x02 | 0x10;
        b[10] = 0x01;
        let r = parse(&b).unwrap();
        assert_eq!(r.input.left.x, 1.0);
        assert_eq!((r.input.l2, r.input.r2), (1.0, 0.0));
        for btn in [
            Button::North,
            Button::DpadDown,
            Button::R1,
            Button::Select,
            Button::Guide,
        ] {
            assert!(r.input.buttons.is_pressed(btn), "{btn:?}");
        }
    }

    #[test]
    fn bluetooth_report_uses_offset_two() {
        let mut b = [0u8; 78];
        b[0] = 0x31;
        b[2] = 0;
        b[3] = 128;
        b[4] = 128;
        b[5] = 128;
        b[9] = 0x08;
        assert_eq!(parse(&b).unwrap().input.left.x, -1.0);
    }

    #[test]
    fn battery_state_is_decoded() {
        let mut b = usb_rest();
        b[53] = 0x10 | 0x07;
        assert_eq!(
            parse(&b).unwrap().status,
            DeviceStatus {
                battery_percent: Some(70),
                charging: true
            }
        );
    }

    #[test]
    fn short_or_unknown_reports_are_rejected() {
        assert!(parse(&[0x01; 10]).is_none());
        assert!(parse(&[0x31; 40]).is_none());
        assert!(parse(&[0x05; 64]).is_none());
        assert!(parse(&[0x01; 63]).is_none());
        assert!(parse(&[0x31; 77]).is_none());
        assert!(parse(&[0x01; 64]).is_some());
        assert!(parse(&[0x31; 78]).is_some());
    }

    #[test]
    fn player_led_patterns() {
        assert_eq!(
            [1, 2, 3, 4, 0, 5].map(player_leds),
            [0b00100, 0b01010, 0b10101, 0b11011, 0, 0]
        );
    }

    #[test]
    fn outputs_place_payload_and_crc() {
        let out = OutputState {
            lightbar: [1, 2, 3],
            rumble_strong: 9,
            rumble_weak: 8,
            player: Some(2),
        };
        let u = output_report(Transport::Usb, &out);
        assert_eq!(u.len(), 48);
        assert_eq!((u[0], u[1], u[2], u[3], u[4]), (0x02, 0x03, 0x14, 8, 9));
        assert_eq!((u[44], &u[45..48]), (0b01010, &[1u8, 2, 3][..]));
        let b = output_report(Transport::Bluetooth, &out);
        assert_eq!((b.len(), b[0], b[1], b[2]), (78, 0x31, 0x02, 0x03));
        assert_eq!(&b[46..49], &[1, 2, 3]);
        assert_eq!(
            u32::from_le_bytes(b[74..78].try_into().unwrap()),
            crc32_bt_output(0xA2, &b[..74])
        );
    }

    #[test]
    fn mac_from_feature_0x09_is_reversed() {
        let mut f = [0u8; 20];
        f[0] = 0x09;
        f[1..7].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        assert_eq!(mac_from_feature_0x09(&f), Some([1, 2, 3, 4, 5, 6]));
    }
}
