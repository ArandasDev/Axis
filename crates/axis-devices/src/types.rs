use axis_core::InputState;

/// Como o controle está conectado ao PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    Usb,
    Bluetooth,
}

/// Estado auxiliar do dispositivo (bateria).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceStatus {
    pub battery_percent: Option<u8>,
    pub charging: bool,
}

/// Resultado da decodificação de um pacote de entrada.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Report {
    pub input: InputState,
    pub status: DeviceStatus,
}

/// Estado de saída desejado (lightbar, rumble, LED de jogador).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputState {
    pub lightbar: [u8; 3],
    pub rumble_strong: u8,
    pub rumble_weak: u8,
    /// Número do jogador, 1..=4.
    pub player: Option<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("dispositivo desconectado")]
    Disconnected,
    #[error("erro de HID: {0}")]
    Hid(String),
    #[error("não suportado: {0}")]
    Unsupported(&'static str),
}
