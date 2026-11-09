//! Virtual controller backends (ViGEmBus) isolated behind traits.

mod pad;
mod x360;

pub use pad::{Rumble, RumbleSink, VirtualBus, VirtualError, VirtualPad};
pub use x360::to_xgamepad;
