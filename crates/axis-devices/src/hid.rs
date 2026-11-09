//! Controle HID (DS4/DualSense) sobre hidapi.

use crate::{
    Controller, DeviceError, HidInfo, OutputState, Report, Transport, classify, ds4, dualsense,
    resolve_identity,
};
use axis_core::DeviceFamily;
use hidapi::{BusType, HidApi, HidDevice};
use std::ffi::CString;
use std::time::{Duration, Instant};

/// Dispositivo HID suportado encontrado na enumeração.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidCandidate {
    pub path: String,
    pub family: DeviceFamily,
    pub transport: Transport,
    pub serial: Option<String>,
}

/// Lista os controles suportados, na ordem do hidapi e sem paths repetidos.
pub fn enumerate_hid(api: &HidApi) -> Vec<HidCandidate> {
    let mut out: Vec<HidCandidate> = Vec::new();
    for d in api.device_list() {
        let info = HidInfo {
            vid: d.vendor_id(),
            pid: d.product_id(),
            usage_page: d.usage_page(),
            usage: d.usage(),
            product: d.product_string(),
            bluetooth: matches!(d.bus_type(), BusType::Bluetooth),
        };
        let Some((family, transport)) = classify(&info) else {
            continue;
        };
        let path = d.path().to_string_lossy().into_owned();
        if out.iter().any(|c| c.path == path) {
            continue;
        }
        out.push(HidCandidate {
            path,
            family,
            transport,
            serial: d.serial_number().map(str::to_owned),
        });
    }
    out
}

pub struct HidController {
    dev: HidDevice,
    family: DeviceFamily,
    transport: Transport,
    id: String,
    buf: [u8; 128],
}

fn is_disconnect_message(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("0x0000048f") || m.contains("0x0000001e")
}

fn map_err(e: hidapi::HidError) -> DeviceError {
    let msg = e.to_string();
    if is_disconnect_message(&msg) {
        DeviceError::Disconnected
    } else {
        DeviceError::Hid(msg)
    }
}

/// Lê um feature report (ignora erros); devolve os bytes lidos.
fn read_feature(dev: &HidDevice, report_id: u8) -> Vec<u8> {
    let mut buf = [0u8; 64];
    buf[0] = report_id;
    match dev.get_feature_report(&mut buf) {
        Ok(n) => buf[..n.min(64)].to_vec(),
        Err(_) => Vec::new(),
    }
}

impl HidController {
    pub fn open(api: &HidApi, c: &HidCandidate) -> Result<HidController, DeviceError> {
        let path = CString::new(c.path.as_bytes())
            .map_err(|_| DeviceError::Hid("path HID inválido".into()))?;
        let dev = api.open_path(&path).map_err(map_err)?;
        let mac = match c.family {
            DeviceFamily::DualShock4 => {
                let mac = ds4::mac_from_feature_0x12(&read_feature(&dev, 0x12));
                if c.transport == Transport::Bluetooth {
                    read_feature(&dev, ds4::FEATURE_ENABLE_FULL_BT);
                }
                mac
            }
            DeviceFamily::DualSense => dualsense::mac_from_feature_0x09(&read_feature(&dev, 0x09)),
            DeviceFamily::Xbox => None,
        };
        let id = resolve_identity(mac, c.serial.as_deref(), &c.path);
        Ok(HidController {
            dev,
            family: c.family,
            transport: c.transport,
            id,
            buf: [0; 128],
        })
    }

    fn parse(&self, n: usize) -> Option<Report> {
        let data = &self.buf[..n];
        match self.family {
            DeviceFamily::DualShock4 => ds4::parse(data),
            DeviceFamily::DualSense => dualsense::parse(data),
            DeviceFamily::Xbox => None,
        }
    }
}

impl Controller for HidController {
    fn id(&self) -> &str {
        &self.id
    }

    fn family(&self) -> DeviceFamily {
        self.family
    }

    fn transport(&self) -> Transport {
        self.transport
    }

    fn experimental(&self) -> bool {
        self.family == DeviceFamily::DualSense
    }

    fn read(&mut self, timeout_ms: i32) -> Result<Option<Report>, DeviceError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.as_millis().min(i32::MAX as u128) as i32;
            let n = self
                .dev
                .read_timeout(&mut self.buf, wait)
                .map_err(map_err)?;
            if n > 0
                && let Some(r) = self.parse(n)
            {
                return Ok(Some(r));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    fn write_output(&mut self, out: &OutputState) -> Result<(), DeviceError> {
        let report = match self.family {
            DeviceFamily::DualShock4 => ds4::output_report(self.transport, out),
            DeviceFamily::DualSense => dualsense::output_report(self.transport, out),
            DeviceFamily::Xbox => return Err(DeviceError::Unsupported("saída HID para Xbox")),
        };
        self.dev.write(&report).map_err(map_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OutputState;
    use axis_core::DeviceFamily;

    #[test]
    fn disconnect_errors_are_recognized() {
        assert!(is_disconnect_message(
            "hidapi error: ReadFile: (0x0000048F) O dispositivo não está conectado."
        ));
        assert!(is_disconnect_message(
            "hidapi error: WriteFile: (0x0000001E) …"
        ));
        assert!(!is_disconnect_message(
            "hidapi error: (0x00000057) Parâmetro incorreto."
        ));
    }

    #[test]
    #[ignore = "hardware: um DS4 conectado"]
    fn ds4_is_enumerated_opened_and_reads_reports() {
        let api = hidapi::HidApi::new().unwrap();
        let c = enumerate_hid(&api)
            .into_iter()
            .find(|c| c.family == DeviceFamily::DualShock4)
            .expect("DS4 conectado");
        let mut ctl = HidController::open(&api, &c).unwrap();
        assert!(
            ctl.id().len() == 17 || ctl.id().starts_with("dev-"),
            "id: {}",
            ctl.id()
        );
        let mut got = 0;
        for _ in 0..50 {
            if ctl.read(100).unwrap().is_some() {
                got += 1;
            }
        }
        assert!(got >= 40, "só {got} reports em 50 leituras");
    }

    #[test]
    #[ignore = "hardware: um DS4 conectado; olhe a lightbar"]
    fn ds4_lightbar_turns_green_then_back() {
        let api = hidapi::HidApi::new().unwrap();
        let c = enumerate_hid(&api)
            .into_iter()
            .find(|c| c.family == DeviceFamily::DualShock4)
            .unwrap();
        let mut ctl = HidController::open(&api, &c).unwrap();
        ctl.write_output(&OutputState {
            lightbar: [0, 255, 0],
            ..Default::default()
        })
        .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        ctl.write_output(&OutputState {
            lightbar: [0, 0, 64],
            ..Default::default()
        })
        .unwrap();
    }
}
