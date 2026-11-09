//! Identidade estável de um controle: MAC quando disponível, senão hash do path.

/// Formata um MAC como `"a4:ae:12:00:be:ef"`.
pub fn format_mac(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Interpreta um serial como MAC (12 dígitos hex, com ou sem `:`/`-`).
/// Um MAC todo zero é tratado como inválido.
pub fn parse_serial_mac(serial: &str) -> Option<[u8; 6]> {
    let digits: String = serial.chars().filter(|c| *c != ':' && *c != '-').collect();
    if digits.len() != 12 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    (mac != [0; 6]).then_some(mac)
}

/// Id de fallback: `"dev-"` + FNV-1a 64 bits do path em maiúsculas.
pub fn fallback_id(instance_path: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in instance_path.to_uppercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("dev-{h:016x}")
}

/// MAC do feature report, depois MAC do serial, depois `fallback_id`.
pub fn resolve_identity(
    feature_mac: Option<[u8; 6]>,
    serial: Option<&str>,
    instance_path: &str,
) -> String {
    feature_mac
        .or_else(|| serial.and_then(parse_serial_mac))
        .map(format_mac)
        .unwrap_or_else(|| fallback_id(instance_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_formats_lowercase_with_colons() {
        assert_eq!(
            format_mac([0x41, 0x42, 0x00, 0x00, 0x84, 0xB6]),
            "41:42:00:00:84:b6"
        );
    }

    #[test]
    fn serial_mac_accepts_bare_and_separated_forms() {
        let m = Some([0x41, 0x42, 0x00, 0x00, 0x84, 0xB6]);
        assert_eq!(parse_serial_mac("4142000084b6"), m);
        assert_eq!(parse_serial_mac("41:42:00:00:84:B6"), m);
        assert_eq!(parse_serial_mac("41-42-00-00-84-b6"), m);
        assert_eq!(parse_serial_mac(""), None);
        assert_eq!(parse_serial_mac("000000000000"), None);
        assert_eq!(parse_serial_mac("Wireless 1234"), None);
    }

    #[test]
    fn identity_prefers_feature_then_serial_then_fallback() {
        let p = r"\?\HID#VID_054C&PID_09CC#7&ABC&0&0000#{4d1e55b2}";
        assert_eq!(
            resolve_identity(Some([1, 2, 3, 4, 5, 6]), Some("4142000084b6"), p),
            "01:02:03:04:05:06"
        );
        assert_eq!(
            resolve_identity(None, Some("4142000084b6"), p),
            "41:42:00:00:84:b6"
        );
        let f = resolve_identity(None, Some(""), p);
        assert!(f.starts_with("dev-") && f.len() == 20);
        assert_eq!(f, resolve_identity(None, None, p));
        assert_ne!(
            f,
            resolve_identity(
                None,
                None,
                r"\?\HID#VID_054C&PID_09CC#7&DEF&0&0000#{4d1e55b2}"
            )
        );
    }
}
