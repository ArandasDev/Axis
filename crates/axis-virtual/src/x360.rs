use axis_core::{Button, InputState};
use vigem_client::{XButtons, XGamepad};

use std::thread::JoinHandle;
use std::time::Duration;
use vigem_client::{Client, TargetId, Xbox360Wired};

use crate::{Rumble, RumbleSink, VirtualBus, VirtualError, VirtualPad};

/// Traduz erros do vigem-client para o erro da camada virtual.
pub(crate) fn map_vigem_error(e: vigem_client::Error) -> VirtualError {
    match e {
        vigem_client::Error::BusNotFound => VirtualError::BusNotFound,
        vigem_client::Error::NoFreeSlot => VirtualError::NoFreeSlot,
        other => VirtualError::Bus(other.to_string()),
    }
}

/// `ERROR_NO_MORE_ITEMS`: o driver/XInput ainda não terminou de preparar o virtual recém-criado
/// (índice de jogador ou primeiros reports); basta tentar de novo em instantes.
const NOT_READY_YET: u32 = 259;
/// Tentativas máximas do aquecimento no plug (cada uma espera `WARMUP_INTERVAL`).
const WARMUP_MAX_TRIES: u32 = 100;
const WARMUP_INTERVAL: Duration = Duration::from_millis(20);

/// Repete `op` enquanto o driver responde "ainda não pronto"; só usado no plug.
fn retry_not_ready<T>(
    mut op: impl FnMut() -> Result<T, vigem_client::Error>,
) -> Result<T, VirtualError> {
    let mut tries = 0;
    loop {
        match op() {
            Err(vigem_client::Error::WinError(NOT_READY_YET)) if tries < WARMUP_MAX_TRIES => {
                tries += 1;
                std::thread::sleep(WARMUP_INTERVAL);
            }
            other => return other.map_err(map_vigem_error),
        }
    }
}

/// Reports neutros bem-sucedidos seguidos exigidos para considerar o virtual aquecido
/// (logo após o plug o driver recusa os primeiros reports com `NOT_READY_YET`); ímpar para o último report enviado ser o neutro.
const WARMUP_OK_STREAK: u32 = 5;

/// Envia reports alternados (o último é neutro) até `WARMUP_OK_STREAK` sucessos seguidos; só usado no plug.
fn warm_up(target: &mut Xbox360Wired<Client>) -> Result<(), VirtualError> {
    let mut streak = 0;
    let mut tries = 0;
    while streak < WARMUP_OK_STREAK {
        // Alterna o report: o driver só recusa o primeiro report que muda o estado.
        let report = XGamepad {
            thumb_lx: (streak % 2) as i16,
            ..XGamepad::default()
        };
        match target.update(&report) {
            Ok(()) => streak += 1,
            Err(vigem_client::Error::WinError(NOT_READY_YET)) if tries < WARMUP_MAX_TRIES => {
                streak = 0;
                tries += 1;
                std::thread::sleep(WARMUP_INTERVAL);
            }
            Err(e) => return Err(map_vigem_error(e)),
        }
    }
    Ok(())
}

/// Barramento ViGEmBus. Cada virtual abre seu próprio cliente.
pub struct ViGEmBus;

impl ViGEmBus {
    /// Verifica que o ViGEmBus está instalado conectando uma vez.
    pub fn connect() -> Result<ViGEmBus, VirtualError> {
        Client::connect().map_err(map_vigem_error)?;
        Ok(ViGEmBus)
    }
}

impl VirtualBus for ViGEmBus {
    fn plug_x360(&self, mut on_rumble: RumbleSink) -> Result<Box<dyn VirtualPad>, VirtualError> {
        let client = Client::connect().map_err(map_vigem_error)?;
        let mut target = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
        target.plugin().map_err(map_vigem_error)?;
        target.wait_ready().map_err(map_vigem_error)?;
        // Todo o aquecimento acontece aqui, fora do caminho crítico de `update`.
        let user_index = retry_not_ready(|| target.get_user_index())?;
        warm_up(&mut target)?;
        // Sem notificação o virtual continua funcionando, só sem vibração.
        let notifier = target.request_notification().ok().map(|req| {
            req.spawn_thread(move |_req, n| {
                on_rumble(Rumble {
                    strong: n.large_motor,
                    weak: n.small_motor,
                });
            })
        });
        Ok(Box::new(ViGEmX360 {
            target,
            user_index,
            notifier,
        }))
    }
}

struct ViGEmX360 {
    target: Xbox360Wired<Client>,
    user_index: u32,
    notifier: Option<JoinHandle<()>>,
}

impl VirtualPad for ViGEmX360 {
    fn update(&mut self, input: &InputState) -> Result<(), VirtualError> {
        self.target
            .update(&to_xgamepad(input))
            .map_err(map_vigem_error)
    }

    fn user_index(&self) -> u32 {
        self.user_index
    }
}

impl Drop for ViGEmX360 {
    fn drop(&mut self) {
        // O unplug vem antes do join: a thread de notificação só termina depois
        // que o alvo sai do barramento. Se o unplug falhar ela continuaria
        // bloqueada, então nesse caso o handle é solto sem join.
        if self.target.unplug().is_ok()
            && let Some(handle) = self.notifier.take()
        {
            let _ = handle.join();
        }
    }
}

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

#[cfg(test)]
mod vigem_tests {
    use super::*;
    use crate::VirtualBus;
    use axis_core::InputState;

    #[test]
    fn vigem_errors_are_mapped() {
        assert!(matches!(
            map_vigem_error(vigem_client::Error::BusNotFound),
            VirtualError::BusNotFound
        ));
        assert!(matches!(
            map_vigem_error(vigem_client::Error::NoFreeSlot),
            VirtualError::NoFreeSlot
        ));
        assert!(matches!(
            map_vigem_error(vigem_client::Error::WinError(5)),
            VirtualError::Bus(_)
        ));
    }

    #[test]
    #[ignore = "hardware: ViGEmBus instalado"]
    fn plugs_a_virtual_pad_visible_in_xinput_and_unplugs_on_drop() {
        let bus = ViGEmBus::connect().unwrap();
        let mut pad = bus.plug_x360(Box::new(|_| {})).unwrap();
        let slot = pad.user_index();
        assert!(slot < 4);
        let mut i = InputState::default();
        i.left.x = 1.0;
        pad.update(&i).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(axis_devices::xinput_is_connected(slot));
        drop(pad);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!axis_devices::xinput_is_connected(slot));
    }
}
