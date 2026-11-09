use axis_core::InputState;

/// Rumble motor intensities reported by the game (0-255).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rumble {
    pub strong: u8,
    pub weak: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum VirtualError {
    #[error("ViGEmBus não encontrado")]
    BusNotFound,
    #[error("sem slot livre no ViGEmBus")]
    NoFreeSlot,
    #[error("erro do ViGEmBus: {0}")]
    Bus(String),
}

/// A virtual controller. Dropping it disconnects the virtual device.
pub trait VirtualPad: Send {
    fn update(&mut self, input: &InputState) -> Result<(), VirtualError>;
    fn user_index(&self) -> u32;
}

pub type RumbleSink = Box<dyn FnMut(Rumble) + Send>;

pub trait VirtualBus: Send + Sync {
    fn plug_x360(&self, on_rumble: RumbleSink) -> Result<Box<dyn VirtualPad>, VirtualError>;
}
