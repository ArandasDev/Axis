# Axis, plano 4: `axis-virtual` + `axis-daemon` (o núcleo)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** criar o núcleo do Axis: um processo leve na bandeja que detecta os controles, aplica o pipeline de cada um, entrega um Xbox 360 virtual ao jogo, esconde o controle físico, lembra de cada controle e do seu dono, mantém o Player durante quedas curtas e inicia com o Windows.

**Architecture:** `axis-virtual` isola as dependências de sistema (ViGEmBus e HidHide) atrás das traits `VirtualBus`/`VirtualPad` e `DeviceHider`. `axis-daemon` é uma biblioteca + binário: uma thread **gerente** com um laço de eventos (`mpsc`) decide tudo; uma thread **trabalhadora** por controle faz leitura bloqueante → pipeline → `VirtualPad::update`; a bandeja é Win32 nativa numa thread própria. Toda dependência de hardware entra no gerente por trait, então a lógica (conexão, período de graça, reconexão, ocultação) é testada com implementações falsas.

**Tech Stack:** Rust 1.99 (edition 2024), `axis-core`, `axis-devices`, `vigem-client` 0.1 (feature `unstable_xtarget_notification`), `windows-sys` 0.61, `serde` + `toml`, `log` 0.4, `thiserror`.

**Spec:** `docs/superpowers/specs/2026-11-09-axis-design.md` (seções 2, 3, 5, 8.3, 9, 10). Achados: `docs/spikes/fase-0-a-vigem-hidhide.md`, `docs/hardware-checklist.md`.

## Global Constraints

- Edition 2024, workspace em `crates/*`, Windows x64, build em `C:/dev/cargo-target/axis`.
- **Sem runtime assíncrono.** Concorrência só com `std::thread` e `std::sync::mpsc`.
- Caminho crítico (thread trabalhadora): nenhuma alocação por pacote; leitura com timeout de **50 ms**; com o controle parado, a thread fica bloqueada no `read` (sem trabalho entre pacotes além de checar comandos).
- Nenhum polling no gerente nem na bandeja: tudo por evento (hotplug do Windows, mensagens de janela, canais). Temporizadores do período de graça são threads que dormem e enviam um evento.
- Ordem ao conectar (spec 5.3): **criar o virtual antes de ocultar o físico**. Ao sair, **sempre desfazer a ocultação** (spec 10).
- Controles Xbox (experimentais) **não** ganham controle virtual nem ocultação neste plano (ocultar Xbox está adiado, spec 4.1.1): o gerente só os registra para a interface. Isso evita entrada duplicada no jogo.
- Quando o XInput dos controles Xbox for ligado ao núcleo (fora deste plano), ele deve excluir os slots dos virtuais do próprio Axis e só consultar slots **depois** de criar o virtual e ler o `user_index` (laço de realimentação).
- `DeviceError::Hid` = fechar e reabrir o controle; `DeviceError::Disconnected` = entrar no período de graça.
- Bateria: ignorar a bateria de reports DS4 `0x01` vindos do Bluetooth (o Windows completa os reports até 128 bytes e daria 0% falso).
- Arquivos (spec 5.2): configuração em `%APPDATA%\Axis\` (`settings.toml`, `controllers\<id>.toml`, `profiles\<nome>.toml`); estado e logs em `%LOCALAPPDATA%\Axis\` (`hidden.toml`, `logs\`). Gravação atômica com `.bak`; arquivo corrompido renomeado para `.corrupt`.
- Metas (spec 9): núcleo ≤ 15 MB de RAM com 2 controles; CPU ~0% parado; latência adicionada p99 ≤ 1 ms.
- Git: Conventional Commits, branch `feat/axis-daemon`, autor único, **sem trailers `Co-Authored-By` nem linhas de atribuição**, sem PR (merge local `--no-ff` e push).
- Identificadores em inglês; textos para o usuário (menu, notificações, logs) em português.

## Review Focus

1. **Controle desconecta e reconecta dentro do período de graça** (Bluetooth instável): reassume o mesmo virtual e o mesmo Player; o jogo não vê desconexão. Teste na Task 8.
2. **O Axis fecha ou trava com controles ocultos:** na próxima inicialização, as entradas antigas do Axis no HidHide são removidas (reconciliação), e "Restaurar controles" limpa tudo. Testes nas Tasks 3 e 8.
3. **ViGEmBus ausente ou falhando:** o núcleo continua em "modo visualização" (lê os controles, não cria virtual, não oculta) e avisa uma vez. Teste na Task 7.
4. **Arquivo de configuração corrompido ou de versão mais nova:** usa o `.bak`; senão, padrão com o arquivo preservado como `.corrupt`; versão mais nova **não** é sobrescrita. Testes na Task 4.
5. **Dois controles com o mesmo ID ao mesmo tempo** (mesmo DS4 por USB e Bluetooth, ou dois clones com o mesmo MAC): o segundo recebe uma sessão própria com o ID sufixado `#2`, sem roubar o virtual do primeiro. Teste na Task 7.

---

## Estrutura de arquivos

```
crates/axis-virtual/
├─ Cargo.toml
└─ src/
   ├─ lib.rs          re-exports
   ├─ pad.rs          Rumble, VirtualError, trait VirtualPad, trait VirtualBus
   ├─ x360.rs         to_xgamepad (puro) + ViGEmBus / ViGEmX360
   └─ hidhide.rs      trait DeviceHider + HidHide (IOCTLs) + funções puras
crates/axis-daemon/
├─ Cargo.toml
└─ src/
   ├─ main.rs         liga tudo: logging, instância única, gerente, bandeja
   ├─ lib.rs          módulos (testáveis)
   ├─ paths.rs        AppPaths
   ├─ store.rs        gravação atômica, leitura com .bak/.corrupt/esquema
   ├─ config.rs       AppSettings, ControllerStore, ProfileStore, export/import
   ├─ worker.rs       thread por controle
   ├─ manager.rs      laço de eventos: sessões, graça, ocultação, luzes
   ├─ ports.rs        traits de porta: ControllerOpener, Scheduler, Notifier
   ├─ logging.rs      logger em arquivo com rotação, fora do caminho crítico
   ├─ instance.rs     instância única (mutex nomeado)
   ├─ autostart.rs    chave Run do usuário
   └─ tray.rs         ícone, menu e notificações (Win32)
docs/benchmarks/daemon.md
```

---

### Task 1: `axis-virtual`: traits e conversão para XInput

**Files:**
- Create: `crates/axis-virtual/Cargo.toml`, `src/lib.rs`, `src/pad.rs`, `src/x360.rs` (só a função pura nesta task)

**Interfaces:**
- Consumes: `axis_core::{InputState, Button, Buttons}`.
- Produces:
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub struct Rumble { pub strong: u8, pub weak: u8 }`
  - `#[derive(Debug, thiserror::Error)] pub enum VirtualError { #[error("ViGEmBus não encontrado")] BusNotFound, #[error("sem slot livre no ViGEmBus")] NoFreeSlot, #[error("erro do ViGEmBus: {0}")] Bus(String) }`
  - `pub trait VirtualPad: Send { fn update(&mut self, input: &InputState) -> Result<(), VirtualError>; fn user_index(&self) -> u32; }` (`Drop` desconecta o virtual)
  - `pub type RumbleSink = Box<dyn FnMut(Rumble) + Send>;`
  - `pub trait VirtualBus: Send + Sync { fn plug_x360(&self, on_rumble: RumbleSink) -> Result<Box<dyn VirtualPad>, VirtualError>; }`
  - `pub fn to_xgamepad(input: &InputState) -> vigem_client::XGamepad`: eixos `(v.clamp(-1,1) * 32767).round() as i16` (+y para cima é o mesmo sentido do XInput, sem inverter); gatilhos `(t.clamp(0,1) * 255).round() as u8`; botões: South→A `0x1000`, East→B `0x2000`, West→X `0x4000`, North→Y `0x8000`, L1→LB `0x0100`, R1→RB `0x0200`, L3 `0x0040`, R3 `0x0080`, Start `0x0010`, Select→Back `0x0020`, Guide `0x0400`, DpadUp `0x0001`, DpadDown `0x0002`, DpadLeft `0x0004`, DpadRight `0x0008`; Touchpad não tem equivalente (ignorado).
- Cargo: `vigem-client = { version = "0.1", features = ["unstable_xtarget_notification"] }`, `windows-sys` (workspace), `thiserror`, `axis-core`, `axis-devices` (para `instance id` na Task 3).

- [ ] **Step 1: criar a branch** (`git switch main && git pull && git switch -c feat/axis-daemon`) **e escrever os testes que falham** em `x360.rs`

```rust
#[test]
fn full_deflection_maps_to_extremes_with_rounding() {
    let mut i = InputState::default();
    i.left = Vec2 { x: 1.0, y: -1.0 };
    i.right = Vec2 { x: 0.5, y: 0.0 };
    i.l2 = 1.0;
    i.r2 = 0.5;
    let g = to_xgamepad(&i);
    assert_eq!((g.thumb_lx, g.thumb_ly), (32767, -32767));
    assert_eq!(g.thumb_rx, 16384);          // 16383.5 arredonda para cima
    assert_eq!((g.left_trigger, g.right_trigger), (255, 128));
}

#[test]
fn rest_is_exact_zero() {
    let g = to_xgamepad(&InputState::default());
    assert_eq!((g.thumb_lx, g.thumb_ly, g.thumb_rx, g.thumb_ry, g.left_trigger, g.right_trigger, g.buttons.raw), (0, 0, 0, 0, 0, 0, 0));
}

#[test]
fn buttons_map_to_xinput_bits_and_touchpad_is_ignored() {
    let mut i = InputState::default();
    for b in [Button::South, Button::North, Button::Select, Button::Guide, Button::DpadLeft, Button::Touchpad] {
        i.buttons.set(b, true);
    }
    assert_eq!(to_xgamepad(&i).buttons.raw, 0x1000 | 0x8000 | 0x0020 | 0x0400 | 0x0004);
}

#[test]
fn out_of_range_values_are_clamped() {
    let mut i = InputState::default();
    i.left.x = 3.0;
    i.l2 = -1.0;
    let g = to_xgamepad(&i);
    assert_eq!((g.thumb_lx, g.left_trigger), (32767, 0));
}
```

- [ ] **Step 2: rodar e confirmar que falha** — `cargo test -p axis-virtual` → erro de compilação.
- [ ] **Step 3: implementar `pad.rs` e `to_xgamepad`; re-exportar em `lib.rs`**
- [ ] **Step 4: rodar e confirmar que passa** — 4 testes; clippy `-D warnings` limpo.
- [ ] **Step 5: commit** — `feat(virtual): traits de controle virtual e conversão para XInput`

---

### Task 2: `ViGEmBus` (Xbox 360 virtual real)

**Files:**
- Modify: `crates/axis-virtual/src/x360.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `pub struct ViGEmBus { … }` com `pub fn connect() -> Result<ViGEmBus, VirtualError>` (`vigem_client::Client::connect`; `BusNotFound` → `VirtualError::BusNotFound`).
  - `impl VirtualBus for ViGEmBus`: `plug_x360` cria `Xbox360Wired` com um cliente próprio (`Client::connect` por virtual), `plugin`, `wait_ready`, lê `get_user_index`, pede `request_notification()` e usa `spawn_thread` para chamar `on_rumble(Rumble { strong: large_motor, weak: small_motor })`. `NoFreeSlot` → `VirtualError::NoFreeSlot`; outros → `Bus(msg)`.
  - `struct ViGEmX360` implementa `VirtualPad`: `update` usa `to_xgamepad`; `Drop` faz `unplug` e depois `join` da thread de notificação (ela termina sozinha quando o alvo some).
  - `pub(crate) fn map_vigem_error(e: vigem_client::Error) -> VirtualError` (pura, testável).

- [ ] **Step 1: testes que falham**

```rust
#[test]
fn vigem_errors_are_mapped() {
    assert!(matches!(map_vigem_error(vigem_client::Error::BusNotFound), VirtualError::BusNotFound));
    assert!(matches!(map_vigem_error(vigem_client::Error::NoFreeSlot), VirtualError::NoFreeSlot));
    assert!(matches!(map_vigem_error(vigem_client::Error::WinError(5)), VirtualError::Bus(_)));
}

#[test]
#[ignore = "hardware: ViGEmBus instalado"]
fn plugs_a_virtual_pad_visible_in_xinput_and_unplugs_on_drop() {
    let bus = ViGEmBus::connect().unwrap();
    let mut pad = bus.plug_x360(Box::new(|_| {})).unwrap();
    let slot = pad.user_index();
    assert!(slot < 4);
    let mut i = InputState::default();
    i.left.x = 1.0;
    pad.update(&i).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(axis_devices::xinput_is_connected(slot));
    drop(pad);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!axis_devices::xinput_is_connected(slot));
}
```

`xinput_is_connected(slot: u32) -> bool` ainda não existe: adicionar em `axis-devices/src/xinput.rs` (é `XInputGetState(slot) == ERROR_SUCCESS`) e re-exportar. Incluir esse arquivo no commit.

**Correção da bateria (Global Constraints), também no `axis-devices`:** em `hid.rs`, adicionar `pub(crate) fn sanitize_status(family: DeviceFamily, transport: Transport, report_id: u8, report: Report) -> Report`, que zera `report.status` para `DeviceStatus::default()` quando `family == DualShock4 && transport == Bluetooth && report_id == 0x01`, e devolve o report intacto nos outros casos. `HidController::read` aplica a função com o primeiro byte do buffer. Teste:

```rust
#[test]
fn ds4_bt_reduced_report_has_no_battery() {
    let mut r = Report::default();
    r.status = DeviceStatus { battery_percent: Some(0), charging: false };
    assert_eq!(sanitize_status(DeviceFamily::DualShock4, Transport::Bluetooth, 0x01, r).status, DeviceStatus::default());
    assert_eq!(sanitize_status(DeviceFamily::DualShock4, Transport::Bluetooth, 0x11, r).status, r.status);
    assert_eq!(sanitize_status(DeviceFamily::DualShock4, Transport::Usb, 0x01, r).status, r.status);
}
```

- [ ] **Step 2: rodar e confirmar que falha** — erro de compilação.
- [ ] **Step 3: implementar `ViGEmBus`, `ViGEmX360`, `map_vigem_error`, `xinput_is_connected`**
- [ ] **Step 4: rodar e confirmar que passa** — `cargo test -p axis-virtual`; e `cargo test -p axis-virtual -- --ignored plugs_a_virtual` (o ViGEmBus está instalado nesta máquina).
- [ ] **Step 5: commit** — `feat(virtual): Xbox 360 virtual pelo ViGEmBus com vibração e índice de jogador` (inclui a correção de bateria no `axis-devices`)

---

### Task 3: HidHide (`DeviceHider`)

**Files:**
- Create: `crates/axis-virtual/src/hidhide.rs`

Base: o código do spike em `spikes/ds4-bridge/src/hidhide.rs` (branch `spike/fase-0`; ler com `git show spike/fase-0:spikes/ds4-bridge/src/hidhide.rs`), reescrito com erros tipados e testes.

**Interfaces:**
- Produces:
  - `#[derive(Debug, thiserror::Error)] pub enum HideError { #[error("HidHide não encontrado")] NotInstalled, #[error("erro do HidHide: código {0}")] Win32(u32) }`
  - `pub trait DeviceHider: Send + Sync { fn hide(&self, instance_id: &str) -> Result<(), HideError>; fn unhide(&self, instance_id: &str) -> Result<(), HideError>; fn unhide_all(&self, instance_ids: &[String]) -> Result<(), HideError>; }`
  - `pub struct HidHide { self_image: String }` com `pub fn open(self_exe: &Path) -> Result<HidHide, HideError>`: confirma que `\\.\HidHide` abre (`ERROR_FILE_NOT_FOUND` → `NotInstalled`) e guarda o caminho do executável em formato `\Device\HarddiskVolumeN\…`.
  - `hide`: garante `self_image` na whitelist, adiciona o ID à blacklist (sem duplicar, comparação sem diferenciar maiúsculas), ativa. `unhide`: remove o ID. `unhide_all`: remove todos os IDs dados; se a blacklist ficar vazia, remove `self_image` da whitelist e desativa.
  - Funções puras (testáveis): `pub fn instance_id_from_path(path: &str) -> String`, `pub(crate) fn encode_multi_sz(items: &[String]) -> Vec<u8>`, `pub(crate) fn decode_multi_sz(bytes: &[u8]) -> Vec<String>`, `pub(crate) const fn ctl_code(function: u32) -> u32`.
  - Re-exportar `DeviceHider`, `HidHide`, `HideError`, `instance_id_from_path`.

- [ ] **Step 1: testes que falham**

```rust
#[test]
fn instance_id_is_derived_from_hid_path() {
    let p = r"\\?\HID#{00001124-0000-1000-8000-00805f9b34fb}_VID&0002054c_PID&09cc#8&2a4cafad&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    assert_eq!(instance_id_from_path(p), r"HID\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002054C_PID&09CC\8&2A4CAFAD&0&0000");
}

#[test]
fn multi_sz_round_trips_and_handles_empty() {
    let items = vec![r"HID\A".to_string(), r"HID\B".to_string()];
    let bytes = encode_multi_sz(&items);
    assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
    assert_eq!(decode_multi_sz(&bytes), items);
    assert_eq!(decode_multi_sz(&encode_multi_sz(&[])), Vec::<String>::new());
    assert_eq!(decode_multi_sz(&[0x41]), Vec::<String>::new()); // byte ímpar solto: ignorado, sem pânico
}

#[test]
fn ioctl_codes_match_hidhide() {
    assert_eq!(ctl_code(2048), 0x8001_6000);
    assert_eq!(ctl_code(2053), 0x8001_6014);
}

#[test]
#[ignore = "hardware: HidHide instalado e um DS4 ligado"]
fn hide_then_unhide_all_restores_original_state() {
    let api = hidapi::HidApi::new().unwrap();
    let c = axis_devices::enumerate_hid(&api).into_iter().next().expect("controle");
    let id = instance_id_from_path(&c.path);
    let h = HidHide::open(&std::env::current_exe().unwrap()).unwrap();
    h.hide(&id).unwrap();
    h.unhide_all(&[id]).unwrap();
}
```

`hidapi` entra só como dev-dependency do `axis-virtual`.

- [ ] **Step 2: rodar e confirmar que falha** — erro de compilação.
- [ ] **Step 3: implementar `hidhide.rs`**
- [ ] **Step 4: rodar e confirmar que passa** — unitários verdes; o de hardware roda se houver DS4 ligado (deixar o HidHide como estava).
- [ ] **Step 5: commit** — `feat(virtual): ocultar controles físicos pelo HidHide`

---

### Task 4: `axis-daemon`: caminhos e armazenamento robusto

**Files:**
- Create: `crates/axis-daemon/Cargo.toml`, `src/lib.rs`, `src/paths.rs`, `src/store.rs`, `src/main.rs` (só `fn main() {}` nesta task)

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug)] pub struct AppPaths { pub config: PathBuf, pub state: PathBuf }` com `pub fn from_env() -> AppPaths` (`%APPDATA%\Axis`, `%LOCALAPPDATA%\Axis`) e `pub fn under(root: &Path) -> AppPaths` (`root/config`, `root/state`, para testes); `pub fn controllers_dir(&self)`, `profiles_dir(&self)`, `settings_file(&self)`, `hidden_file(&self)`, `logs_dir(&self)`.
  - `pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()>`: cria o diretório; se o arquivo existe, copia para `<arquivo>.bak`; escreve em `<arquivo>.tmp`, `sync_all`, renomeia por cima.
  - `#[derive(Debug, PartialEq)] pub enum Loaded<T> { Fresh(T), FromBackup(T), Missing, Corrupt { preserved_as: PathBuf }, TooNew { found: u32 } }`
  - `pub fn load_toml<T: DeserializeOwned>(path: &Path, check: impl Fn(&T) -> Result<(), String>) -> Loaded<T>`: lê; primeiro passa pelo `axis_core::SchemaProbe` + `check_schema` (versão mais nova → `TooNew`, **sem tocar no arquivo**); depois desserializa e roda `check`. Falhou → tenta o `.bak` (`FromBackup`); falhou também → renomeia o arquivo para `<arquivo>.corrupt` (com sufixo numérico se já existir) e devolve `Corrupt`. Não existe → `Missing`.
  - Testes usam um diretório temporário único: `std::env::temp_dir().join(format!("axis-test-{}-{}", std::process::id(), nome))`, apagado no fim (helper `TempDir` com `Drop` em `store.rs` sob `#[cfg(test)]`).

- [ ] **Step 1: testes que falham** (`store.rs`), com um tipo de teste `#[derive(Serialize, Deserialize, PartialEq, Debug)] struct Doc { schema_version: u32, value: u32 }`

```rust
#[test] fn write_then_load_is_fresh() { /* write_atomic(doc v=1) → load_toml == Fresh(doc) */ }
#[test] fn second_write_keeps_previous_as_bak() { /* write v=1, write v=2 → .bak contém v=1 */ }
#[test] fn corrupt_file_falls_back_to_backup() { /* write v=1, write v=2, sobrescreve arquivo com "lixo" → FromBackup(v=1) */ }
#[test] fn corrupt_without_backup_is_preserved() { /* só "lixo" → Corrupt{preserved_as: …corrupt}; o .corrupt existe e o original não */ }
#[test] fn newer_schema_is_left_untouched() { /* "schema_version = 2\nvalue = 1" → TooNew{found:2}; arquivo intacto, sem .corrupt */ }
#[test] fn missing_file_is_missing() { /* → Missing */ }
#[test] fn check_failure_counts_as_corrupt() { /* check retorna Err → mesmo caminho do lixo */ }
```

O implementador escreve cada corpo exatamente como o comentário descreve.

- [ ] **Step 2: rodar e confirmar que falha** — `cargo test -p axis-daemon`.
- [ ] **Step 3: implementar `paths.rs` e `store.rs`**
- [ ] **Step 4: rodar e confirmar que passa** — 7 testes; clippy limpo.
- [ ] **Step 5: commit** — `feat(daemon): caminhos do app e armazenamento atômico com backup`

---

### Task 5: configurações, controles e perfis

**Files:**
- Create: `crates/axis-daemon/src/config.rs`

**Interfaces:**
- Consumes: `store` (Task 4), `axis_core::{ControllerRecord, Corrections, Profile, DeviceFamily, SCHEMA_VERSION}`.
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)] #[serde(default)] pub struct AppSettings { pub schema_version: u32, pub autostart: bool, pub grace_secs: u64, pub theme: String, pub accent: String }`; `Default`: versão 1, `autostart: true`, `grace_secs: 5`, `theme: "dark"`, `accent: "laranja"`. Validação: `1 ≤ grace_secs ≤ 60`.
  - `pub struct ConfigStore { paths: AppPaths, notices: Vec<String> }` com:
    - `pub fn new(paths: AppPaths) -> ConfigStore`
    - `pub fn load_settings(&mut self) -> AppSettings` / `pub fn save_settings(&self, s: &AppSettings) -> io::Result<()>`
    - `pub fn load_record(&mut self, id: &str) -> Option<ControllerRecord>` / `pub fn save_record(&self, r: &ControllerRecord) -> io::Result<()>`
    - `pub fn new_record(id: &str, family: DeviceFamily, palette_index: usize) -> ControllerRecord`: dono `"Controle N"` (N = `palette_index + 1`), cor da paleta `OWNER_PALETTE[palette_index % 6]`, perfil `"Padrão"`, `Corrections::default()`.
    - `pub const OWNER_PALETTE: [[u8; 3]; 6] = [[95, 179, 122], [155, 122, 216], [224, 176, 74], [91, 160, 220], [217, 119, 87], [210, 110, 160]];`
    - `pub fn load_profile(&mut self, name: &str) -> Profile`: ausente → cria e salva o `Profile::default()` quando `name == "Padrão"`; outro nome ausente/corrompido → `Profile::default()` e um aviso.
    - `pub fn export_profile(p: &Profile, dest: &Path) -> io::Result<()>` / `pub fn import_profile(src: &Path) -> Result<Profile, String>` (valida; versão mais nova → erro com texto em português).
    - `pub fn take_notices(&mut self) -> Vec<String>`: avisos acumulados (backup usado, arquivo corrompido preservado, versão mais nova) para a bandeja mostrar.
    - Nome de arquivo do controle: `id` com `:` trocado por `-`, mais `.toml` (`41-42-00-00-84-b6.toml`). Nome de perfil: caracteres fora de `[A-Za-z0-9 _-]` e acentos comuns trocados por `_`.
  - Versão mais nova (`TooNew`) **não** é sobrescrita: o registro fica só em memória e gera aviso.

- [ ] **Step 1: testes que falham**

```rust
#[test] fn settings_default_when_missing_and_round_trip() { /* load_settings em dir vazio == default; save + load == igual */ }
#[test] fn new_record_cycles_palette_and_names() { /* new_record(.., 0).owner_name == "Controle 1", cor OWNER_PALETTE[0]; índice 7 → OWNER_PALETTE[1] */ }
#[test] fn record_file_name_replaces_colons() { /* save_record id "41:42:00:00:84:b6" → arquivo controllers/41-42-00-00-84-b6.toml existe; load_record devolve igual */ }
#[test] fn default_profile_is_created_on_first_load() { /* load_profile("Padrão") → default; arquivo profiles/Padrão.toml passa a existir */ }
#[test] fn export_import_round_trip_and_rejects_invalid() { /* export → import == igual; arquivo com inner >= outer → Err */ }
#[test] fn too_new_record_is_not_overwritten() { /* grava "schema_version = 99" no arquivo do id; load_record → None, take_notices não vazio, arquivo intacto */ }
```

- [ ] **Step 2: rodar e confirmar que falha**
- [ ] **Step 3: implementar `config.rs`**
- [ ] **Step 4: rodar e confirmar que passa**
- [ ] **Step 5: commit** — `feat(daemon): configurações, registros de controle e perfis`

---

### Task 6: thread trabalhadora por controle

**Files:**
- Create: `crates/axis-daemon/src/worker.rs`

**Interfaces:**
- Consumes: `axis_devices::{Controller, Report, OutputState, DeviceError, DeviceStatus}`, `axis_core::{Pipeline, InputState}`, `axis_virtual::{VirtualPad, Rumble}`.
- Produces:
  - `pub type SharedPad = Arc<Mutex<Box<dyn VirtualPad>>>;`
  - `pub enum WorkerCommand { SetPipeline(Box<Pipeline>), SetLight { lightbar: [u8; 3], player: Option<u8> }, Rumble(Rumble), SetPaused(bool), Shutdown }`
  - `#[derive(Debug, PartialEq)] pub enum WorkerExit { Disconnected, Error(String), Shutdown }`
  - `#[derive(Clone, Copy, Debug, Default, PartialEq)] pub struct WorkerStatus { pub status: DeviceStatus, pub reports: u64 }` (compartilhado por `Arc<Mutex<WorkerStatus>>`, atualizado a cada 64 reports, não a cada um).
  - `pub fn spawn_worker(controller: Box<dyn Controller>, pad: Option<SharedPad>, pipeline: Pipeline, status: Arc<Mutex<WorkerStatus>>, on_exit: impl FnOnce(WorkerExit) + Send + 'static) -> (Sender<WorkerCommand>, JoinHandle<()>)`. `pad = None` = modo visualização (sem virtual).
  - Laço: drena todos os comandos com `try_recv`; `controller.read(50)`; `Some(report)` → `pipeline.process(&report.input)`; se não pausado e há pad: `pad.lock().update(&out)`; se pausado: envia `InputState::default()` uma vez ao pausar. `SetLight`/`Rumble` → atualiza um `OutputState` local e chama `write_output` só quando ele muda. `Err(Disconnected)` → sai com `WorkerExit::Disconnected`; `Err(Hid(m))` → `WorkerExit::Error(m)`; `Shutdown` → `WorkerExit::Shutdown`. `on_exit` é chamado uma vez, ao sair.
  - Erro de `pad.update` é registrado com `log::warn!` no máximo uma vez por segundo e não derruba a thread.
  - **Latência (spec 9):** `pub(crate) struct LatencyHistogram { buckets: [u32; 101] }` (bucket `i` = `[10·i, 10·i + 10)` µs; o último acumula ≥ 1000 µs), com `fn record(&mut self, us: u64)`, `fn percentile(&self, p: f64) -> u64` (devolve o limite superior do bucket onde a contagem acumulada atinge `p`; histograma vazio → 0) e `fn reset(&mut self)`. O worker mede de "report recebido" até "`update` do pad retornou" com `Instant`, grava no histograma (sem alocar) e, a cada 60 s, faz `log::info!("latência p50 {} µs, p99 {} µs, {} reports")` e zera.

- [ ] **Step 1: testes que falham**, com falsos no módulo de teste: `FakeController` (fila de `Result<Option<Report>, DeviceError>`, grava os `OutputState` escritos) e `FakePad` (grava os `InputState` recebidos)

```rust
#[test] fn reports_go_through_pipeline_to_the_pad() { /* report com left.x = 0.03 (dentro da deadzone padrão 0.05) e outro com 1.0 → pad recebe x 0.0 e ~1.0 */ }
#[test] fn disconnect_ends_the_worker_with_disconnected() { /* fila termina em Err(Disconnected) → on_exit(Disconnected) */ }
#[test] fn hid_error_ends_with_error() { /* Err(Hid("x")) → on_exit(Error("x")) */ }
#[test] fn pause_sends_neutral_once_and_stops_forwarding() { /* SetPaused(true) → pad recebe um InputState::default() e mais nada até SetPaused(false) */ }
#[test] fn light_and_rumble_write_output_only_on_change() { /* SetLight igual duas vezes → 1 write; Rumble muda → 2º write com rumble */ }
#[test] fn pipeline_swap_takes_effect() { /* SetPipeline com deadzone 0.5 → input 0.3 vira 0.0 */ }
#[test] fn view_only_mode_reads_without_pad() { /* pad None; status.reports cresce */ }
#[test] fn latency_histogram_percentiles() { /* record 5, 15, 15, 2000 → percentile(0.5) == 20, percentile(0.99) >= 1000; reset → percentile(0.5) == 0 */ }
```

- [ ] **Step 2: rodar e confirmar que falha**
- [ ] **Step 3: implementar `worker.rs`**
- [ ] **Step 4: rodar e confirmar que passa**
- [ ] **Step 5: commit** — `feat(daemon): thread de controle com pipeline, pausa, luz e vibração`

---

### Task 7: gerente: chegada de controles

**Files:**
- Create: `crates/axis-daemon/src/ports.rs`, `crates/axis-daemon/src/manager.rs`

**Interfaces:**
- Consumes: Tasks 5 e 6; `axis_devices::{HidCandidate, Controller, DeviceError}`; `axis_virtual::{VirtualBus, DeviceHider, instance_id_from_path, Rumble}`.
- Produces (`ports.rs`):
  - `pub trait ControllerOpener: Send { fn enumerate(&mut self) -> Vec<HidCandidate>; fn open(&mut self, c: &HidCandidate) -> Result<Box<dyn Controller>, DeviceError>; }` (implementação real `HidOpener` com `hidapi::HidApi`, chamando `refresh_devices` em `enumerate`).
  - `pub trait Scheduler: Send { fn after(&self, delay: Duration, event: Event); }` (real: thread que dorme e envia pelo `Sender<Event>`).
  - `pub trait Notifier: Send { fn notify(&self, title: &str, body: &str); }` (real: bandeja, Task 10; falso nos testes grava as mensagens).
- Produces (`manager.rs`):
  - `pub enum Event { DeviceArrived(String), DeviceRemoved(String), WorkerExited { session: u64, exit: WorkerExit }, GraceExpired { id: String, generation: u64 }, Rumble { id: String, rumble: Rumble }, SetPaused(bool), RestoreControllers, Quit, Rescan }`
  - `#[derive(Clone, Debug, PartialEq)] pub struct SessionView { pub id: String, pub owner: String, pub color: [u8; 3], pub family: DeviceFamily, pub player: Option<u8>, pub connected: bool, pub experimental: bool, pub view_only: bool }`
  - `pub struct Manager { … }` com `pub fn new(deps: ManagerDeps) -> Manager` (`ManagerDeps { opener, bus: Option<Arc<dyn VirtualBus>>, hider: Option<Arc<dyn DeviceHider>>, store: ConfigStore, scheduler, notifier, events: Sender<Event> }`), `pub fn handle(&mut self, e: Event) -> bool` (retorna `false` em `Quit`), `pub fn sessions(&self) -> Vec<SessionView>`, `pub fn start(&mut self)` (enumera os já conectados).
  - **Fluxo de chegada** (`DeviceArrived` e `start`): enumerar; para cada candidato ainda sem sessão pelo path: `open` → `id` → se já há sessão **ativa** com esse id, usar `"{id}#2"` (`#3`…) → `load_record` ou `new_record` (salvando) → `load_profile` → `Pipeline::compile` (falha → perfil padrão + aviso) → se `bus` existe e a família não é Xbox: `plug_x360` (o `RumbleSink` envia `Event::Rumble { id }`), **depois** `hider.hide(instance_id_from_path(path))` e grava o ID em `hidden.toml` → `spawn_worker` → `SetLight { lightbar: record.owner_color, player: Some(user_index + 1) }` → `notifier.notify("Axis", "Maria (verde) → Player 2")` (nome da cor pela paleta: verde, roxo, amarelo, azul, laranja, rosa; fora da paleta: sem a cor).
  - `bus == None` (ViGEmBus ausente): modo visualização para todos (`view_only = true`), sem ocultar; um único aviso "ViGEmBus não encontrado: o Axis só mostra os controles, sem emular." na primeira chegada.
  - Família Xbox: sessão `view_only`, sem virtual nem ocultação. O XInput dos Xbox (abertura via `XInputController`) fica para quando houver hardware; nesta task, Xbox não entra pelo `enumerate` do HID e não é tratado.

- [ ] **Step 1: testes que falham** (falsos: `FakeOpener` com candidatos e controladores falsos, `FakeBus` que conta virtuais vivos e índices, `FakeHider` que registra a ordem das chamadas junto com o `FakeBus` num log compartilhado, `FakeNotifier`, `FakeScheduler` que guarda os eventos agendados)

```rust
#[test] fn arrival_plugs_then_hides_then_lights_and_notifies() { /* log == ["plug", "hide:<instance>"]; worker recebeu SetLight{owner_color, Some(1)}; notifier recebeu "Controle 1 (verde) → Player 1" */ }
#[test] fn known_controller_keeps_owner_and_profile() { /* record salvo antes com owner "Maria", cor [155,122,216] → notificação "Maria (roxo) → Player 1" */ }
#[test] fn new_controller_gets_record_saved_with_next_palette_color() { /* 2 controles novos → cores OWNER_PALETTE[0] e [1]; arquivos salvos */ }
#[test] fn missing_vigem_means_view_only_with_single_notice() { /* bus None, 2 chegadas → nenhum hide; sessions().all(view_only); 1 notificação de aviso */ }
#[test] fn duplicate_id_gets_suffix_and_own_pad() { /* dois candidatos com o mesmo id → ids "x" e "x#2", 2 virtuais */ }
#[test] fn arrival_of_same_path_twice_is_ignored() { /* DeviceArrived repetido → 1 sessão */ }
```

- [ ] **Step 2: rodar e confirmar que falha**
- [ ] **Step 3: implementar `ports.rs` (traits + `HidOpener` + `ThreadScheduler`) e o fluxo de chegada em `manager.rs`**
- [ ] **Step 4: rodar e confirmar que passa**
- [ ] **Step 5: commit** — `feat(daemon): gerente com fluxo de conexão, identidade e ocultação`

---

### Task 8: gerente: desconexão, período de graça, restauração

**Files:**
- Modify: `crates/axis-daemon/src/manager.rs`

**Interfaces:**
- Consumes: Task 7.
- Produces (comportamento do `Manager::handle`):
  - `WorkerExited { Disconnected }` → a sessão fica `connected = false`; o virtual **continua** e recebe `InputState::default()`; agenda `GraceExpired { id, generation }` para `settings.grace_secs`. `generation` aumenta a cada desconexão (evita expirar uma sessão que já voltou).
  - Nova chegada com o **mesmo id** de uma sessão em graça → reaproveita o mesmo `SharedPad` (mesmo Player), oculta o novo path, novo worker, `connected = true`, sem notificação de Player novo (só "Maria reconectou").
  - `GraceExpired` com `generation` atual e sessão ainda desconectada → solta o virtual (Drop desconecta), desoculta o path antigo, remove o ID de `hidden.toml`, remove a sessão.
  - `WorkerExited { Error }` → fecha e tenta reabrir o mesmo path uma vez via `Rescan`; se falhar, trata como `Disconnected`.
  - `DeviceRemoved(path)` → nada além de log (a saída do worker é o sinal confiável).
  - `Rumble { id, r }` → `WorkerCommand::Rumble(r)` para o worker ativo daquela sessão (ignora se em graça).
  - `SetPaused(b)` → `SetPaused(b)` para todos os workers.
  - `RestoreControllers` → `hider.unhide_all(todos os IDs em hidden.toml)`, limpa `hidden.toml`; as sessões passam para `view_only` até o próximo início; notificação "Controles restaurados".
  - `Quit` → `Shutdown` para os workers (com `join`), solta os virtuais, `unhide_all`, limpa `hidden.toml`, retorna `false`.
  - `Manager::start` faz a **reconciliação** antes de tudo: lê `hidden.toml` (de uma execução que travou) e chama `unhide_all` com aqueles IDs; depois enumera.

- [ ] **Step 1: testes que falham**

```rust
#[test] fn disconnect_keeps_pad_and_sends_neutral_then_schedules_grace() { /* worker sai Disconnected → FakeBus ainda tem 1 virtual vivo; último input == default; scheduler tem GraceExpired{gen 1} após 5 s */ }
#[test] fn reconnect_within_grace_reuses_the_same_pad_and_player() { /* desconecta, chega de novo mesmo id → FakeBus total de plugs == 1; sessions()[0].player == Some(1); notificação "… reconectou" */ }
#[test] fn grace_expiry_unplugs_and_unhides() { /* desconecta, GraceExpired gen 1 → virtuais vivos 0; FakeHider.unhide do instance; hidden.toml sem o id; sessions() vazio */ }
#[test] fn stale_grace_expiry_is_ignored_after_reconnect() { /* desconecta (gen 1), reconecta, GraceExpired gen 1 → sessão continua */ }
#[test] fn startup_reconciles_hidden_entries_from_a_crash() { /* hidden.toml pré-existente com ["HID\\X"] → start chama unhide_all(["HID\\X"]) antes de qualquer hide */ }
#[test] fn restore_unhides_everything_and_clears_state() { /* 2 controles → RestoreControllers → unhide_all com 2 ids; hidden.toml vazio */ }
#[test] fn quit_shuts_down_workers_and_unhides() { /* Quit → handle retorna false; workers receberam Shutdown; unhide_all chamado; virtuais vivos 0 */ }
#[test] fn rumble_reaches_the_active_worker() { /* Event::Rumble → worker recebeu Rumble */ }
```

- [ ] **Step 2: rodar e confirmar que falha**
- [ ] **Step 3: implementar**
- [ ] **Step 4: rodar e confirmar que passa**
- [ ] **Step 5: commit** — `feat(daemon): período de graça, reconexão e restauração dos controles`

---

### Task 9: logging, instância única e início automático

**Files:**
- Create: `crates/axis-daemon/src/logging.rs`, `src/instance.rs`, `src/autostart.rs`

**Interfaces:**
- Produces:
  - `pub fn init_logging(dir: &Path) -> io::Result<()>`: implementa `log::Log`; as mensagens vão por um `mpsc` para uma thread escritora (nunca escreve no caminho crítico); arquivo `axis.log`; ao passar de **1 MiB**, gira para `axis.1.log` … `axis.3.log` (mantém 3). Nível `Info`. Linha: `2026-11-09T16:40:12Z INFO alvo: mensagem` (hora UTC via `SystemTime`, sem crate de data).
  - `pub(crate) fn rotate(dir: &Path, keep: usize) -> io::Result<()>` (pura em relação ao sistema de arquivos, testável).
  - `pub struct SingleInstance` com `pub fn acquire(name: &str) -> Option<SingleInstance>` (`CreateMutexW`; `ERROR_ALREADY_EXISTS` → `None`); `Drop` fecha o handle. Nome no app: `"Local\\AxisDaemon"`.
  - `pub struct RunKey { subkey: Vec<u16>, value: Vec<u16> }` com `pub fn user_run(value: &str) -> RunKey` (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`) e `pub fn custom(subkey: &str, value: &str) -> RunKey` (para testes); `pub fn is_enabled(&self) -> bool`, `pub fn enable(&self, exe: &Path) -> io::Result<()>` (valor `"\"<exe>\" --tray"`), `pub fn disable(&self) -> io::Result<()>`.
  - Cargo: `log = "0.4"`; `windows-sys` features `Win32_System_Registry`, `Win32_System_Threading`, `Win32_Security`.

- [ ] **Step 1: testes que falham**

```rust
// logging.rs
#[test] fn rotate_shifts_files_and_keeps_three() { /* cria axis.log, axis.1..3.log com conteúdos "0".."3" → rotate → axis.1=="0", axis.2=="1", axis.3=="2", sem axis.4, sem axis.log */ }
// instance.rs
#[test] fn second_acquire_fails_until_first_is_dropped() { /* nome único com pid → acquire Some, acquire None, drop, acquire Some */ }
// autostart.rs (usa uma chave de teste, nunca a Run real)
#[test] fn run_key_enable_disable_round_trip() { /* RunKey::custom("Software\\AxisTest-<pid>", "Axis") → !is_enabled, enable, is_enabled, disable, !is_enabled; apaga a subchave no fim */ }
```

- [ ] **Step 2: rodar e confirmar que falha**
- [ ] **Step 3: implementar os três módulos**
- [ ] **Step 4: rodar e confirmar que passa**
- [ ] **Step 5: commit** — `feat(daemon): logging em arquivo, instância única e início automático`

---

### Task 10: bandeja, notificações e o binário

**Files:**
- Create: `crates/axis-daemon/src/tray.rs`; Modify: `crates/axis-daemon/src/main.rs`

**Interfaces:**
- Consumes: tudo; `axis_devices::HotplugWatcher`.
- Produces:
  - `pub enum TrayCommand { Pause, Resume, Restore, ToggleAutostart, Quit }`
  - `pub struct Tray` com `pub fn spawn(on_command: impl Fn(TrayCommand) + Send + 'static, sessions: Arc<Mutex<Vec<SessionView>>>, autostart: Arc<AtomicBool>) -> (TrayHandle, JoinHandle<()>)`: thread própria com janela oculta (`CreateWindowExW` message-only), `Shell_NotifyIconW` (ícone `IDI_APPLICATION` por enquanto, dica "Axis"), laço `GetMessageW`. Clique direito → `TrackPopupMenu` montado na hora:
    - `Axis` (título, desabilitado)
    - uma linha desabilitada por sessão: `"Maria — Player 1"`, `"Maria — reconectando…"`, `"Maria — só visualização"`
    - separador
    - `Pausar emulação` / `Retomar emulação`
    - `Restaurar controles`
    - `Iniciar com o Windows` (marcado conforme `autostart`)
    - separador
    - `Sair`
  - `TrayHandle: Notifier` (envia a notificação como balão `NIF_INFO` postando uma mensagem para a janela da bandeja) e `fn close(&self)`.
  - `main.rs`, em ordem: `SingleInstance::acquire` (já rodando → sai em silêncio) → `AppPaths::from_env` → `init_logging` → `ConfigStore` + `load_settings` → ajusta a chave Run conforme `settings.autostart` (primeira execução: habilita) → `ViGEmBus::connect` (falha → `None` + log) → `HidHide::open` (falha → `None` + log) → canal de eventos → `Manager::new` → `Tray::spawn` (comandos viram `Event`s; `ToggleAutostart` grava settings e a chave) → `HotplugWatcher::start` (Arrived/Removed viram `Event`s) → `manager.start()` → laço `while manager.handle(rx.recv())`; a cada evento atualiza o `Arc<Mutex<Vec<SessionView>>>`; mostra `store.take_notices()` pela bandeja → ao sair: `tray.close()`, log "encerrado".
  - Sem testes automatizados nesta task (é a camada Win32 e a composição); a verificação é manual, com o autor, na Task 11.

- [ ] **Step 1: implementar `tray.rs` e `main.rs`**
- [ ] **Step 2: compilar e rodar uma fumaça sem controle** — `cargo build -p axis-daemon --release`; executar o binário, conferir que o ícone aparece, que uma segunda execução sai sozinha, que "Sair" encerra e que `%LOCALAPPDATA%\Axis\logs\axis.log` tem as linhas de início e fim.
- [ ] **Step 3: workspace verde** — `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
- [ ] **Step 4: commit** — `feat(daemon): bandeja com menu e notificações, e o binário do núcleo`

---

### Task 11: verificação ponta a ponta com o autor e medições

Tarefa do **controlador com o autor** (precisa do DS4 e de olhar a tela). Não é delegada.

**Files:**
- Create: `docs/benchmarks/daemon.md`; Modify: `docs/hardware-checklist.md`

- [ ] **Step 1: ponta a ponta** com o DS4 por Bluetooth e o núcleo em release:
  - ao ligar o controle: notificação "Controle 1 (verde) → Player 1", lightbar na cor do dono;
  - `joy.cpl` mostra **só** o "Controlador Xbox 360", e o DS4 físico não aparece (ocultado);
  - mexer nos analógicos e botões aparece no Xbox 360 virtual;
  - desligar o DS4 e religar em menos de 5 s → mesmo Player, sem notificação de Player novo;
  - desligar e esperar mais de 5 s → o virtual some e o DS4 físico volta a aparecer no `joy.cpl`;
  - "Pausar emulação" congela a entrada; "Retomar" volta;
  - "Restaurar controles" → o DS4 físico volta a aparecer;
  - matar o processo pelo Gerenciador de Tarefas com o DS4 oculto → ao abrir de novo, o DS4 físico volta (reconciliação);
  - "Iniciar com o Windows" alterna a entrada em `HKCU\…\Run`.
- [ ] **Step 2: medições** (spec 9) com o DS4 conectado: working set e memória privada após 1 min; CPU média em 60 s parado e 60 s mexendo (`Get-Process`, `TotalProcessorTime`); latência leitura → envio pelas linhas `latência p50/p99` do `axis.log`. Registrar em `docs/benchmarks/daemon.md`.
- [ ] **Step 3: atualizar o checklist** (vibração pelo jogo, se testada) e commit — `test(daemon): verificação ponta a ponta e medições do núcleo`
