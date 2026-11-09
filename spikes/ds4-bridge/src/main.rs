//! SPIKE (Fase 0, a) — descartável.
//! Perguntas: do Rust conseguimos (1) ler o DS4 por USB/BT e obter o MAC,
//! (2) criar e alimentar um Xbox 360 virtual (ViGEm), (3) ocultar o DS4 pelo
//! HidHide (e com que privilégio), (4) qual a latência leitura → envio?
//!
//! Uso: ds4-bridge-spike <list|read|bridge|hidehide-status|hide|unhide> [segundos]

mod hidhide;

use std::time::{Duration, Instant};

const SONY_VID: u16 = 0x054C;
const DS4_PIDS: [u16; 3] = [0x05C4, 0x09CC, 0x0BA0];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("list");
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let api = hidapi::HidApi::new().expect("hidapi");
    match cmd {
        "list" => list(&api),
        "read" => read(&api, secs),
        "bridge" => bridge(&api, secs),
        "hidehide-status" => hidhide::status(),
        "hide" => hide(&api, true),
        "unhide" => hide(&api, false),
        other => eprintln!("comando desconhecido: {other}"),
    }
}

fn is_gamepad(d: &hidapi::DeviceInfo) -> bool {
    d.usage_page() == 0x01 && (d.usage() == 0x05 || d.usage() == 0x04)
}

fn list(api: &hidapi::HidApi) {
    for d in api.device_list().filter(|d| is_gamepad(d)) {
        println!(
            "VID {:04X} PID {:04X} bus {:?} usage {:02X}:{:02X}\n  produto: {:?} fabricante: {:?} serial: {:?}\n  path: {}",
            d.vendor_id(),
            d.product_id(),
            d.bus_type(),
            d.usage_page(),
            d.usage(),
            d.product_string(),
            d.manufacturer_string(),
            d.serial_number(),
            d.path().to_string_lossy()
        );
    }
}

/// Escolhe o primeiro dispositivo que pareça um DS4: VID/PID Sony ou um
/// gamepad cujo produto seja "Wireless Controller" (clones).
fn find_ds4(api: &hidapi::HidApi) -> Option<&hidapi::DeviceInfo> {
    api.device_list().find(|d| {
        (d.vendor_id() == SONY_VID && DS4_PIDS.contains(&d.product_id()))
            || (is_gamepad(d) && d.product_string() == Some("Wireless Controller"))
    })
}

#[derive(Debug, Default, Clone, Copy)]
struct Ds4State {
    lx: u8,
    ly: u8,
    rx: u8,
    ry: u8,
    l2: u8,
    r2: u8,
    b5: u8,
    b6: u8,
    b7: u8,
}

/// Decodifica um input report. USB e BT reduzido: id 0x01, dados a partir do byte 1.
/// BT completo: id 0x11, dados a partir do byte 3.
fn parse(buf: &[u8]) -> Option<(Ds4State, &'static str)> {
    let (off, kind) = match buf.first()? {
        0x01 if buf.len() >= 10 => (1, "0x01"),
        0x11 if buf.len() >= 12 => (3, "0x11 (BT completo)"),
        _ => return None,
    };
    let b = &buf[off..];
    Some((
        Ds4State {
            lx: b[0],
            ly: b[1],
            rx: b[2],
            ry: b[3],
            b5: b[4],
            b6: b[5],
            b7: b[6],
            l2: b[7],
            r2: b[8],
        },
        kind,
    ))
}

fn read_mac(dev: &hidapi::HidDevice, info: &hidapi::DeviceInfo) {
    // USB: feature report 0x12 traz o MAC do controle nos bytes 1..=6 (invertido).
    let mut f = [0u8; 64];
    f[0] = 0x12;
    match dev.get_feature_report(&mut f) {
        Ok(n) if n >= 7 => {
            let m: Vec<String> = f[1..7].iter().rev().map(|b| format!("{b:02X}")).collect();
            println!("MAC (feature 0x12): {}", m.join(":"));
        }
        Ok(n) => println!("feature 0x12 retornou {n} bytes"),
        Err(e) => println!("feature 0x12 falhou: {e}"),
    }
    // BT: o serial do hidapi costuma ser o MAC.
    println!("serial (hidapi): {:?}", info.serial_number());
    // Pedir o feature 0x05 ativa o report completo 0x11 no Bluetooth.
    let mut f5 = [0u8; 64];
    f5[0] = 0x05;
    match dev.get_feature_report(&mut f5) {
        Ok(n) => println!("feature 0x05 (ativa modo completo no BT): {n} bytes"),
        Err(e) => println!("feature 0x05 falhou: {e}"),
    }
}

fn read(api: &hidapi::HidApi, secs: u64) {
    let Some(info) = find_ds4(api) else {
        println!("nenhum DS4 encontrado");
        return;
    };
    println!(
        "DS4: VID {:04X} PID {:04X} bus {:?}",
        info.vendor_id(),
        info.product_id(),
        info.bus_type()
    );
    let dev = info.open_device(api).expect("abrir DS4");
    read_mac(&dev, info);
    let mut buf = [0u8; 128];
    let end = Instant::now() + Duration::from_secs(secs);
    let (mut n, mut last) = (0u32, None);
    let mut kinds = std::collections::BTreeMap::new();
    while Instant::now() < end {
        if let Ok(len) = dev.read_timeout(&mut buf, 100)
            && len > 0
        {
            n += 1;
            *kinds.entry(buf[0]).or_insert(0u32) += 1;
            if let Some((s, _)) = parse(&buf[..len]) {
                last = Some(s);
            }
        }
    }
    println!(
        "{n} reports em {secs}s = {:.0} Hz; ids: {kinds:02X?}",
        n as f64 / secs as f64
    );
    println!("último estado: {last:?}");
}

fn axis(v: u8, invert: bool) -> i16 {
    let f = (v as f32 - 127.5) / 127.5;
    let f = if invert { -f } else { f };
    (f.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn to_x360(s: &Ds4State) -> vigem_client::XGamepad {
    let mut raw = 0u16;
    // d-pad: nibble baixo do byte 5 (0=N … 7=NO, 8=solto)
    raw |= match s.b5 & 0x0F {
        0 => 0x0001,
        1 => 0x0001 | 0x0008,
        2 => 0x0008,
        3 => 0x0002 | 0x0008,
        4 => 0x0002,
        5 => 0x0002 | 0x0004,
        6 => 0x0004,
        7 => 0x0001 | 0x0004,
        _ => 0,
    };
    let bit = |byte: u8, mask: u8, x: u16| if byte & mask != 0 { x } else { 0 };
    raw |= bit(s.b5, 0x10, 0x4000); // quadrado → X
    raw |= bit(s.b5, 0x20, 0x1000); // xis → A
    raw |= bit(s.b5, 0x40, 0x2000); // círculo → B
    raw |= bit(s.b5, 0x80, 0x8000); // triângulo → Y
    raw |= bit(s.b6, 0x01, 0x0100); // L1
    raw |= bit(s.b6, 0x02, 0x0200); // R1
    raw |= bit(s.b6, 0x10, 0x0020); // share → back
    raw |= bit(s.b6, 0x20, 0x0010); // options → start
    raw |= bit(s.b6, 0x40, 0x0040); // L3
    raw |= bit(s.b6, 0x80, 0x0080); // R3
    raw |= bit(s.b7, 0x01, 0x0400); // PS → guide
    vigem_client::XGamepad {
        buttons: vigem_client::XButtons { raw },
        left_trigger: s.l2,
        right_trigger: s.r2,
        thumb_lx: axis(s.lx, false),
        thumb_ly: axis(s.ly, true),
        thumb_rx: axis(s.rx, false),
        thumb_ry: axis(s.ry, true),
    }
}

fn xinput_slots() -> Vec<u32> {
    use windows_sys::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};
    (0..4)
        .filter(|&i| {
            let mut st: XINPUT_STATE = unsafe { std::mem::zeroed() };
            unsafe { XInputGetState(i, &mut st) == 0 }
        })
        .collect()
}

fn bridge(api: &hidapi::HidApi, secs: u64) {
    let Some(info) = find_ds4(api) else {
        println!("nenhum DS4 encontrado");
        return;
    };
    let dev = info.open_device(api).expect("abrir DS4");
    read_mac(&dev, info);

    println!("slots XInput antes: {:?}", xinput_slots());
    let client = vigem_client::Client::connect().expect("conectar ao ViGEmBus");
    let mut target =
        vigem_client::Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
    target.plugin().expect("plugin X360");
    target.wait_ready().expect("X360 pronto");
    std::thread::sleep(Duration::from_millis(300));
    println!("slots XInput depois: {:?}", xinput_slots());
    println!("espelhando DS4 → Xbox 360 por {secs}s (abra joy.cpl para conferir)...");

    let mut buf = [0u8; 128];
    let mut lat_ns: Vec<u64> = Vec::with_capacity(20_000);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Ok(len) = dev.read_timeout(&mut buf, 100)
            && len > 0
        {
            let t = Instant::now();
            if let Some((s, _)) = parse(&buf[..len]) {
                target.update(&to_x360(&s)).expect("update X360");
                lat_ns.push(t.elapsed().as_nanos() as u64);
            }
        }
    }
    // Confere de ponta a ponta: o que o XInput (jogo) vê vs. o último report do DS4.
    std::thread::sleep(Duration::from_millis(50));
    if let Some((s, _)) = parse(&buf) {
        use windows_sys::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};
        let expected = to_x360(&s);
        for slot in xinput_slots() {
            let mut st: XINPUT_STATE = unsafe { std::mem::zeroed() };
            unsafe { XInputGetState(slot, &mut st) };
            let g = st.Gamepad;
            println!(
                "XInput slot {slot}: LX {} LY {} RX {} RY {} LT {} RT {} botões {:04X} | esperado LX {} LY {} RX {} RY {} LT {} RT {} botões {:04X}",
                g.sThumbLX, g.sThumbLY, g.sThumbRX, g.sThumbRY, g.bLeftTrigger, g.bRightTrigger, g.wButtons,
                expected.thumb_lx, expected.thumb_ly, expected.thumb_rx, expected.thumb_ry,
                expected.left_trigger, expected.right_trigger, expected.buttons.raw
            );
        }
    }
    target.unplug().ok();
    lat_ns.sort_unstable();
    let pct = |p: f64| lat_ns[((lat_ns.len() as f64 - 1.0) * p) as usize] as f64 / 1000.0;
    if lat_ns.is_empty() {
        println!("nenhum report recebido");
    } else {
        println!(
            "{} pacotes; latência parse+envio (µs): p50 {:.1} p99 {:.1} máx {:.1}",
            lat_ns.len(),
            pct(0.5),
            pct(0.99),
            pct(1.0)
        );
    }
}

fn hide(api: &hidapi::HidApi, on: bool) {
    let Some(info) = find_ds4(api) else {
        println!("nenhum DS4 encontrado");
        return;
    };
    let instance = hidhide::instance_id_from_path(&info.path().to_string_lossy());
    println!("instance id: {instance}");
    if on {
        hidhide::hide_device(&instance);
    } else {
        hidhide::unhide_device(&instance);
    }
}
