//! Detecção de chegada e remoção de dispositivos HID por notificação do Windows.
//!
//! Sem thread própria e sem polling: o Windows chama a callback registrada
//! com `CM_Register_Notification`.

use std::ffi::c_void;
use std::mem::{offset_of, size_of};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL,
    CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_EVENT_DATA_0_0,
    CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification,
    CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
};
use windows_sys::core::GUID;

use crate::DeviceError;

/// `GUID_DEVINTERFACE_HID` = {4D1E55B2-F16F-11CF-88CB-001111000030}.
const GUID_DEVINTERFACE_HID: GUID = GUID {
    data1: 0x4D1E_55B2,
    data2: 0xF16F,
    data3: 0x11CF,
    data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};

/// Chegada ou remoção de uma interface HID; o valor é o path do dispositivo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotplugEvent {
    Arrived(String),
    Removed(String),
}

type Callback = Box<dyn Fn(HotplugEvent) + Send + Sync>;

/// Mantém o registro de notificação enquanto vive; `Drop` cancela o registro.
pub struct HotplugWatcher {
    handle: HCMNOTIFICATION,
    /// Ponteiro de `Box<Callback>` passado como contexto; liberado no `Drop`.
    context: *mut Callback,
}

// SAFETY: `handle` e `context` só são usados no `Drop` (cancelar o registro e
// liberar a caixa). A caixa guarda um closure `Send + Sync`, que a callback do
// Windows acessa apenas por referência compartilhada.
unsafe impl Send for HotplugWatcher {}

impl HotplugWatcher {
    /// Começa a observar chegadas e remoções de dispositivos HID.
    ///
    /// `on_event` roda em uma thread do Windows; não destrua o watcher de
    /// dentro dele (o cancelamento espera as callbacks em andamento).
    pub fn start(
        on_event: impl Fn(HotplugEvent) + Send + Sync + 'static,
    ) -> Result<HotplugWatcher, DeviceError> {
        let boxed: Box<Callback> = Box::new(Box::new(on_event));
        let context = Box::into_raw(boxed);

        let mut filter = CM_NOTIFY_FILTER {
            cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            ..Default::default()
        };
        filter.u.DeviceInterface.ClassGuid = GUID_DEVINTERFACE_HID;

        let mut handle: HCMNOTIFICATION = Default::default();
        // SAFETY: `filter` é válido durante a chamada; `context` é um ponteiro
        // de `Box` vivo até o `Drop`; `handle` é uma saída válida.
        let cr = unsafe {
            CM_Register_Notification(
                &filter,
                context as *const c_void,
                Some(notify_callback),
                &mut handle,
            )
        };
        if cr != CR_SUCCESS {
            // SAFETY: o registro falhou, então nenhuma callback usa `context`.
            drop(unsafe { Box::from_raw(context) });
            return Err(DeviceError::Hid(format!(
                "CM_Register_Notification falhou: {cr}"
            )));
        }
        Ok(HotplugWatcher { handle, context })
    }
}

impl Drop for HotplugWatcher {
    fn drop(&mut self) {
        // SAFETY: `handle` veio de um registro bem-sucedido. O cancelamento
        // espera as callbacks em andamento, então só depois a caixa é liberada.
        unsafe {
            CM_Unregister_Notification(self.handle);
            drop(Box::from_raw(self.context));
        }
    }
}

/// Callback chamada pelo Windows. Nunca deixa um pânico atravessar o FFI.
unsafe extern "system" fn notify_callback(
    _hnotify: HCMNOTIFICATION,
    context: *const c_void,
    action: CM_NOTIFY_ACTION,
    eventdata: *const CM_NOTIFY_EVENT_DATA,
    eventdatasize: u32,
) -> u32 {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: `context` é o ponteiro do `Box<Callback>` guardado no
        // watcher, vivo até o fim do `CM_Unregister_Notification`.
        let callback = unsafe { &*(context as *const Callback) };
        // SAFETY: o Windows garante `eventdata` válido por `eventdatasize` bytes.
        let Some(path) = (unsafe { symbolic_link_of(eventdata, eventdatasize) }) else {
            return;
        };
        let event = match action {
            CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL => HotplugEvent::Arrived(path),
            CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL => HotplugEvent::Removed(path),
            _ => return,
        };
        callback(event);
    });
    0 // ERROR_SUCCESS
}

/// Lê o `SymbolicLink` do evento, sem passar de `size` bytes.
///
/// # Safety
/// `data` deve ser nulo ou apontar para ao menos `size` bytes legíveis.
unsafe fn symbolic_link_of(data: *const CM_NOTIFY_EVENT_DATA, size: u32) -> Option<String> {
    let offset =
        offset_of!(CM_NOTIFY_EVENT_DATA, u) + offset_of!(CM_NOTIFY_EVENT_DATA_0_0, SymbolicLink);
    let size = size as usize;
    if data.is_null() || size < offset + size_of::<u16>() {
        return None;
    }
    // SAFETY: `data` é legível por `size` bytes e `offset < size`.
    let first = unsafe { (data as *const u8).add(offset) } as *const u16;
    let len = (size - offset) / size_of::<u16>();
    // SAFETY: `len` unidades u16 a partir de `first` ficam dentro de `size`.
    let wide = unsafe { std::slice::from_raw_parts(first, len) };
    Some(path_from_symbolic_link(wide))
}

/// Converte o link simbólico UTF-16 (terminado em NUL) em `String`.
pub(crate) fn path_from_symbolic_link(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_link_is_converted_up_to_nul() {
        let w: Vec<u16> = "\\\\?\\HID#VID_054C&PID_09CC#x#{4d1e55b2}\0lixo"
            .encode_utf16()
            .collect();
        assert_eq!(
            path_from_symbolic_link(&w),
            "\\\\?\\HID#VID_054C&PID_09CC#x#{4d1e55b2}"
        );
    }

    #[test]
    #[ignore = "hardware: desligue e religue um DS4 durante 15 s"]
    fn arrival_and_removal_are_reported() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let _w = HotplugWatcher::start(move |e| {
            let _ = tx.lock().unwrap().send(e);
        })
        .unwrap();
        let events: Vec<HotplugEvent> = rx.iter().take(2).collect();
        assert!(events.iter().any(|e| matches!(e, HotplugEvent::Removed(_))));
        assert!(events.iter().any(|e| matches!(e, HotplugEvent::Arrived(_))));
    }
}
