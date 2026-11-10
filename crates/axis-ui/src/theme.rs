//! Paleta de destaque em OKLCH.
#![allow(dead_code)] // ACCENTS e o resto da paleta serão usados pelas próximas telas

/// Destaques disponíveis: (id, rótulo). O laranja é fixo; os demais usam L 0,67.
pub const ACCENTS: [(&str, &str); 7] = [
    ("laranja", "Laranja"),
    ("vermelho", "Vermelho"),
    ("pink", "Pink"),
    ("roxo", "Roxo"),
    ("indigo", "Índigo"),
    ("azul", "Azul"),
    ("verde", "Verde"),
];

const ORANGE: [u8; 3] = [0xd9, 0x77, 0x57];
const LIGHTNESS: f32 = 0.67;

/// Converte OKLCH em sRGB de 8 bits (com clamp ao gamut).
pub fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> [u8; 3] {
    let h = h_deg.to_radians();
    let a = c * h.cos();
    let b = c * h.sin();

    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (lc, mc, sc) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);

    let r = 4.076_741_7 * lc - 3.307_711_6 * mc + 0.230_969_94 * sc;
    let g = -1.268_438 * lc + 2.609_757_4 * mc - 0.341_319_4 * sc;
    let bl = -0.004_196_086_3 * lc - 0.703_418_6 * mc + 1.707_614_7 * sc;

    [encode(r), encode(g), encode(bl)]
}

fn encode(linear: f32) -> u8 {
    let v = linear.clamp(0.0, 1.0);
    let gamma = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (gamma * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Cor do destaque pelo id; id desconhecido cai no laranja.
pub fn accent_rgb(id: &str) -> [u8; 3] {
    let (c, h) = match id {
        "vermelho" => (0.17, 27.0),
        "pink" => (0.16, 355.0),
        "roxo" => (0.14, 305.0),
        "indigo" => (0.14, 275.0),
        "azul" => (0.13, 245.0),
        "verde" => (0.12, 150.0),
        _ => return ORANGE,
    };
    oklch_to_srgb(LIGHTNESS, c, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(rgb: [u8; 3]) -> f32 {
        let lin = |v: u8| {
            let c = f32::from(v) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
    }

    #[test]
    fn oklch_known_values() {
        assert_eq!(oklch_to_srgb(1.0, 0.0, 0.0), [255, 255, 255]);
        assert_eq!(oklch_to_srgb(0.0, 0.0, 0.0), [0, 0, 0]);
        let [r, _, b] = oklch_to_srgb(0.67, 0.13, 245.0);
        assert!(b > r, "azul esperado, veio r={r} b={b}");
    }

    #[test]
    fn accent_palette_has_seven_and_falls_back() {
        assert_eq!(ACCENTS.len(), 7);
        assert_eq!(accent_rgb("laranja"), [0xd9, 0x77, 0x57]);
        assert_eq!(accent_rgb("???"), accent_rgb("laranja"));
    }

    #[test]
    fn accents_share_lightness() {
        let lums: Vec<f32> = ACCENTS
            .iter()
            .filter(|(id, _)| *id != "laranja")
            .map(|(id, _)| luminance(accent_rgb(id)))
            .collect();
        for a in &lums {
            for b in &lums {
                assert!((a - b).abs() <= 0.08, "luminâncias {a} e {b} divergem");
            }
        }
    }
}
