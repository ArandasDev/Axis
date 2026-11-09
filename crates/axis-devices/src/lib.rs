//! Drivers de gamepads (DualShock 4, DualSense, Xbox) para o Axis.

mod crc;
mod types;

pub use crc::crc32;
pub use types::{DeviceError, DeviceStatus, OutputState, Report, Transport};
