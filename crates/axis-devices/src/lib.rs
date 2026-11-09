//! Drivers de gamepads (DualShock 4, DualSense, Xbox) para o Axis.

mod classify;
mod controller;
mod crc;
pub mod ds4;
pub mod dualsense;
mod hid;
mod hotplug;
mod identity;
mod types;
pub mod xinput;

pub use classify::{HidInfo, classify};
pub use controller::Controller;
pub use crc::crc32;
pub use hid::{HidCandidate, HidController, enumerate_hid};
pub use hotplug::{HotplugEvent, HotplugWatcher};
pub use identity::{fallback_id, format_mac, parse_serial_mac, resolve_identity};
pub use types::{DeviceError, DeviceStatus, OutputState, Report, Transport};
pub use xinput::{XInputController, available_slots};
