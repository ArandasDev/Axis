//! Controle Xbox via XInput (experimental). XInput só tem API por polling,
//! então `read` consulta o estado a cada 4 ms enquanto aguarda mudança.

use crate::{Controller, DeviceError, OutputState, Report, Transport};
use axis_core::{Button, DeviceFamily, InputState, Vec2};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD, XINPUT_STATE, XINPUT_VIBRATION, XInputGetState, XInputSetState,
};

const POLL_INTERVAL: Duration = Duration::from_millis(4);

const BUTTON_MAP: [(u16, Button); 14] = [
    (0x0001, Button::DpadUp),
    (0x0002, Button::DpadDown),
    (0x0004, Button::DpadLeft),
    (0x0008, Button::DpadRight),
    (0x0010, Button::Start),
    (0x0020, Button::Select),
    (0x0040, Button::L3),
    (0x0080, Button::R3),
    (0x0100, Button::L1),
    (0x0200, Button::R1),
    (0x1000, Button::South),
    (0x2000, Button::East),
    (0x4000, Button::West),
    (0x8000, Button::North),
];

fn query(slot: u32) -> Result<XINPUT_STATE, u32> {
    // SAFETY: XINPUT_STATE é POD; zerado é válido e a API só escreve nele.
    let mut state: XINPUT_STATE = unsafe { std::mem::zeroed() };
    // SAFETY: ponteiro válido para um XINPUT_STATE vivo.
    let rc = unsafe { XInputGetState(slot, &mut state) };
    if rc == ERROR_SUCCESS {
        Ok(state)
    } else {
        Err(rc)
    }
}

/// Parte pura de [`available_slots`].
pub(crate) fn filter_slots(connected: [bool; 4], exclude: &[u32]) -> Vec<u32> {
    (0u32..4)
        .filter(|s| connected[*s as usize] && !exclude.contains(s))
        .collect()
}

/// Slots 0..4 com controle conectado, exceto os de `exclude`
/// (ocupados pelos controles virtuais do próprio Axis).
pub fn available_slots(exclude: &[u32]) -> Vec<u32> {
    let mut connected = [false; 4];
    for (slot, c) in connected.iter_mut().enumerate() {
        *c = query(slot as u32).is_ok();
    }
    filter_slots(connected, exclude)
}

fn axis(v: i16) -> f32 {
    (v as f32 / 32767.0).clamp(-1.0, 1.0)
}

pub fn gamepad_to_report(g: &XINPUT_GAMEPAD) -> Report {
    let mut input = InputState {
        left: Vec2 {
            x: axis(g.sThumbLX),
            y: axis(g.sThumbLY),
        },
        right: Vec2 {
            x: axis(g.sThumbRX),
            y: axis(g.sThumbRY),
        },
        l2: g.bLeftTrigger as f32 / 255.0,
        r2: g.bRightTrigger as f32 / 255.0,
        ..InputState::default()
    };
    for (mask, button) in BUTTON_MAP {
        input.buttons.set(button, g.wButtons & mask != 0);
    }
    Report {
        input,
        ..Report::default()
    }
}

pub struct XInputController {
    slot: u32,
    last_packet: u32,
    id: String,
}

impl XInputController {
    pub fn open(slot: u32) -> Result<XInputController, DeviceError> {
        let state = query(slot).map_err(|_| DeviceError::Disconnected)?;
        Ok(XInputController {
            slot,
            last_packet: state.dwPacketNumber,
            id: format!("xinput-{slot}"),
        })
    }
}

impl Controller for XInputController {
    fn id(&self) -> &str {
        &self.id
    }

    fn family(&self) -> DeviceFamily {
        DeviceFamily::Xbox
    }

    fn transport(&self) -> Transport {
        Transport::Usb
    }

    fn experimental(&self) -> bool {
        true
    }

    fn read(&mut self, timeout_ms: i32) -> Result<Option<Report>, DeviceError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        loop {
            match query(self.slot) {
                Ok(state) => {
                    if state.dwPacketNumber != self.last_packet {
                        self.last_packet = state.dwPacketNumber;
                        return Ok(Some(gamepad_to_report(&state.Gamepad)));
                    }
                }
                // ERROR_DEVICE_NOT_CONNECTED (ou qualquer outra falha) = desconectado.
                Err(_) => return Err(DeviceError::Disconnected),
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL_INTERVAL.min(deadline - now));
        }
    }

    fn write_output(&mut self, out: &OutputState) -> Result<(), DeviceError> {
        let vib = XINPUT_VIBRATION {
            wLeftMotorSpeed: out.rumble_strong as u16 * 257,
            wRightMotorSpeed: out.rumble_weak as u16 * 257,
        };
        // SAFETY: ponteiro válido para um XINPUT_VIBRATION vivo.
        let rc = unsafe { XInputSetState(self.slot, &vib) };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(DeviceError::Disconnected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axis_core::Button;
    use windows_sys::Win32::UI::Input::XboxController::XINPUT_GAMEPAD;

    #[test]
    fn own_virtual_slots_are_excluded() {
        assert_eq!(filter_slots([true, true, false, true], &[0]), vec![1, 3]);
        assert_eq!(
            filter_slots([true, false, false, false], &[0]),
            Vec::<u32>::new()
        );
        assert_eq!(filter_slots([false; 4], &[]), Vec::<u32>::new());
    }

    #[test]
    fn gamepad_maps_axes_and_buttons() {
        let g = XINPUT_GAMEPAD {
            wButtons: 0x1000 | 0x0001 | 0x0100,
            bLeftTrigger: 255,
            bRightTrigger: 0,
            sThumbLX: 32767,
            sThumbLY: -32768,
            sThumbRX: 0,
            sThumbRY: 0,
        };
        let r = gamepad_to_report(&g);
        assert_eq!((r.input.left.x, r.input.left.y), (1.0, -1.0));
        assert_eq!(r.input.l2, 1.0);
        for b in [Button::South, Button::DpadUp, Button::L1] {
            assert!(r.input.buttons.is_pressed(b));
        }
    }

    #[test]
    #[ignore = "hardware: controle Xbox conectado e nenhum virtual do Axis"]
    fn xbox_controller_reads_state() {
        let slot = *available_slots(&[]).first().expect("controle Xbox");
        let mut c = XInputController::open(slot).unwrap();
        let _ = c.read(500).unwrap();
    }
}
