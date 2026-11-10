//! Message contract between the Axis daemon and the UI process.
//!
//! Wire format: one JSON object per line (`\n`-terminated), internally tagged
//! with `"t"`. The types mirror the core ones so this crate has no coupling to
//! the other Axis crates; conversion lives in the daemon.

use std::io::{ErrorKind, Read};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;
pub const PIPE_NAME: &str = r"\\.\pipe\axis-v1";
/// Maximum line length in bytes, counted before the terminating `\n`.
pub const MAX_LINE: usize = 65_536;

/// Mirror of the core pad state (`buttons` bits are `axis_core::Button as u8`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PadState {
    pub lx: f32,
    pub ly: f32,
    pub rx: f32,
    pub ry: f32,
    pub l2: f32,
    pub r2: f32,
    pub buttons: u32,
}

/// Per-stage stick values, each an `[x, y]` pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StickStages {
    pub raw: [f32; 2],
    pub calibrated: [f32; 2],
    pub drift: [f32; 2],
    pub deadzone: [f32; 2],
    pub curve: [f32; 2],
    pub anti: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Family {
    DualShock4,
    DualSense,
    Xbox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Usb,
    Bluetooth,
}

/// Number of angular sectors in `StickCorrectionDto::outer_radius`.
const SECTORS: usize = 16;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StickCorrectionDto {
    pub center: [f32; 2],
    pub outer_radius: Vec<f32>,
    pub drift_radius: f32,
}

impl Default for StickCorrectionDto {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0],
            outer_radius: vec![1.0; SECTORS],
            drift_radius: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerCalDto {
    pub rest: f32,
    pub max: f32,
}

impl Default for TriggerCalDto {
    fn default() -> Self {
        Self {
            rest: 0.0,
            max: 1.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CorrectionsDto {
    pub left: StickCorrectionDto,
    pub right: StickCorrectionDto,
    pub l2: TriggerCalDto,
    pub r2: TriggerCalDto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKindDto {
    DriftLeft,
    DriftRight,
    RangeLeft,
    RangeRight,
    Triggers,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureResultDto {
    Drift {
        center: [f32; 2],
        noise: f32,
        radius: f32,
        worn: bool,
    },
    Range {
        outer_radius: Vec<f32>,
        circularity_pct: f32,
    },
    Triggers {
        l2: Option<TriggerCalDto>,
        r2: Option<TriggerCalDto>,
        l2_presses: u8,
        r2_presses: u8,
    },
    Rejected {
        reason: String,
    },
    Incomplete {
        reason: String,
    },
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureState {
    pub kind: CaptureKindDto,
    pub progress: f32,
    pub result: Option<CaptureResultDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub owner: String,
    pub color: [u8; 3],
    pub family: Family,
    pub player: Option<u8>,
    pub connected: bool,
    pub experimental: bool,
    pub view_only: bool,
    pub battery: Option<u8>,
    pub charging: bool,
    /// `None` no Xbox/XInput.
    pub transport: Option<Transport>,
    pub disconnects: u32,
    pub corrections: CorrectionsDto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveFrame {
    pub id: String,
    pub input: PadState,
    pub output: PadState,
    pub left: StickStages,
    pub right: StickStages,
    pub rate_hz: u32,
    pub latency_p99_us: u32,
    pub battery: Option<u8>,
    pub charging: bool,
    pub jitter_us: u32,
    pub capture: Option<CaptureState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        version: u32,
    },
    Inspect {
        id: Option<String>,
    },
    SetPaused {
        paused: bool,
    },
    Restore,
    StartCapture {
        id: String,
        kind: CaptureKindDto,
    },
    CancelCapture {
        id: String,
    },
    SetCorrections {
        id: String,
        corrections: CorrectionsDto,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
// `Live` carries the whole frame inline; messages are transient and boxing would
// ripple through every consumer for no gain.
#[allow(clippy::large_enum_variant)]
pub enum ServerMsg {
    Welcome {
        version: u32,
        paused: bool,
        emulation: bool,
    },
    Incompatible {
        server_version: u32,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Live {
        frame: LiveFrame,
    },
    Notice {
        text: String,
    },
    /// Pausa ou emulação mudaram depois do `Welcome` (ex.: pela bandeja).
    State {
        paused: bool,
        emulation: bool,
    },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ProtocolError {
    #[error("mensagem malformada: {0}")]
    Malformed(String),
    #[error("linha acima do limite")]
    TooLong,
    #[error("erro de E/S: {0}")]
    Io(String),
}

/// Serializes a message as a single JSON line terminated by `\n`.
pub fn encode<T: Serialize>(msg: &T) -> String {
    // Serializing these plain data types cannot fail; fall back to an empty
    // object rather than panicking.
    let mut s = serde_json::to_string(msg).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

fn decode<T: DeserializeOwned>(line: &str) -> Result<T, ProtocolError> {
    serde_json::from_str(line.trim_end_matches(['\r', '\n']))
        .map_err(|e| ProtocolError::Malformed(e.to_string()))
}

pub fn decode_client(line: &str) -> Result<ClientMsg, ProtocolError> {
    decode(line)
}

pub fn decode_server(line: &str) -> Result<ServerMsg, ProtocolError> {
    decode(line)
}

/// Splits a byte stream into lines, bounded by [`MAX_LINE`].
///
/// After a `TooLong` error the reader is poisoned: every later call returns
/// `TooLong` again (the stream position is unrecoverable, so the caller should
/// drop the connection). A final line without `\n` at EOF is returned as a
/// line; `\r\n` is accepted.
pub struct LineReader<R: Read> {
    reader: R,
    buf: Vec<u8>,
    poisoned: bool,
}

impl<R: Read> LineReader<R> {
    pub fn new(r: R) -> Self {
        Self {
            reader: r,
            buf: Vec::new(),
            poisoned: false,
        }
    }

    /// `Ok(None)` means end of stream.
    pub fn next_line(&mut self) -> Result<Option<String>, ProtocolError> {
        if self.poisoned {
            return Err(ProtocolError::TooLong);
        }
        let mut scanned = 0;
        loop {
            if let Some(off) = self.buf[scanned..].iter().position(|&b| b == b'\n') {
                let end = scanned + off;
                if end > MAX_LINE {
                    return Err(self.poison());
                }
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop(); // '\n'
                return Self::to_string(line).map(Some);
            }
            scanned = self.buf.len();
            if self.buf.len() > MAX_LINE {
                return Err(self.poison());
            }
            let mut chunk = [0u8; 4096];
            match self.reader.read(&mut chunk) {
                Ok(0) => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    let line = std::mem::take(&mut self.buf);
                    return Self::to_string(line).map(Some);
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(ProtocolError::Io(e.to_string())),
            }
        }
    }

    fn poison(&mut self) -> ProtocolError {
        self.poisoned = true;
        self.buf.clear();
        ProtocolError::TooLong
    }

    fn to_string(mut line: Vec<u8>) -> Result<String, ProtocolError> {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        String::from_utf8(line).map_err(|e| ProtocolError::Malformed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn corrections() -> CorrectionsDto {
        let mut c = CorrectionsDto::default();
        c.left.center = [0.02, -0.01];
        c.left.outer_radius[3] = 0.9;
        c.right.drift_radius = 0.05;
        c.l2 = TriggerCalDto {
            rest: 0.04,
            max: 0.97,
        };
        c
    }

    fn session() -> SessionInfo {
        SessionInfo {
            id: "ds4-1".into(),
            owner: "Emmanoel".into(),
            color: [1, 2, 3],
            family: Family::DualShock4,
            player: Some(2),
            connected: true,
            experimental: false,
            view_only: true,
            battery: Some(80),
            charging: true,
            transport: Some(Transport::Bluetooth),
            disconnects: 3,
            corrections: corrections(),
        }
    }

    fn pad(v: f32) -> PadState {
        PadState {
            lx: v,
            ly: -v,
            rx: 0.5,
            ry: -0.5,
            l2: 0.25,
            r2: 1.0,
            buttons: 0b1011,
        }
    }

    fn stages(v: f32) -> StickStages {
        StickStages {
            raw: [v, v],
            calibrated: [0.1, 0.2],
            drift: [0.3, 0.4],
            deadzone: [0.5, 0.6],
            curve: [0.7, 0.8],
            anti: [0.9, 1.0],
        }
    }

    fn frame() -> LiveFrame {
        LiveFrame {
            id: "ds4-1".into(),
            input: pad(0.5),
            output: pad(0.25),
            left: stages(0.5),
            right: stages(-0.5),
            rate_hz: 250,
            latency_p99_us: 900,
            battery: None,
            charging: false,
            jitter_us: 120,
            capture: Some(CaptureState {
                kind: CaptureKindDto::RangeLeft,
                progress: 0.5,
                result: None,
            }),
        }
    }

    #[test]
    fn round_trip_every_message() {
        let clients = [
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
            },
            ClientMsg::Inspect {
                id: Some("ds4-1".into()),
            },
            ClientMsg::Inspect { id: None },
            ClientMsg::SetPaused { paused: true },
            ClientMsg::Restore,
        ];
        for m in clients {
            assert_eq!(decode_client(&encode(&m)).unwrap(), m);
        }
        let servers = [
            ServerMsg::Welcome {
                version: 1,
                paused: false,
                emulation: true,
            },
            ServerMsg::Incompatible { server_version: 9 },
            ServerMsg::Sessions {
                sessions: vec![session(), session()],
            },
            ServerMsg::Live { frame: frame() },
            ServerMsg::Notice {
                text: "olá\nmundo".into(),
            },
            ServerMsg::State {
                paused: true,
                emulation: false,
            },
        ];
        for m in servers {
            assert_eq!(decode_server(&encode(&m)).unwrap(), m);
        }
    }

    #[test]
    fn encoded_message_is_one_line() {
        let s = encode(&ServerMsg::Notice {
            text: "a\nb".into(),
        });
        assert!(s.ends_with('\n'));
        assert_eq!(s.matches('\n').count(), 1);
    }

    #[test]
    fn unknown_or_garbage_is_malformed() {
        assert!(matches!(
            decode_client("{\"t\":\"explode\"}"),
            Err(ProtocolError::Malformed(_))
        ));
        assert!(matches!(
            decode_client("lixo"),
            Err(ProtocolError::Malformed(_))
        ));
        assert!(matches!(
            decode_server("lixo"),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn line_reader_splits_and_limits() {
        let mut r = LineReader::new(Cursor::new(b"a\nb\n".to_vec()));
        assert_eq!(r.next_line(), Ok(Some("a".into())));
        assert_eq!(r.next_line(), Ok(Some("b".into())));
        assert_eq!(r.next_line(), Ok(None));

        let mut r = LineReader::new(Cursor::new(vec![b'x'; MAX_LINE + 1]));
        assert_eq!(r.next_line(), Err(ProtocolError::TooLong));
        assert_eq!(r.next_line(), Err(ProtocolError::TooLong));

        let mut r = LineReader::new(Cursor::new(vec![0xFF, b'\n']));
        assert!(matches!(r.next_line(), Err(ProtocolError::Malformed(_))));
    }

    #[test]
    fn line_reader_edge_cases() {
        let mut r = LineReader::new(Cursor::new(b"a\r\nlast".to_vec()));
        assert_eq!(r.next_line(), Ok(Some("a".into())));
        assert_eq!(r.next_line(), Ok(Some("last".into())));
        assert_eq!(r.next_line(), Ok(None));

        // exactly MAX_LINE bytes before '\n' is allowed
        let mut data = vec![b'x'; MAX_LINE];
        data.push(b'\n');
        let mut r = LineReader::new(Cursor::new(data));
        assert_eq!(r.next_line().unwrap().unwrap().len(), MAX_LINE);

        // long line with newline present in the same chunk is still too long
        let mut data = vec![b'x'; MAX_LINE + 1];
        data.push(b'\n');
        let mut r = LineReader::new(Cursor::new(data));
        assert_eq!(r.next_line(), Err(ProtocolError::TooLong));
    }

    #[test]
    fn pipe_name_is_in_the_win32_pipe_namespace() {
        assert!(PIPE_NAME.starts_with("\\\\.\\pipe\\"));
        assert!(PIPE_NAME.ends_with("axis-v1"));
        assert_eq!(PIPE_NAME.len(), 16);
    }

    #[test]
    fn state_uses_the_state_tag() {
        let s = encode(&ServerMsg::State {
            paused: true,
            emulation: false,
        });
        assert!(s.contains("\"t\":\"state\""), "{s}");
    }

    #[test]
    fn hello_carries_the_version() {
        let s = encode(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
        });
        assert!(s.contains("\"version\":2"));
    }

    #[test]
    fn version_is_two() {
        assert_eq!(PROTOCOL_VERSION, 2);
    }

    #[test]
    fn default_corrections_are_neutral() {
        let c = CorrectionsDto::default();
        assert_eq!(c.left.outer_radius.len(), 16);
        assert!(c.right.outer_radius.iter().all(|&r| r == 1.0));
        assert_eq!(c.left.center, [0.0, 0.0]);
        assert_eq!(c.left.drift_radius, 0.0);
        assert_eq!(
            c.l2,
            TriggerCalDto {
                rest: 0.0,
                max: 1.0
            }
        );
        assert_eq!(
            c.r2,
            TriggerCalDto {
                rest: 0.0,
                max: 1.0
            }
        );
    }

    #[test]
    fn capture_messages_round_trip() {
        let clients = [
            ClientMsg::StartCapture {
                id: "ds4-1".into(),
                kind: CaptureKindDto::DriftLeft,
            },
            ClientMsg::StartCapture {
                id: "ds4-1".into(),
                kind: CaptureKindDto::Triggers,
            },
            ClientMsg::CancelCapture { id: "ds4-1".into() },
            ClientMsg::SetCorrections {
                id: "ds4-1".into(),
                corrections: corrections(),
            },
        ];
        for m in clients {
            assert_eq!(decode_client(&encode(&m)).unwrap(), m);
        }
    }

    #[test]
    fn capture_results_round_trip() {
        let results = [
            CaptureResultDto::Drift {
                center: [0.01, 0.02],
                noise: 0.003,
                radius: 0.04,
                worn: true,
            },
            CaptureResultDto::Range {
                outer_radius: vec![0.9; 16],
                circularity_pct: 97.5,
            },
            CaptureResultDto::Triggers {
                l2: Some(TriggerCalDto {
                    rest: 0.02,
                    max: 0.98,
                }),
                r2: None,
                l2_presses: 3,
                r2_presses: 1,
            },
            CaptureResultDto::Rejected {
                reason: "mexeu".into(),
            },
            CaptureResultDto::Incomplete {
                reason: "faltou".into(),
            },
            CaptureResultDto::Aborted,
        ];
        for r in results {
            let mut f = frame();
            f.capture = Some(CaptureState {
                kind: CaptureKindDto::DriftRight,
                progress: 1.0,
                result: Some(r),
            });
            let m = ServerMsg::Live { frame: f };
            assert_eq!(decode_server(&encode(&m)).unwrap(), m);
        }
    }

    #[test]
    fn session_and_frame_new_fields_round_trip() {
        let mut s = session();
        s.transport = None;
        let m = ServerMsg::Sessions {
            sessions: vec![session(), s],
        };
        assert_eq!(decode_server(&encode(&m)).unwrap(), m);
        let mut f = frame();
        f.capture = None;
        let m = ServerMsg::Live { frame: f };
        assert_eq!(decode_server(&encode(&m)).unwrap(), m);
    }

    #[test]
    fn capture_tags_are_pinned() {
        let s = encode(&ClientMsg::StartCapture {
            id: "x".into(),
            kind: CaptureKindDto::DriftLeft,
        });
        assert!(s.contains("\"t\":\"start_capture\""), "{s}");
        assert!(s.contains("\"kind\":\"drift_left\""), "{s}");
        let s = encode(&ClientMsg::CancelCapture { id: "x".into() });
        assert!(s.contains("\"t\":\"cancel_capture\""), "{s}");
        let s = encode(&ClientMsg::SetCorrections {
            id: "x".into(),
            corrections: CorrectionsDto::default(),
        });
        assert!(s.contains("\"t\":\"set_corrections\""), "{s}");
    }
}
