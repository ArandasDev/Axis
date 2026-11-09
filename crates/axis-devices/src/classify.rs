//! Classificação de dispositivos HID como controles suportados.

use crate::Transport;
use axis_core::DeviceFamily;

const SONY_VID: u16 = 0x054C;

/// Dados HID mínimos para classificar um dispositivo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidInfo<'a> {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub product: Option<&'a str>,
    pub bluetooth: bool,
}

/// Identifica DS4/DualSense (inclusive clones de DS4). Xbox entra pelo XInput.
pub fn classify(info: &HidInfo) -> Option<(DeviceFamily, Transport)> {
    let family = match (info.vid, info.pid) {
        (SONY_VID, 0x05C4 | 0x09CC | 0x0BA0) => DeviceFamily::DualShock4,
        (SONY_VID, 0x0CE6 | 0x0DF2) => DeviceFamily::DualSense,
        _ if info.usage_page == 0x01
            && info.usage == 0x05
            && info.product == Some("Wireless Controller") =>
        {
            DeviceFamily::DualShock4
        }
        _ => return None,
    };
    let transport = if info.bluetooth {
        Transport::Bluetooth
    } else {
        Transport::Usb
    };
    Some((family, transport))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(vid: u16, pid: u16, product: Option<&str>, bluetooth: bool) -> HidInfo<'_> {
        HidInfo {
            vid,
            pid,
            usage_page: 0x01,
            usage: 0x05,
            product,
            bluetooth,
        }
    }

    #[test]
    fn sony_ids_are_classified() {
        assert_eq!(
            classify(&info(0x054C, 0x09CC, None, true)),
            Some((DeviceFamily::DualShock4, Transport::Bluetooth))
        );
        assert_eq!(
            classify(&info(0x054C, 0x05C4, None, false)),
            Some((DeviceFamily::DualShock4, Transport::Usb))
        );
        assert_eq!(
            classify(&info(0x054C, 0x0CE6, None, false)),
            Some((DeviceFamily::DualSense, Transport::Usb))
        );
    }

    #[test]
    fn clone_named_wireless_controller_is_ds4() {
        assert_eq!(
            classify(&info(0x7545, 0x0104, Some("Wireless Controller"), true)),
            Some((DeviceFamily::DualShock4, Transport::Bluetooth))
        );
    }

    #[test]
    fn unrelated_devices_are_ignored() {
        assert_eq!(
            classify(&info(0x045E, 0x028E, Some("Controller"), false)),
            None
        );
        let mut keyboard = info(0x054C, 0x09CC, None, false);
        keyboard.usage = 0x06;
        assert_eq!(
            classify(&HidInfo {
                vid: 0x1234,
                ..keyboard
            }),
            None
        );
    }
}
