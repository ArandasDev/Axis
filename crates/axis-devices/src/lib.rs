//! Drivers de gamepads (DualShock 4, DualSense, Xbox) para o Axis.

mod classify;
mod crc;
mod identity;
mod types;

pub use classify::{HidInfo, classify};
pub use crc::crc32;
pub use identity::{fallback_id, format_mac, parse_serial_mac, resolve_identity};
pub use types::{DeviceError, DeviceStatus, OutputState, Report, Transport};
