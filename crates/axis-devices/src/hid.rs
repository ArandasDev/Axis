//! Controle HID (DS4/DualSense) sobre hidapi.

use crate::{
    Controller, DeviceError, DeviceStatus, HidInfo, OutputState, Report, Transport, classify, ds4,
    dualsense, resolve_identity,
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
    /// Último pedido de modo completo feito durante a leitura (DS4 Bluetooth).
    last_full_request: Option<Instant>,
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

/// Feature reports lidos ao abrir, em ordem. O primeiro é o de identidade (MAC); no Bluetooth
/// o segundo tira o controle do modo simples.
pub(crate) fn features_to_read(family: DeviceFamily, transport: Transport) -> &'static [u8] {
    match (family, transport) {
        (DeviceFamily::DualShock4, Transport::Bluetooth) => &[0x12, ds4::FEATURE_ENABLE_FULL_BT],
        (DeviceFamily::DualShock4, _) => &[0x12],
        (DeviceFamily::DualSense, Transport::Bluetooth) => {
            &[0x09, dualsense::FEATURE_ENABLE_FULL_BT]
        }
        (DeviceFamily::DualSense, _) => &[0x09],
        (DeviceFamily::Xbox, _) => &[],
    }
}

impl HidController {
    pub fn open(api: &HidApi, c: &HidCandidate) -> Result<HidController, DeviceError> {
        let path = CString::new(c.path.as_bytes())
            .map_err(|_| DeviceError::Hid("path HID inválido".into()))?;
        let dev = api.open_path(&path).map_err(map_err)?;
        let mut mac = None;
        for (i, &feature) in features_to_read(c.family, c.transport).iter().enumerate() {
            let data = read_feature(&dev, feature);
            if i == 0 {
                mac = match c.family {
                    DeviceFamily::DualShock4 => ds4::mac_from_feature_0x12(&data),
                    DeviceFamily::DualSense => dualsense::mac_from_feature_0x09(&data),
                    DeviceFamily::Xbox => None,
                };
            }
        }
        let id = resolve_identity(mac, c.serial.as_deref(), &c.path);
        Ok(HidController {
            dev,
            family: c.family,
            transport: c.transport,
            id,
            buf: [0; 128],
            last_full_request: None,
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

/// Reports DS4 `0x01` via Bluetooth são completados pelo Windows até 128 bytes
/// e não trazem bateria real; descarta o status para evitar 0% falso.
pub(crate) fn sanitize_status(
    family: DeviceFamily,
    transport: Transport,
    report_id: u8,
    mut report: Report,
) -> Report {
    if family == DeviceFamily::DualShock4 && transport == Transport::Bluetooth && report_id == 0x01
    {
        report.status = DeviceStatus::default();
    }
    report
}

/// Intervalo mínimo entre dois pedidos de modo completo feitos durante a leitura.
const FULL_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

/// DS4 Bluetooth que voltou (ex.: religado) pode reportar no modo reduzido `0x01`;
/// pede o modo completo de novo, no máximo uma vez por segundo.
pub(crate) fn should_request_full_mode(
    family: DeviceFamily,
    transport: Transport,
    report_id: u8,
    last: Option<Instant>,
    now: Instant,
) -> bool {
    family == DeviceFamily::DualShock4
        && transport == Transport::Bluetooth
        && report_id == 0x01
        && last.is_none_or(|t| now.saturating_duration_since(t) >= FULL_REQUEST_INTERVAL)
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
                let now = Instant::now();
                if should_request_full_mode(
                    self.family,
                    self.transport,
                    self.buf[0],
                    self.last_full_request,
                    now,
                ) {
                    self.last_full_request = Some(now);
                    // Buffer na pilha (sem alocação); a resposta não interessa.
                    let mut feature = [0u8; 64];
                    feature[0] = ds4::FEATURE_ENABLE_FULL_BT;
                    let _ = self.dev.get_feature_report(&mut feature);
                }
                return Ok(Some(sanitize_status(
                    self.family,
                    self.transport,
                    self.buf[0],
                    r,
                )));
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
    fn feature_reads_per_family_and_transport() {
        use Transport::{Bluetooth, Usb};
        assert_eq!(features_to_read(DeviceFamily::DualShock4, Usb), &[0x12]);
        assert_eq!(
            features_to_read(DeviceFamily::DualShock4, Bluetooth),
            &[0x12, ds4::FEATURE_ENABLE_FULL_BT]
        );
        assert_eq!(features_to_read(DeviceFamily::DualSense, Usb), &[0x09]);
        assert_eq!(
            features_to_read(DeviceFamily::DualSense, Bluetooth),
            &[0x09, dualsense::FEATURE_ENABLE_FULL_BT]
        );
        assert!(features_to_read(DeviceFamily::Xbox, Usb).is_empty());
    }

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

    #[test]
    fn full_mode_is_re_requested_at_most_once_per_second_for_ds4_bt_reduced_reports() {
        use DeviceFamily::{DualSense, DualShock4};
        use Transport::{Bluetooth, Usb};
        let now = Instant::now();
        let ago = |ms| now.checked_sub(Duration::from_millis(ms)).unwrap();
        assert!(should_request_full_mode(
            DualShock4, Bluetooth, 0x01, None, now
        ));
        assert!(should_request_full_mode(
            DualShock4,
            Bluetooth,
            0x01,
            Some(ago(1000)),
            now
        ));
        assert!(!should_request_full_mode(
            DualShock4,
            Bluetooth,
            0x01,
            Some(ago(400)),
            now
        ));
        assert!(!should_request_full_mode(
            DualShock4, Bluetooth, 0x11, None, now
        ));
        assert!(!should_request_full_mode(DualShock4, Usb, 0x01, None, now));
        assert!(!should_request_full_mode(
            DualSense, Bluetooth, 0x01, None, now
        ));
    }

    #[test]
    fn ds4_bt_reduced_report_has_no_battery() {
        let r = Report {
            status: DeviceStatus {
                battery_percent: Some(0),
                charging: false,
            },
            ..Report::default()
        };
        assert_eq!(
            sanitize_status(DeviceFamily::DualShock4, Transport::Bluetooth, 0x01, r).status,
            DeviceStatus::default()
        );
        assert_eq!(
            sanitize_status(DeviceFamily::DualShock4, Transport::Bluetooth, 0x11, r).status,
            r.status
        );
        assert_eq!(
            sanitize_status(DeviceFamily::DualShock4, Transport::Usb, 0x01, r).status,
            r.status
        );
    }
}
