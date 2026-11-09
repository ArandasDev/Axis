# Axis, plano 3: `axis-devices` (drivers de controle)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** criar o crate `axis-devices`, que descobre, identifica, lê e escreve nos controles físicos: DualShock 4 (verificado em hardware), DualSense e Xbox (experimentais), com detecção de conexão por evento e gravação/replay de pacotes.

**Architecture:** a decodificação e a montagem de pacotes de cada família são funções puras (`ds4.rs`, `dualsense.rs`), testadas com pacotes sintéticos e gravados. Uma camada fina de I/O (`hid.rs` com hidapi, `xinput.rs`, `hotplug.rs`) implementa a trait `Controller`, que o daemon (plano 4) vai usar com uma thread bloqueante por controle. Nada de runtime assíncrono.

**Tech Stack:** Rust 1.99 (edition 2024), `axis-core`, `hidapi` 2, `windows-sys` 0.61, `thiserror` 2.

**Spec:** `docs/superpowers/specs/2026-11-09-axis-design.md` (seções 3, 4, 4.1.1, 5.3, 10, 11). Achados dos spikes: `docs/spikes/fase-0-a-vigem-hidhide.md`.

## Global Constraints

- Edition 2024, workspace em `crates/*`. Plataforma alvo: Windows (x64). Build fora do OneDrive (`C:/dev/cargo-target/axis`, já configurado).
- Dependências de runtime do `axis-devices`: só `axis-core`, `hidapi = "2"`, `windows-sys = "0.61"`, `thiserror`. Sem runtime assíncrono.
- **Decodificar um pacote de entrada não aloca memória** (`Report` é `Copy`, tamanho fixo). Montar pacotes de saída pode alocar (`Vec<u8>`), porque é raro.
- Convenção de saída (do `axis-core`): eixos em [−1, 1] com **+y para cima**; gatilhos em [0, 1]; `Button` abstrato (South = ✕/A, East = ○/B, West = □/X, North = △/Y, Start = Options, Select = Share/Create, Guide = PS, Touchpad = clique do touchpad).
- Normalização de eixo u8: `(v − 127.5) / 127.5`, limitado a [−1, 1]; eixo Y invertido (255 = baixo = −1). Gatilho u8: `v / 255`.
- Identidade: MAC em minúsculas `aa:bb:cc:dd:ee:ff` (mesmo formato de `ControllerRecord.id`).
- Detecção de conexão por **notificação do Windows**, nunca por polling. A única exceção é o XInput (experimental), que só tem API por polling: ele consulta o estado apenas enquanto há um controle Xbox conectado.
- DualSense e Xbox são **experimentais**: testes de hardware marcados `#[ignore]` e listados em `docs/hardware-checklist.md` (spec 4.1.1).
- Testes que dependem de hardware: `#[ignore = "hardware: …"]`, rodados com `cargo test -p axis-devices -- --ignored`.
- Git: Conventional Commits, branch `feat/axis-devices`, autor único, **sem trailers `Co-Authored-By` nem linhas de atribuição**, sem PR (merge local `--no-ff` na `main` e push).
- Identificadores no código em inglês. Docs e mensagens de commit podem ser em português.

## Review Focus

1. **Pacote truncado ou de tamanho inesperado** (report reduzido no Bluetooth, leitura parcial): `parse` retorna `None` e nunca entra em pânico nem lê fora do buffer. Testes nas Tasks 3 e 5.
2. **ID de report desconhecido** (`0x00`, `0x12`, lixo): `None`. Testes nas Tasks 3 e 5.
3. **Clone sem MAC acessível** (feature report falha e o serial vem vazio ou fora do formato): a identidade cai num ID estável derivado do instance ID, nunca vazio e nunca igual entre dois controles diferentes. Teste na Task 2.
4. **Valores fora da faixa vindos de clones** (nível de bateria 12–15, d-pad 9–15): bateria limitada a 100%, d-pad tratado como solto. Testes nas Tasks 3 e 5.
5. **O próprio Xbox 360 virtual do Axis aparecendo no XInput** (laço de realimentação): os slots dos controles virtuais do Axis são excluídos da enumeração do Xbox. Teste na Task 7.

---

## Estrutura de arquivos

```
crates/axis-devices/
├─ Cargo.toml
├─ src/
│  ├─ lib.rs            módulos + re-exports
│  ├─ types.rs          Transport, DeviceStatus, Report, OutputState, DeviceError
│  ├─ identity.rs       format_mac, parse_serial_mac, fallback_id, resolve_identity
│  ├─ crc.rs            crc32 (IEEE) para pacotes Bluetooth
│  ├─ classify.rs       classify(): VID/PID/usage/produto/barramento → família + transporte
│  ├─ ds4.rs            parse, output_report, mac_from_feature_0x12
│  ├─ dualsense.rs      parse, output_report, player_leds, mac_from_feature_0x09
│  ├─ controller.rs     trait Controller
│  ├─ hid.rs            HidController (hidapi): enumerate, open, read, write_output
│  ├─ xinput.rs         XInputController (experimental)
│  ├─ hotplug.rs        HotplugWatcher (CM_Register_Notification)
│  └─ recording.rs      Recording: gravar e ler sessões de pacotes crus
├─ examples/
│  ├─ watch.rs          imprime chegadas/remoções e o estado dos controles
│  └─ record.rs         grava N segundos do primeiro controle em um .rec
└─ tests/
   ├─ replay.rs         reproduz fixtures gravados pelo parse
   └─ fixtures/         *.rec (sintéticos e, na Task 10, reais)
docs/hardware-checklist.md
```

---

### Task 1: crate, tipos comuns e CRC32

**Files:**
- Create: `crates/axis-devices/Cargo.toml`, `src/lib.rs`, `src/types.rs`, `src/crc.rs`

**Interfaces:**
- Consumes: `axis_core::{InputState, DeviceFamily}`.
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum Transport { Usb, Bluetooth }`
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub struct DeviceStatus { pub battery_percent: Option<u8>, pub charging: bool }`
  - `#[derive(Clone, Copy, Debug, Default, PartialEq)] pub struct Report { pub input: InputState, pub status: DeviceStatus }`
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub struct OutputState { pub lightbar: [u8; 3], pub rumble_strong: u8, pub rumble_weak: u8, pub player: Option<u8> }` (`player` = 1..=4)
  - `#[derive(Debug, thiserror::Error)] pub enum DeviceError { #[error("dispositivo desconectado")] Disconnected, #[error("erro de HID: {0}")] Hid(String), #[error("não suportado: {0}")] Unsupported(&'static str) }`
  - `pub fn crc32(data: &[u8]) -> u32` (IEEE, refletido, polinômio `0xEDB88320`, init `0xFFFFFFFF`, xor final `0xFFFFFFFF`)
  - `pub(crate) fn crc32_bt_output(seed: u8, body: &[u8]) -> u32`: CRC de `[seed] ++ body`, sem alocar (alimenta o seed e depois o corpo).

- [ ] **Step 1: criar a branch e o crate**

```bash
git switch main && git pull && git switch -c feat/axis-devices
```

`crates/axis-devices/Cargo.toml`: herda edition/rust-version; deps `axis-core = { path = "../axis-core" }`, `hidapi = "2"`, `thiserror = { workspace = true }`, `windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_UI_Input_XboxController", "Win32_Devices_DeviceAndDriverInstallation"] }`. Adicionar `hidapi` e `windows-sys` em `[workspace.dependencies]` e referenciar com `workspace = true`.

- [ ] **Step 2: testes que falham** (`crc.rs`)

```rust
#[test]
fn crc32_matches_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn bt_output_crc_prefixes_the_seed() {
    let body = [0x11u8, 0x80, 0x00, 0xFF];
    assert_eq!(crc32_bt_output(0xA2, &body), crc32(&[0xA2, 0x11, 0x80, 0x00, 0xFF]));
}
```

- [ ] **Step 3: rodar e confirmar que falha**

Run: `cargo test -p axis-devices`
Expected: erro de compilação (`crc32` não definido).

- [ ] **Step 4: implementar `types.rs` e `crc.rs`; re-exportar tudo em `lib.rs`**

- [ ] **Step 5: rodar e confirmar que passa**

Run: `cargo test -p axis-devices`
Expected: 2 testes passam; `cargo clippy -p axis-devices --all-targets -- -D warnings` limpo.

- [ ] **Step 6: commit**

```bash
git add Cargo.toml Cargo.lock crates/axis-devices
git commit -m "feat(devices): criar crate axis-devices com tipos comuns e CRC32"
```

---

### Task 2: identidade e classificação de dispositivos

**Files:**
- Create: `crates/axis-devices/src/identity.rs`, `crates/axis-devices/src/classify.rs`

**Interfaces:**
- Consumes: `Transport` (Task 1), `axis_core::DeviceFamily`.
- Produces:
  - `pub fn format_mac(mac: [u8; 6]) -> String` → `"a4:ae:12:00:be:ef"`.
  - `pub fn parse_serial_mac(serial: &str) -> Option<[u8; 6]>`: aceita 12 dígitos hex com ou sem separadores `:`/`-`, maiúsculas ou minúsculas; qualquer outra coisa → `None`. Um MAC todo zero → `None`.
  - `pub fn fallback_id(instance_path: &str) -> String`: `"dev-"` + FNV-1a 64 bits, em hex de 16 dígitos minúsculos, do path em maiúsculas. Determinístico.
  - `pub fn resolve_identity(feature_mac: Option<[u8; 6]>, serial: Option<&str>, instance_path: &str) -> String`: prioridade: MAC do feature report → MAC do serial → `fallback_id`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct HidInfo<'a> { pub vid: u16, pub pid: u16, pub usage_page: u16, pub usage: u16, pub product: Option<&'a str>, pub bluetooth: bool }`
  - `pub fn classify(info: &HidInfo) -> Option<(DeviceFamily, Transport)>`:
    - Sony `0x054C` + PID `0x05C4`, `0x09CC` ou `0x0BA0` → `DualShock4`.
    - Sony `0x054C` + PID `0x0CE6` ou `0x0DF2` → `DualSense`.
    - Qualquer VID com usage `0x01:0x05` (gamepad) e produto exatamente `"Wireless Controller"` → `DualShock4` (clones).
    - Todo o resto, inclusive controles Xbox, → `None` (o Xbox entra pelo XInput, Task 7).
    - O transporte vem de `bluetooth`.

- [ ] **Step 1: testes que falham**

```rust
// identity.rs
#[test]
fn mac_formats_lowercase_with_colons() {
    assert_eq!(format_mac([0x41, 0x42, 0x00, 0x00, 0x84, 0xB6]), "41:42:00:00:84:b6");
}

#[test]
fn serial_mac_accepts_bare_and_separated_forms() {
    let m = Some([0x41, 0x42, 0x00, 0x00, 0x84, 0xB6]);
    assert_eq!(parse_serial_mac("4142000084b6"), m);
    assert_eq!(parse_serial_mac("41:42:00:00:84:B6"), m);
    assert_eq!(parse_serial_mac("41-42-00-00-84-b6"), m);
    assert_eq!(parse_serial_mac(""), None);
    assert_eq!(parse_serial_mac("000000000000"), None);
    assert_eq!(parse_serial_mac("Wireless 1234"), None);
}

#[test]
fn identity_prefers_feature_then_serial_then_fallback() {
    let p = r"\\?\HID#VID_054C&PID_09CC#7&ABC&0&0000#{4d1e55b2}";
    assert_eq!(resolve_identity(Some([1, 2, 3, 4, 5, 6]), Some("4142000084b6"), p), "01:02:03:04:05:06");
    assert_eq!(resolve_identity(None, Some("4142000084b6"), p), "41:42:00:00:84:b6");
    let f = resolve_identity(None, Some(""), p);
    assert!(f.starts_with("dev-") && f.len() == 20);
    assert_eq!(f, resolve_identity(None, None, p));
    assert_ne!(f, resolve_identity(None, None, r"\\?\HID#VID_054C&PID_09CC#7&DEF&0&0000#{4d1e55b2}"));
}

// classify.rs
fn info(vid: u16, pid: u16, product: Option<&str>, bluetooth: bool) -> HidInfo<'_> {
    HidInfo { vid, pid, usage_page: 0x01, usage: 0x05, product, bluetooth }
}

#[test]
fn sony_ids_are_classified() {
    assert_eq!(classify(&info(0x054C, 0x09CC, None, true)), Some((DeviceFamily::DualShock4, Transport::Bluetooth)));
    assert_eq!(classify(&info(0x054C, 0x05C4, None, false)), Some((DeviceFamily::DualShock4, Transport::Usb)));
    assert_eq!(classify(&info(0x054C, 0x0CE6, None, false)), Some((DeviceFamily::DualSense, Transport::Usb)));
}

#[test]
fn clone_named_wireless_controller_is_ds4() {
    assert_eq!(classify(&info(0x7545, 0x0104, Some("Wireless Controller"), true)), Some((DeviceFamily::DualShock4, Transport::Bluetooth)));
}

#[test]
fn unrelated_devices_are_ignored() {
    assert_eq!(classify(&info(0x045E, 0x028E, Some("Controller"), false)), None);
    let mut keyboard = info(0x054C, 0x09CC, None, false);
    keyboard.usage = 0x06;
    assert_eq!(classify(&HidInfo { vid: 0x1234, ..keyboard }), None);
}
```

O último caso garante que um usage diferente de gamepad (`0x06`) só é aceito quando o VID/PID é da Sony; com outro VID, é ignorado.

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices`
Expected: erro de compilação.

- [ ] **Step 3: implementar `identity.rs` e `classify.rs`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices`
Expected: todos os testes passam; clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices/src
git commit -m "feat(devices): identidade por MAC com fallback estável e classificação de dispositivos"
```

---

### Task 3: DualShock 4, entrada

**Files:**
- Create: `crates/axis-devices/src/ds4.rs`

**Interfaces:**
- Consumes: `Report`, `DeviceStatus` (Task 1), `axis_core::{InputState, Vec2, Button, Buttons}`.
- Produces:
  - `pub fn parse(buf: &[u8]) -> Option<Report>`
  - `pub fn mac_from_feature_0x12(buf: &[u8]) -> Option<[u8; 6]>`: `buf[0] == 0x12` e `buf.len() >= 7`; MAC = `buf[1..7]` **invertido**; MAC todo zero → `None`.
  - `pub const FEATURE_ENABLE_FULL_BT: u8 = 0x05;` (ler este feature report coloca o DS4 no modo completo `0x11` no Bluetooth; achado do spike)
  - `pub(crate) fn axis_u8(v: u8, invert: bool) -> f32` e `pub(crate) fn trigger_u8(v: u8) -> f32`, reutilizadas pelo DualSense.

Layout (a partir do início dos dados `d`):
- Report `0x01` (USB, e reduzido no BT): `d = buf[1..]`, exige `buf.len() >= 10`. Bateria só quando `buf.len() >= 31` (o reduzido do BT tem 10 bytes e não traz bateria → `battery_percent: None`, `charging: false`).
- Report `0x11` (BT completo): `d = buf[3..]`, exige `buf.len() >= 33`.
- `d[0]` LX, `d[1]` LY, `d[2]` RX, `d[3]` RY.
- `d[4]`: nibble baixo = d-pad (0 = cima, 1 = cima+direita, 2 = direita, 3 = baixo+direita, 4 = baixo, 5 = baixo+esquerda, 6 = esquerda, 7 = cima+esquerda, 8..=15 = solto); bit `0x10` □ (West), `0x20` ✕ (South), `0x40` ○ (East), `0x80` △ (North).
- `d[5]`: `0x01` L1, `0x02` R1, `0x10` Share (Select), `0x20` Options (Start), `0x40` L3, `0x80` R3. (`0x04`/`0x08` são L2/R2 digitais, ignorados.)
- `d[6]`: `0x01` PS (Guide), `0x02` touchpad (Touchpad); bits 2..7 = contador (ignorar).
- `d[7]` L2, `d[8]` R2.
- `d[29]`: nível = nibble baixo; bit `0x10` = cabo conectado. `battery_percent = min(nível × 10, 100)`; `charging = cabo && nível < 11`.

- [ ] **Step 1: testes que falham** (com um construtor de pacote sintético no módulo de teste)

```rust
/// Monta um report USB 0x01 de 64 bytes em repouso (sticks 128, d-pad solto).
fn usb_rest() -> [u8; 64] {
    let mut b = [0u8; 64];
    b[0] = 0x01; b[1] = 128; b[2] = 128; b[3] = 128; b[4] = 128; b[5] = 0x08;
    b
}

#[test]
fn rest_report_is_centered_and_released() {
    let r = parse(&usb_rest()).unwrap();
    assert!(r.input.left.x.abs() < 0.01 && r.input.left.y.abs() < 0.01);
    assert_eq!(r.input.buttons.0, 0);
    assert_eq!(r.input.l2, 0.0);
}

#[test]
fn stick_down_is_negative_y_and_right_is_positive_x() {
    let mut b = usb_rest();
    b[1] = 255; b[2] = 255;
    let r = parse(&b).unwrap();
    assert_eq!(r.input.left.x, 1.0);
    assert_eq!(r.input.left.y, -1.0);
}

#[test]
fn face_buttons_dpad_and_triggers_map_to_abstract_buttons() {
    let mut b = usb_rest();
    b[5] = 0x20 | 0x02;        // ✕ + d-pad direita
    b[6] = 0x01 | 0x20;        // L1 + Options
    b[7] = 0x01 | 0x02;        // PS + touchpad
    b[8] = 255; b[9] = 51;     // L2 cheio, R2 20%
    let r = parse(&b).unwrap();
    for btn in [Button::South, Button::DpadRight, Button::L1, Button::Start, Button::Guide, Button::Touchpad] {
        assert!(r.input.buttons.is_pressed(btn), "{btn:?}");
    }
    assert!(!r.input.buttons.is_pressed(Button::DpadUp));
    assert_eq!(r.input.l2, 1.0);
    assert!((r.input.r2 - 0.2).abs() < 1e-6);
}

#[test]
fn diagonal_dpad_sets_two_directions_and_out_of_range_is_released() {
    let mut b = usb_rest();
    b[5] = 0x07;               // NO
    let r = parse(&b).unwrap();
    assert!(r.input.buttons.is_pressed(Button::DpadUp) && r.input.buttons.is_pressed(Button::DpadLeft));
    b[5] = 0x0B;               // valor inválido de clone
    assert_eq!(parse(&b).unwrap().input.buttons.0, 0);
}

#[test]
fn battery_and_charging_are_decoded_and_clamped() {
    let mut b = usb_rest();
    b[30] = 0x10 | 0x05;       // cabo, nível 5
    let s = parse(&b).unwrap().status;
    assert_eq!(s, DeviceStatus { battery_percent: Some(50), charging: true });
    b[30] = 0x10 | 0x0F;       // nível fora da faixa
    assert_eq!(parse(&b).unwrap().status, DeviceStatus { battery_percent: Some(100), charging: false });
}

#[test]
fn bluetooth_full_report_uses_offset_three() {
    let mut b = [0u8; 78];
    b[0] = 0x11; b[3] = 255; b[4] = 0; b[5] = 128; b[6] = 128; b[7] = 0x08;
    let r = parse(&b).unwrap();
    assert_eq!(r.input.left.x, 1.0);
    assert_eq!(r.input.left.y, 1.0);
}

#[test]
fn bluetooth_reduced_report_has_no_battery() {
    let mut b = [0u8; 10];
    b[0] = 0x01; b[1] = 128; b[2] = 128; b[3] = 128; b[4] = 128; b[5] = 0x08;
    assert_eq!(parse(&b).unwrap().status.battery_percent, None);
}

#[test]
fn short_or_unknown_reports_are_rejected() {
    assert!(parse(&[]).is_none());
    assert!(parse(&[0x01, 128, 128]).is_none());
    assert!(parse(&[0x11; 20]).is_none());
    let mut b = usb_rest();
    b[0] = 0x12;
    assert!(parse(&b).is_none());
}

#[test]
fn mac_from_usb_feature_is_reversed() {
    let mut f = [0u8; 16];
    f[0] = 0x12;
    f[1..7].copy_from_slice(&[0xB6, 0x84, 0x00, 0x00, 0x42, 0x41]);
    assert_eq!(mac_from_feature_0x12(&f), Some([0x41, 0x42, 0x00, 0x00, 0x84, 0xB6]));
    assert_eq!(mac_from_feature_0x12(&[0x12, 0, 0, 0, 0, 0, 0]), None);
    assert_eq!(mac_from_feature_0x12(&[0x12, 1, 2]), None);
}
```

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices ds4`
Expected: erro de compilação.

- [ ] **Step 3: implementar `ds4::parse`, `mac_from_feature_0x12`, `axis_u8`, `trigger_u8`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices`
Expected: todos passam; clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices/src
git commit -m "feat(devices): decodificar reports de entrada do DualShock 4 (USB e Bluetooth)"
```

---

### Task 4: DualShock 4, saída (lightbar e vibração)

**Files:**
- Modify: `crates/axis-devices/src/ds4.rs`

**Interfaces:**
- Consumes: `OutputState`, `Transport`, `crc32_bt_output` (Task 1).
- Produces: `pub fn output_report(transport: Transport, out: &OutputState) -> Vec<u8>`
  - USB: 32 bytes; `[0] = 0x05`, `[1] = 0xFF`, `[4] = rumble_weak`, `[5] = rumble_strong`, `[6..9] = lightbar` (R, G, B); resto zero.
  - Bluetooth: 78 bytes; `[0] = 0x11`, `[1] = 0x80`, `[3] = 0xFF`, `[6] = rumble_weak`, `[7] = rumble_strong`, `[8..11] = lightbar`; `[74..78]` = `crc32_bt_output(0xA2, &buf[..74])` em little-endian.
  - O DS4 não tem LEDs de jogador: `player` é ignorado.

- [ ] **Step 1: testes que falham**

```rust
#[test]
fn usb_output_places_rumble_and_color() {
    let out = OutputState { lightbar: [95, 179, 122], rumble_strong: 200, rumble_weak: 50, player: Some(2) };
    let r = output_report(Transport::Usb, &out);
    assert_eq!(r.len(), 32);
    assert_eq!(&r[..2], &[0x05, 0xFF]);
    assert_eq!((r[4], r[5]), (50, 200));
    assert_eq!(&r[6..9], &[95, 179, 122]);
}

#[test]
fn bluetooth_output_has_valid_crc() {
    let out = OutputState { lightbar: [155, 122, 216], ..Default::default() };
    let r = output_report(Transport::Bluetooth, &out);
    assert_eq!(r.len(), 78);
    assert_eq!((r[0], r[1], r[3]), (0x11, 0x80, 0xFF));
    assert_eq!(&r[8..11], &[155, 122, 216]);
    let crc = u32::from_le_bytes(r[74..78].try_into().unwrap());
    assert_eq!(crc, crc32_bt_output(0xA2, &r[..74]));
}
```

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices output`
Expected: erro de compilação.

- [ ] **Step 3: implementar `ds4::output_report`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices`
Expected: todos passam; clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices/src/ds4.rs
git commit -m "feat(devices): montar report de saída do DualShock 4 (lightbar e vibração)"
```

---

### Task 5: DualSense (experimental), entrada e saída

**Files:**
- Create: `crates/axis-devices/src/dualsense.rs`

**Interfaces:**
- Consumes: `Report`, `DeviceStatus`, `OutputState`, `Transport`, `crc32_bt_output` (Task 1); `ds4::axis_u8`, `ds4::trigger_u8` (Task 3).
- Produces:
  - `pub fn parse(buf: &[u8]) -> Option<Report>`: report `0x01` (USB, `d = buf[1..]`, exige `len >= 64`) ou `0x31` (BT, `d = buf[2..]`, exige `len >= 78`). `0x01` com menos de 64 bytes → `None`.
    - `d[0]` LX, `d[1]` LY, `d[2]` RX, `d[3]` RY, `d[4]` L2, `d[5]` R2, `d[6]` contador.
    - `d[7]`: d-pad no nibble baixo (mesma codificação do DS4); `0x10` □, `0x20` ✕, `0x40` ○, `0x80` △.
    - `d[8]`: `0x01` L1, `0x02` R1, `0x10` Create (Select), `0x20` Options (Start), `0x40` L3, `0x80` R3.
    - `d[9]`: `0x01` PS (Guide), `0x02` touchpad (Touchpad). O botão de mudo não tem `Button` e é ignorado.
    - `d[52]`: nível = nibble baixo (0..=10), estado = nibble alto (`0` descarregando, `1` carregando, `2` cheio). `battery_percent = min(nível × 10, 100)`; `charging = estado == 1`.
  - `pub fn player_leds(player: u8) -> u8`: 1 → `0b00100`, 2 → `0b01010`, 3 → `0b10101`, 4 → `0b11011`, outro → `0`.
  - `pub fn output_report(transport: Transport, out: &OutputState) -> Vec<u8>`: carga útil `p` (47 bytes) com `p[0] = 0x03` (vibração compatível), `p[1] = 0x14` (lightbar + LEDs de jogador), `p[2] = rumble_weak`, `p[3] = rumble_strong`, `p[43] = player_leds(player)` (ou 0), `p[44..47] = lightbar`.
    - USB: `[0x02] ++ p` (48 bytes).
    - Bluetooth: 78 bytes, `[0] = 0x31`, `[1] = 0x02`, `p` a partir do byte 2, CRC `crc32_bt_output(0xA2, &buf[..74])` em `[74..78]` little-endian.
  - `pub fn mac_from_feature_0x09(buf: &[u8]) -> Option<[u8; 6]>`: `buf[0] == 0x09`, `len >= 7`, MAC = `buf[1..7]` invertido, todo zero → `None`.
  - Doc comment no topo: "Experimental: layout da documentação pública; sem verificação em hardware (spec 4.1.1)."

- [ ] **Step 1: testes que falham** (pacotes sintéticos)

```rust
fn usb_rest() -> [u8; 64] {
    let mut b = [0u8; 64];
    b[0] = 0x01; b[1] = 128; b[2] = 128; b[3] = 128; b[4] = 128; b[8] = 0x08;
    b
}

#[test]
fn usb_report_maps_axes_triggers_and_buttons() {
    let mut b = usb_rest();
    b[1] = 255; b[5] = 255; b[6] = 0;
    b[8] = 0x80 | 0x04;        // △ + d-pad baixo
    b[9] = 0x02 | 0x10;        // R1 + Create
    b[10] = 0x01;              // PS
    let r = parse(&b).unwrap();
    assert_eq!(r.input.left.x, 1.0);
    assert_eq!((r.input.l2, r.input.r2), (1.0, 0.0));
    for btn in [Button::North, Button::DpadDown, Button::R1, Button::Select, Button::Guide] {
        assert!(r.input.buttons.is_pressed(btn), "{btn:?}");
    }
}

#[test]
fn bluetooth_report_uses_offset_two() {
    let mut b = [0u8; 78];
    b[0] = 0x31; b[2] = 0; b[3] = 128; b[4] = 128; b[5] = 128; b[9] = 0x08;
    assert_eq!(parse(&b).unwrap().input.left.x, -1.0);
}

#[test]
fn battery_state_is_decoded() {
    let mut b = usb_rest();
    b[53] = 0x10 | 0x07;
    assert_eq!(parse(&b).unwrap().status, DeviceStatus { battery_percent: Some(70), charging: true });
}

#[test]
fn short_or_unknown_reports_are_rejected() {
    assert!(parse(&[0x01; 10]).is_none());
    assert!(parse(&[0x31; 40]).is_none());
    assert!(parse(&[0x05; 64]).is_none());
}

#[test]
fn player_led_patterns() {
    assert_eq!([1, 2, 3, 4, 0, 5].map(player_leds), [0b00100, 0b01010, 0b10101, 0b11011, 0, 0]);
}

#[test]
fn outputs_place_payload_and_crc() {
    let out = OutputState { lightbar: [1, 2, 3], rumble_strong: 9, rumble_weak: 8, player: Some(2) };
    let u = output_report(Transport::Usb, &out);
    assert_eq!(u.len(), 48);
    assert_eq!((u[0], u[1], u[2], u[3], u[4]), (0x02, 0x03, 0x14, 8, 9));
    assert_eq!((u[44], &u[45..48]), (0b01010, &[1u8, 2, 3][..]));
    let b = output_report(Transport::Bluetooth, &out);
    assert_eq!((b.len(), b[0], b[1], b[2]), (78, 0x31, 0x02, 0x03));
    assert_eq!(&b[46..49], &[1, 2, 3]);
    assert_eq!(u32::from_le_bytes(b[74..78].try_into().unwrap()), crc32_bt_output(0xA2, &b[..74]));
}

#[test]
fn mac_from_feature_0x09_is_reversed() {
    let mut f = [0u8; 20];
    f[0] = 0x09;
    f[1..7].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
    assert_eq!(mac_from_feature_0x09(&f), Some([1, 2, 3, 4, 5, 6]));
}
```

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices dualsense`
Expected: erro de compilação.

- [ ] **Step 3: implementar `dualsense.rs`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices`
Expected: todos passam; clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices/src
git commit -m "feat(devices): DualSense experimental (entrada, saída, LEDs de jogador)"
```

---

### Task 6: trait `Controller` e `HidController`

**Files:**
- Create: `crates/axis-devices/src/controller.rs`, `crates/axis-devices/src/hid.rs`

**Interfaces:**
- Consumes: tudo das Tasks 1 a 5.
- Produces:
  - ```rust
    pub trait Controller: Send {
        fn id(&self) -> &str;
        fn family(&self) -> DeviceFamily;
        fn transport(&self) -> Transport;
        fn experimental(&self) -> bool;
        /// Bloqueia até chegar um report ou `timeout_ms` passar (`Ok(None)`).
        fn read(&mut self, timeout_ms: i32) -> Result<Option<Report>, DeviceError>;
        fn write_output(&mut self, out: &OutputState) -> Result<(), DeviceError>;
    }
    ```
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct HidCandidate { pub path: String, pub family: DeviceFamily, pub transport: Transport, pub serial: Option<String> }`
  - `pub fn enumerate_hid(api: &hidapi::HidApi) -> Vec<HidCandidate>`: aplica `classify` a cada `DeviceInfo` (`bluetooth` = `bus_type() == BusType::Bluetooth`), mantém a ordem e remove paths duplicados.
  - `pub struct HidController { … }` com `pub fn open(api: &hidapi::HidApi, c: &HidCandidate) -> Result<HidController, DeviceError>`:
    - abre pelo path;
    - DS4: lê o feature `0x12` (ignora erro) e, no Bluetooth, lê o feature `FEATURE_ENABLE_FULL_BT` (`0x05`) para ativar o report `0x11`;
    - DualSense: lê o feature `0x09` (ignora erro);
    - `id = resolve_identity(feature_mac, serial, path)`;
    - `experimental = family == DualSense`.
  - `read` usa um buffer fixo de 128 bytes no struct (sem alocar), `read_timeout`, e chama o `parse` da família. Leitura de 0 bytes → `Ok(None)`. Report que não decodifica → continua lendo até o timeout acabar. Erro de hidapi → `DeviceError::Disconnected` se o dispositivo sumiu (a mensagem contém o código Win32 `0x48F` ou `0x1E`), senão `DeviceError::Hid(msg)`.
  - `write_output` monta o report da família e envia com `write`.
  - Re-exportar `Controller`, `HidController`, `HidCandidate`, `enumerate_hid` em `lib.rs`.

- [ ] **Step 1: testes de hardware que falham** (`hid.rs`, `#[cfg(test)]`)

```rust
#[test]
#[ignore = "hardware: um DS4 conectado"]
fn ds4_is_enumerated_opened_and_reads_reports() {
    let api = hidapi::HidApi::new().unwrap();
    let c = enumerate_hid(&api).into_iter().find(|c| c.family == DeviceFamily::DualShock4).expect("DS4 conectado");
    let mut ctl = HidController::open(&api, &c).unwrap();
    assert!(ctl.id().len() == 17 || ctl.id().starts_with("dev-"), "id: {}", ctl.id());
    let mut got = 0;
    for _ in 0..50 {
        if ctl.read(100).unwrap().is_some() { got += 1; }
    }
    assert!(got >= 40, "só {got} reports em 50 leituras");
}

#[test]
#[ignore = "hardware: um DS4 conectado; olhe a lightbar"]
fn ds4_lightbar_turns_green_then_back() {
    let api = hidapi::HidApi::new().unwrap();
    let c = enumerate_hid(&api).into_iter().find(|c| c.family == DeviceFamily::DualShock4).unwrap();
    let mut ctl = HidController::open(&api, &c).unwrap();
    ctl.write_output(&OutputState { lightbar: [0, 255, 0], ..Default::default() }).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    ctl.write_output(&OutputState { lightbar: [0, 0, 64], ..Default::default() }).unwrap();
}
```

Teste unitário sem hardware no mesmo arquivo:

```rust
#[test]
fn disconnect_errors_are_recognized() {
    assert!(is_disconnect_message("hidapi error: ReadFile: (0x0000048F) O dispositivo não está conectado."));
    assert!(is_disconnect_message("hidapi error: WriteFile: (0x0000001E) …"));
    assert!(!is_disconnect_message("hidapi error: (0x00000057) Parâmetro incorreto."));
}
```

com `fn is_disconnect_message(msg: &str) -> bool` privada.

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices -- --include-ignored hid`
Expected: erro de compilação.

- [ ] **Step 3: implementar `controller.rs` e `hid.rs`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices` (sem hardware: o teste de desconexão passa e os de hardware ficam ignorados). Depois, com o DS4 ligado: `cargo test -p axis-devices -- --ignored ds4_is_enumerated`.
Expected: tudo passa. Se o implementador não tiver hardware, registra no relatório que os ignorados não rodaram; o controlador roda na Task 10.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices/src
git commit -m "feat(devices): trait Controller e HidController sobre hidapi"
```

---

### Task 7: controle Xbox via XInput (experimental)

**Files:**
- Create: `crates/axis-devices/src/xinput.rs`

**Interfaces:**
- Consumes: `Controller`, `Report`, `OutputState`, `Transport`, `DeviceError`.
- Produces:
  - `pub fn available_slots(exclude: &[u32]) -> Vec<u32>`: slots 0..4 com `XInputGetState == ERROR_SUCCESS`, sem os de `exclude` (os slots ocupados pelos controles virtuais do próprio Axis).
  - `pub(crate) fn filter_slots(connected: [bool; 4], exclude: &[u32]) -> Vec<u32>`: a parte pura, testável.
  - `pub fn gamepad_to_report(g: &XINPUT_GAMEPAD) -> Report`: eixos i16 → `v / 32767` limitado a [−1, 1] (o Y do XInput já é +y para cima); gatilhos `v / 255`; botões XInput → `Button` (A → South, B → East, X → West, Y → North, LB → L1, RB → R1, LThumb → L3, RThumb → R3, Start → Start, Back → Select, d-pad → Dpad*). Guide e Touchpad não existem no XInput público; ficam soltos. `status` padrão.
  - `pub struct XInputController { slot: u32, last_packet: u32, id: String }` com `pub fn open(slot: u32) -> Result<XInputController, DeviceError>` (`id = "xinput-{slot}"`, `experimental = true`, `transport = Usb`).
  - `read(timeout_ms)`: consulta `XInputGetState` a cada 4 ms (250 Hz) até `dwPacketNumber` mudar ou o timeout acabar; `ERROR_DEVICE_NOT_CONNECTED` → `DeviceError::Disconnected`. **Este é o único polling permitido (Global Constraints).**
  - `write_output`: `XInputSetState` com `wLeftMotorSpeed = rumble_strong × 257`, `wRightMotorSpeed = rumble_weak × 257`; lightbar e `player` ignorados.

- [ ] **Step 1: testes que falham**

```rust
#[test]
fn own_virtual_slots_are_excluded() {
    assert_eq!(filter_slots([true, true, false, true], &[0]), vec![1, 3]);
    assert_eq!(filter_slots([true, false, false, false], &[0]), Vec::<u32>::new());
    assert_eq!(filter_slots([false; 4], &[]), Vec::<u32>::new());
}

#[test]
fn gamepad_maps_axes_and_buttons() {
    let g = XINPUT_GAMEPAD { wButtons: 0x1000 | 0x0001 | 0x0100, bLeftTrigger: 255, bRightTrigger: 0,
        sThumbLX: 32767, sThumbLY: -32768, sThumbRX: 0, sThumbRY: 0 };
    let r = gamepad_to_report(&g);
    assert_eq!((r.input.left.x, r.input.left.y), (1.0, -1.0));
    assert_eq!(r.input.l2, 1.0);
    for b in [Button::South, Button::DpadUp, Button::L1] { assert!(r.input.buttons.is_pressed(b)); }
}

#[test]
#[ignore = "hardware: controle Xbox conectado e nenhum virtual do Axis"]
fn xbox_controller_reads_state() {
    let slot = *available_slots(&[]).first().expect("controle Xbox");
    let mut c = XInputController::open(slot).unwrap();
    let _ = c.read(500).unwrap();
}
```

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices xinput`
Expected: erro de compilação.

- [ ] **Step 3: implementar `xinput.rs`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices`
Expected: todos passam (o de hardware fica ignorado); clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices/src
git commit -m "feat(devices): controle Xbox experimental via XInput com exclusão dos virtuais"
```

---

### Task 8: detecção de conexão e desconexão (hotplug)

**Files:**
- Create: `crates/axis-devices/src/hotplug.rs`, `crates/axis-devices/examples/watch.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub enum HotplugEvent { Arrived(String), Removed(String) }` (path do dispositivo HID).
  - `pub struct HotplugWatcher { … }` com `pub fn start(on_event: impl Fn(HotplugEvent) + Send + Sync + 'static) -> Result<HotplugWatcher, DeviceError>`. Usa `CM_Register_Notification` com `CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE` e `GUID_DEVINTERFACE_HID` = `{4D1E55B2-F16F-11CF-88CB-001111000030}`. A callback converte `CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL`/`REMOVAL` + `SymbolicLink` em `HotplugEvent`. `Drop` chama `CM_Unregister_Notification`. Nenhuma thread própria, nenhum polling: o Windows chama a callback.
  - `pub(crate) fn path_from_symbolic_link(wide: &[u16]) -> String`: converte o link UTF-16 terminado em NUL em `String`. É a parte testável.
  - `examples/watch.rs`: inicia o watcher, imprime cada evento e, em cada chegada, chama `enumerate_hid` e imprime os candidatos; roda até Ctrl+C.

- [ ] **Step 1: testes que falham**

```rust
#[test]
fn symbolic_link_is_converted_up_to_nul() {
    let w: Vec<u16> = "\\\\?\\HID#VID_054C&PID_09CC#x#{4d1e55b2}\0lixo".encode_utf16().collect();
    assert_eq!(path_from_symbolic_link(&w), "\\\\?\\HID#VID_054C&PID_09CC#x#{4d1e55b2}");
}

#[test]
#[ignore = "hardware: desligue e religue um DS4 durante 15 s"]
fn arrival_and_removal_are_reported() {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let _w = HotplugWatcher::start(move |e| { let _ = tx.lock().unwrap().send(e); }).unwrap();
    let events: Vec<HotplugEvent> = rx.iter().take(2).collect();
    assert!(events.iter().any(|e| matches!(e, HotplugEvent::Removed(_))));
    assert!(events.iter().any(|e| matches!(e, HotplugEvent::Arrived(_))));
}
```

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices hotplug`
Expected: erro de compilação.

- [ ] **Step 3: implementar `hotplug.rs` e o exemplo `watch`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices` e `cargo build -p axis-devices --examples`
Expected: testes passam (o de hardware ignorado); exemplos compilam; clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices
git commit -m "feat(devices): detectar chegada e remoção de dispositivos HID por notificação"
```

---

### Task 9: gravação e replay

**Files:**
- Create: `crates/axis-devices/src/recording.rs`, `crates/axis-devices/examples/record.rs`, `crates/axis-devices/tests/replay.rs`, `crates/axis-devices/tests/fixtures/ds4-usb-synthetic.rec`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Frame { pub t_us: u64, pub data: Vec<u8> }`
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Recording { pub family: DeviceFamily, pub transport: Transport, pub frames: Vec<Frame> }`
  - `impl Recording { pub fn to_text(&self) -> String; pub fn from_text(s: &str) -> Result<Recording, RecordingError> }`
  - Formato: primeira linha `axis-rec 1 <family> <transport>` (`DualShock4`/`DualSense`/`Xbox`, `usb`/`bluetooth`); cada linha seguinte `<t_us> <hex minúsculo sem separadores>`; linhas vazias e começando com `#` são ignoradas.
  - `#[derive(Debug, thiserror::Error, PartialEq)] pub enum RecordingError { #[error("cabeçalho inválido")] BadHeader, #[error("versão {0} não suportada")] Version(u32), #[error("linha {0} inválida")] BadLine(usize) }`
  - `pub fn parse_frame(family: DeviceFamily, data: &[u8]) -> Option<Report>`: despacha para `ds4::parse`/`dualsense::parse`; `Xbox` → `None`.
  - `examples/record.rs`: `record <segundos> <arquivo.rec>`: abre o primeiro candidato de `enumerate_hid`, lê os pacotes crus (não os decodificados) com timestamp relativo em µs e grava com `to_text`.

- [ ] **Step 1: testes que falham**

```rust
// recording.rs
#[test]
fn text_round_trip() {
    let r = Recording { family: DeviceFamily::DualShock4, transport: Transport::Bluetooth,
        frames: vec![Frame { t_us: 0, data: vec![0x11, 0xAB] }, Frame { t_us: 4995, data: vec![0x11, 0x00] }] };
    let text = r.to_text();
    assert!(text.starts_with("axis-rec 1 DualShock4 bluetooth\n"));
    assert_eq!(Recording::from_text(&text).unwrap(), r);
}

#[test]
fn bad_inputs_are_explicit_errors() {
    assert_eq!(Recording::from_text("lixo"), Err(RecordingError::BadHeader));
    assert_eq!(Recording::from_text("axis-rec 2 DualShock4 usb\n"), Err(RecordingError::Version(2)));
    assert_eq!(Recording::from_text("axis-rec 1 DualShock4 usb\n10 zz\n"), Err(RecordingError::BadLine(2)));
}

// tests/replay.rs
#[test]
fn synthetic_ds4_fixture_replays_within_ranges() {
    let rec = Recording::from_text(include_str!("fixtures/ds4-usb-synthetic.rec")).unwrap();
    assert!(rec.frames.len() >= 4);
    for f in &rec.frames {
        let r = parse_frame(rec.family, &f.data).expect("todo frame do fixture decodifica");
        for v in [r.input.left.x, r.input.left.y, r.input.right.x, r.input.right.y] { assert!((-1.0..=1.0).contains(&v)); }
        for t in [r.input.l2, r.input.r2] { assert!((0.0..=1.0).contains(&t)); }
    }
}
```

O fixture sintético tem 4 frames USB `0x01` de 64 bytes: repouso; stick esquerdo todo para a direita; ✕ apertado; L2 cheio. Começa com o comentário `# sintético — montado conforme o layout da Task 3`.

- [ ] **Step 2: rodar e confirmar que falha**

Run: `cargo test -p axis-devices`
Expected: erro de compilação.

- [ ] **Step 3: implementar `recording.rs`, o fixture e o exemplo `record`**

- [ ] **Step 4: rodar e confirmar que passa**

Run: `cargo test -p axis-devices` e `cargo build -p axis-devices --examples`
Expected: tudo passa; clippy limpo.

- [ ] **Step 5: commit**

```bash
git add crates/axis-devices
git commit -m "feat(devices): gravação e replay de sessões de pacotes crus"
```

---

### Task 10: checklist de hardware e verificação com o DS4 real

Esta tarefa é do **controlador, com o autor presente** (precisa mexer no controle). Não é delegada.

**Files:**
- Create: `docs/hardware-checklist.md`, `crates/axis-devices/tests/fixtures/ds4-bt-real.rec`
- Modify: `crates/axis-devices/tests/replay.rs`

- [ ] **Step 1: escrever `docs/hardware-checklist.md`**

Uma seção por família (DS4, DualSense, Xbox), cada uma com: comando para rodar os testes `#[ignore]`; itens verificáveis (enumera, ID estável entre reconexões, lê ≥ 40 de 50, sticks e gatilhos nos extremos, todos os botões, lightbar/LEDs de jogador, vibração, desconexão gera `Disconnected`, hotplug detecta chegada e remoção); e a linha "Estado: verificado em <data> | pendente". O DS4 começa como "Bluetooth: em verificação; USB: pendente (cabo só de carga)". DualSense e Xbox começam "pendente".

- [ ] **Step 2: rodar os testes de hardware do DS4 com o autor**

Run: `cargo test -p axis-devices -- --ignored ds4_ arrival_and_removal`
Expected: passam; o autor confirma que a lightbar ficou verde e voltou.

- [ ] **Step 3: gravar o fixture real**

Run: `cargo run -p axis-devices --example record -- 8 crates/axis-devices/tests/fixtures/ds4-bt-real.rec`, com o autor seguindo o roteiro: 2 s parado; girar os dois analógicos até a borda; apertar L2 e R2 até o fim; apertar ✕ ○ □ △; d-pad nas 4 direções.

- [ ] **Step 4: teste de replay do fixture real** (`tests/replay.rs`)

```rust
#[test]
fn real_ds4_bt_fixture_covers_extremes() {
    let rec = Recording::from_text(include_str!("fixtures/ds4-bt-real.rec")).unwrap();
    let reports: Vec<_> = rec.frames.iter().filter_map(|f| parse_frame(rec.family, &f.data)).collect();
    assert!(reports.len() * 10 >= rec.frames.len() * 9, "≥ 90% dos frames decodificam");
    assert!(reports.iter().any(|r| r.input.left.x > 0.9) && reports.iter().any(|r| r.input.left.x < -0.9));
    assert!(reports.iter().any(|r| r.input.l2 > 0.95) && reports.iter().any(|r| r.input.r2 > 0.95));
    assert!(reports.iter().any(|r| r.input.buttons.is_pressed(Button::South)));
}
```

Run: `cargo test -p axis-devices --test replay`
Expected: passa.

- [ ] **Step 5: atualizar o checklist (DS4 Bluetooth: verificado em <data>), workspace verde e commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: tudo passa.

```bash
git add docs/hardware-checklist.md crates/axis-devices/tests
git commit -m "test(devices): fixture real do DS4 por Bluetooth e checklist de hardware"
```
