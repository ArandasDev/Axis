use crate::{DeviceError, OutputState, Report, Transport};
use axis_core::DeviceFamily;

/// Um controle físico, lido por uma thread bloqueante dedicada.
pub trait Controller: Send {
    fn id(&self) -> &str;
    fn family(&self) -> DeviceFamily;
    fn transport(&self) -> Transport;
    fn experimental(&self) -> bool;
    /// Bloqueia até chegar um report ou `timeout_ms` passar (`Ok(None)`).
    fn read(&mut self, timeout_ms: i32) -> Result<Option<Report>, DeviceError>;
    fn write_output(&mut self, out: &OutputState) -> Result<(), DeviceError>;
}
