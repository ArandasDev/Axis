const POLY: u32 = 0xEDB8_8320;

fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & mask);
        }
    }
    crc
}

/// CRC32 IEEE (refletido, polinômio 0xEDB88320, init e xor final 0xFFFFFFFF).
pub fn crc32(data: &[u8]) -> u32 {
    !update(0xFFFF_FFFF, data)
}

/// CRC de `[seed] ++ body`, sem alocar.
// Ainda sem chamadores: usado pelos drivers Bluetooth nas próximas tarefas.
#[allow(dead_code)]
pub(crate) fn crc32_bt_output(seed: u8, body: &[u8]) -> u32 {
    !update(update(0xFFFF_FFFF, &[seed]), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn bt_output_crc_prefixes_the_seed() {
        let body = [0x11u8, 0x80, 0x00, 0xFF];
        assert_eq!(
            crc32_bt_output(0xA2, &body),
            crc32(&[0xA2, 0x11, 0x80, 0x00, 0xFF])
        );
    }
}
