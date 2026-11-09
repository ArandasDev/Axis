use crate::input::{Button, Buttons};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemapParams {
    pub target: [Option<Button>; Button::COUNT],
}

impl Default for RemapParams {
    fn default() -> Self {
        Self {
            target: Button::ALL.map(Some),
        }
    }
}

pub fn remap(src: Buttons, p: &RemapParams) -> Buttons {
    let mut out = Buttons::default();
    for b in Button::ALL {
        if src.is_pressed(b)
            && let Some(t) = p.target[b as usize]
        {
            out.set(t, true);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_remap_is_identity() {
        let mut b = Buttons::default();
        b.set(Button::North, true);
        assert_eq!(remap(b, &RemapParams::default()), b);
    }

    #[test]
    fn swap_and_disable() {
        let mut p = RemapParams::default();
        p.target[Button::South as usize] = Some(Button::East);
        p.target[Button::East as usize] = Some(Button::South);
        p.target[Button::Touchpad as usize] = None;
        let mut b = Buttons::default();
        b.set(Button::South, true);
        b.set(Button::Touchpad, true);
        let out = remap(b, &p);
        assert!(out.is_pressed(Button::East));
        assert!(!out.is_pressed(Button::South));
        assert!(!out.is_pressed(Button::Touchpad));
    }
}
