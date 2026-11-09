//! Gravação e replay de sessões de pacotes crus.

use crate::{Report, Transport, ds4, dualsense};
use axis_core::DeviceFamily;

/// Um pacote cru com o instante (µs desde o início da gravação).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub t_us: u64,
    pub data: Vec<u8>,
}

/// Sessão gravada de pacotes crus de um controle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    pub family: DeviceFamily,
    pub transport: Transport,
    pub frames: Vec<Frame>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RecordingError {
    #[error("cabeçalho inválido")]
    BadHeader,
    #[error("versão {0} não suportada")]
    Version(u32),
    #[error("linha {0} inválida")]
    BadLine(usize),
}

fn family_name(f: DeviceFamily) -> &'static str {
    match f {
        DeviceFamily::DualShock4 => "DualShock4",
        DeviceFamily::DualSense => "DualSense",
        DeviceFamily::Xbox => "Xbox",
    }
}

fn transport_name(t: Transport) -> &'static str {
    match t {
        Transport::Usb => "usb",
        Transport::Bluetooth => "bluetooth",
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

impl Recording {
    pub fn to_text(&self) -> String {
        let mut out = format!(
            "axis-rec 1 {} {}\n",
            family_name(self.family),
            transport_name(self.transport)
        );
        for f in &self.frames {
            out.push_str(&f.t_us.to_string());
            out.push(' ');
            for b in &f.data {
                out.push_str(&format!("{b:02x}"));
            }
            out.push('\n');
        }
        out
    }

    pub fn from_text(s: &str) -> Result<Recording, RecordingError> {
        let mut lines = s.lines();
        let mut head = lines.next().unwrap_or("").split_whitespace();
        if head.next() != Some("axis-rec") {
            return Err(RecordingError::BadHeader);
        }
        let version: u32 = head
            .next()
            .filter(|v| v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse().ok())
            .ok_or(RecordingError::BadHeader)?;
        let family = match head.next() {
            Some("DualShock4") => DeviceFamily::DualShock4,
            Some("DualSense") => DeviceFamily::DualSense,
            Some("Xbox") => DeviceFamily::Xbox,
            _ => return Err(RecordingError::BadHeader),
        };
        let transport = match head.next() {
            Some("usb") => Transport::Usb,
            Some("bluetooth") => Transport::Bluetooth,
            _ => return Err(RecordingError::BadHeader),
        };
        if head.next().is_some() {
            return Err(RecordingError::BadHeader);
        }
        if version != 1 {
            return Err(RecordingError::Version(version));
        }
        let mut frames = Vec::new();
        for (i, line) in lines.enumerate() {
            let n = i + 2;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(t), Some(hex), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(RecordingError::BadLine(n));
            };
            let t_us = Some(t)
                .filter(|t| t.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|t| t.parse().ok())
                .ok_or(RecordingError::BadLine(n))?;
            let data = decode_hex(hex).ok_or(RecordingError::BadLine(n))?;
            frames.push(Frame { t_us, data });
        }
        Ok(Recording {
            family,
            transport,
            frames,
        })
    }
}

/// Decodifica um pacote cru conforme a família (Xbox não tem pacote HID cru).
pub fn parse_frame(family: DeviceFamily, data: &[u8]) -> Option<Report> {
    match family {
        DeviceFamily::DualShock4 => ds4::parse(data),
        DeviceFamily::DualSense => dualsense::parse(data),
        DeviceFamily::Xbox => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let r = Recording {
            family: DeviceFamily::DualShock4,
            transport: Transport::Bluetooth,
            frames: vec![
                Frame {
                    t_us: 0,
                    data: vec![0x11, 0xAB],
                },
                Frame {
                    t_us: 4995,
                    data: vec![0x11, 0x00],
                },
            ],
        };
        let text = r.to_text();
        assert!(text.starts_with("axis-rec 1 DualShock4 bluetooth\n"));
        assert_eq!(Recording::from_text(&text).unwrap(), r);
    }

    #[test]
    fn bad_inputs_are_explicit_errors() {
        assert_eq!(Recording::from_text("lixo"), Err(RecordingError::BadHeader));
        assert_eq!(
            Recording::from_text("axis-rec 2 DualShock4 usb\n"),
            Err(RecordingError::Version(2))
        );
        assert_eq!(
            Recording::from_text("axis-rec 1 DualShock4 usb\n10 zz\n"),
            Err(RecordingError::BadLine(2))
        );
    }

    #[test]
    fn signs_are_rejected() {
        assert_eq!(
            Recording::from_text("axis-rec 1 DualShock4 usb\n10 +f\n"),
            Err(RecordingError::BadLine(2))
        );
        assert_eq!(
            Recording::from_text("axis-rec 1 DualShock4 usb\n+5 00\n"),
            Err(RecordingError::BadLine(2))
        );
        assert_eq!(
            Recording::from_text("axis-rec +1 DualShock4 usb\n"),
            Err(RecordingError::BadHeader)
        );
    }
}
