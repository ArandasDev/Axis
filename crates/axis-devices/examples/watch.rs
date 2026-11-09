//! Imprime chegadas e remoções de dispositivos HID e, a cada chegada, os
//! controles reconhecidos. Roda até Ctrl+C.

use axis_devices::{HotplugEvent, HotplugWatcher, enumerate_hid};
use hidapi::HidApi;

fn main() {
    let _watcher = HotplugWatcher::start(|event| {
        println!("{event:?}");
        if matches!(event, HotplugEvent::Arrived(_)) {
            match HidApi::new() {
                Ok(api) => {
                    for c in enumerate_hid(&api) {
                        println!("  candidato: {c:?}");
                    }
                }
                Err(e) => eprintln!("  hidapi: {e}"),
            }
        }
    })
    .expect("falha ao iniciar o watcher");
    println!("Observando dispositivos HID (Ctrl+C para sair)...");
    loop {
        std::thread::park();
    }
}
