//! Grava pacotes crus do primeiro controle HID: `record <segundos> <arquivo.rec>`.

use axis_core::DeviceFamily;
use axis_devices::{Frame, Recording, Transport, enumerate_hid};
use hidapi::HidApi;
use std::ffi::CString;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(secs), Some(file)) = (args.get(1).and_then(|s| s.parse::<u64>().ok()), args.get(2))
    else {
        eprintln!("uso: record <segundos> <arquivo.rec>");
        std::process::exit(2);
    };
    let api = HidApi::new().expect("hidapi");
    let Some(c) = enumerate_hid(&api).into_iter().next() else {
        eprintln!("nenhum controle HID encontrado");
        std::process::exit(1);
    };
    let path = CString::new(c.path.clone()).expect("path");
    let dev = api.open_path(&path).expect("falha ao abrir o controle");
    if c.family == DeviceFamily::DualShock4 && c.transport == Transport::Bluetooth {
        // Feature report 0x05 faz o DS4 passar a enviar o report 0x11 completo.
        let mut feat = [0u8; 64];
        feat[0] = 0x05;
        if let Err(e) = dev.get_feature_report(&mut feat) {
            eprintln!("aviso: feature report 0x05 falhou: {e}");
        }
    }
    let start = Instant::now();
    let limit = Duration::from_secs(secs);
    let mut frames = Vec::new();
    let mut buf = [0u8; 128];
    while start.elapsed() < limit {
        match dev.read_timeout(&mut buf, 100) {
            Ok(0) => {}
            Ok(n) => frames.push(Frame {
                t_us: start.elapsed().as_micros() as u64,
                data: buf[..n].to_vec(),
            }),
            Err(e) => {
                eprintln!("erro de leitura: {e}");
                break;
            }
        }
    }
    let rec = Recording {
        family: c.family,
        transport: c.transport,
        frames,
    };
    std::fs::write(file, rec.to_text()).expect("falha ao gravar");
    println!("{} frames gravados em {file}", rec.frames.len());
}
