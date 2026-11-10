//! Virtual controller backends (ViGEmBus) isolated behind traits.

mod hidhide;
mod pad;
mod x360;

pub use hidhide::{DeviceHider, HidHide, HideError, instance_id_from_path};
pub use pad::{Rumble, RumbleSink, VirtualBus, VirtualError, VirtualPad};
pub use x360::{ViGEmBus, to_xgamepad};
