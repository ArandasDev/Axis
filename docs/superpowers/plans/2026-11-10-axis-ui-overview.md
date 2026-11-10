# Axis, plano 5a: protocolo, servidor no núcleo e interface (Visão geral)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a janela do Axis abre sob demanda, conversa com o núcleo por named pipe e mostra ao vivo os controles conectados e a tela Visão geral (palco com o controle, a cadeia "Seu controle → O jogo recebe" e as métricas), no visual adotado do design.

**Architecture:** `axis-protocol` define mensagens JSON (uma por linha) e tipos próprios, estáveis e independentes dos internos. O núcleo ganha um servidor de named pipe (`ipc.rs`) que lê um registro compartilhado (sessões + "torneiras" ao vivo por controle) e só calcula o rastro das etapas quando uma interface está inspecionando aquele controle. `axis-ui` é um processo separado em Slint com renderizador por software: uma thread cliente recebe mensagens e atualiza os modelos Slint pelo `invoke_from_event_loop`. A interface nunca depende de `axis-devices` nem de `axis-virtual` (spec 3.2).

**Tech Stack:** Rust 1.99 (edition 2024), `serde` + `serde_json`, `slint` 1.18 (`renderer-software`, `backend-winit`), `slint-build`, `windows-sys` 0.61.

**Spec:** `docs/superpowers/specs/2026-11-09-axis-design.md` (3, 6.3, 8, 9). Visual: `docs/spikes/fase-0-c-slint.md` e o design "AXIS Emulador de Controle" (Claude Design, página *Controller Emulator v2*). Planos seguintes: **5b** Correção (assistentes), **5c** Mapeamento, Perfis, nome e cor do controle.

## Global Constraints

- Edition 2024, workspace `crates/*`, Windows x64, build em `C:/dev/cargo-target/axis`.
- `axis-ui` depende só de `axis-protocol`, `slint`, `windows-sys`, `serde_json`/`serde` (via protocolo). **Nunca** de `axis-devices`, `axis-virtual` ou `axis-daemon`.
- Pipe: `\\.\pipe\axis-v1`, só local (`PIPE_REJECT_REMOTE_CLIENTS`), até 4 clientes, uma mensagem JSON por linha, linha máxima **64 KiB** (maior → desconectar o cliente).
- Custo zero sem interface: com nenhum cliente inspecionando, o worker usa `process` (sem rastro) e nada é amostrado; o servidor não acorda sem evento (nada de polling). Com inspeção ativa: amostra ≤ **60 por segundo**, sem alocação no worker.
- Slint com `default-features = false` + `backend-winit` + `renderer-software` + `compat-1-2`; fontes embutidas; cores, espaçamentos, raios e durações só via `global Tokens`.
- Visual (spec 8.1): base `#1c1b1a`, superfície `#262523`, texto `#ecebe6`, divisores com o texto a 15%; destaque escolhido pelo usuário (laranja `#d97757` padrão; vermelho, pink, roxo, índigo, azul, verde em OKLCH L 0,67); painéis com raio 18 px, botões 11 px, pílulas; Barlow Condensed (títulos 600) e Barlow (texto).
- Metas (spec 9): abrir a interface até o primeiro quadro em **< 500 ms**; registrar a RAM da interface aberta.
- Textos da interface em português.
- Git: Conventional Commits, branch `feat/axis-ui`, autor único, **sem trailers `Co-Authored-By` nem linhas de atribuição**, sem PR.

## Review Focus

1. **Núcleo não está rodando** ao abrir a interface: ela tenta iniciar `axis.exe` (mesma pasta) e reconecta; se não der, mostra "O núcleo do Axis não está rodando" com o botão "Iniciar". Nunca trava nem fecha sozinha. Testes nas Tasks 6 e 9.
2. **Núcleo de outra versão do protocolo:** a interface mostra "Versão do núcleo incompatível; reinicie o Axis" em vez de interpretar errado. Teste na Task 1 (negociação) e 6.
3. **Interface fechada no meio da inspeção** (cliente some): o worker volta a `process` sem rastro e o servidor libera a conexão. Teste na Task 4.
4. **Mensagem malformada ou gigante** de um cliente: o servidor desconecta só aquele cliente, sem pânico. Testes nas Tasks 1 e 4.
5. **Controle desconectado enquanto inspecionado:** o palco mostra o estado "reconectando…" e os valores vão a neutro; nada de valores congelados. Testes nas Tasks 3 e 8.

---

## Estrutura de arquivos

```
crates/axis-protocol/
├─ Cargo.toml
└─ src/lib.rs                 versão, nome do pipe, mensagens, tipos espelho, framing
crates/axis-daemon/src/
├─ live.rs                    LiveTap (amostra ao vivo por sessão) e UiRegistry
├─ ipc.rs                     servidor do named pipe
├─ worker.rs                  (mod.) usa a LiveTap
├─ manager.rs                 (mod.) mantém o UiRegistry
├─ tray.rs                    (mod.) item "Abrir o Axis"
└─ main.rs / app.rs           (mod.) inicia o servidor
crates/axis-ui/
├─ Cargo.toml, build.rs
├─ ui/
│  ├─ tokens.slint            global Tokens (tema + destaque)
│  ├─ fonts/                  Barlow, Barlow Condensed (.ttf, licença OFL)
│  ├─ components.slint        Pill, Stat, Bar, Plot, PadCard, NavItem
│  ├─ controller-art.slint    ilustração em linha (DS4, DualSense, Xbox) com analógicos e gatilhos vivos
│  ├─ overview.slint          tela Visão geral
│  └─ app.slint               janela: barra de título, barra lateral, estados de conexão
└─ src/
   ├─ main.rs                 inicia Slint (software), cliente e ligações
   ├─ theme.rs                OKLCH → sRGB, paleta de destaque
   ├─ client.rs               cliente do pipe (thread), reconexão, início do núcleo
   └─ view.rs                 funções puras: SessionInfo/LiveFrame → textos e valores da UI
docs/benchmarks/ui.md
```

---

### Task 1: `axis-protocol`

**Files:** Create `crates/axis-protocol/Cargo.toml`, `src/lib.rs`

**Interfaces (Produces):**
- `pub const PROTOCOL_VERSION: u32 = 1;` `pub const PIPE_NAME: &str = r"\\.\pipe\axis-v1";` `pub const MAX_LINE: usize = 65_536;`
- Tipos espelho (`Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize`):
  - `pub struct PadState { pub lx: f32, pub ly: f32, pub rx: f32, pub ry: f32, pub l2: f32, pub r2: f32, pub buttons: u32 }` (bits = `axis_core::Button as u8`)
  - `pub struct StickStages { pub raw: [f32; 2], pub calibrated: [f32; 2], pub drift: [f32; 2], pub deadzone: [f32; 2], pub curve: [f32; 2], pub anti: [f32; 2] }`
- `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)] pub enum Family { DualShock4, DualSense, Xbox }`
- `pub struct SessionInfo { pub id: String, pub owner: String, pub color: [u8; 3], pub family: Family, pub player: Option<u8>, pub connected: bool, pub experimental: bool, pub view_only: bool, pub battery: Option<u8>, pub charging: bool }` (`Clone, Debug, PartialEq, Serialize, Deserialize`)
- `pub struct LiveFrame { pub id: String, pub input: PadState, pub output: PadState, pub left: StickStages, pub right: StickStages, pub rate_hz: u32, pub latency_p99_us: u32, pub battery: Option<u8>, pub charging: bool }`
- `#[serde(tag = "t", rename_all = "snake_case")] pub enum ClientMsg { Hello { version: u32 }, Inspect { id: Option<String> }, SetPaused { paused: bool }, Restore }`
- `#[serde(tag = "t", rename_all = "snake_case")] pub enum ServerMsg { Welcome { version: u32, paused: bool, emulation: bool }, Incompatible { server_version: u32 }, Sessions { sessions: Vec<SessionInfo> }, Live { frame: LiveFrame }, Notice { text: String } }` (todas variantes struct, para o `tag` interno funcionar).
- `pub fn encode<T: Serialize>(msg: &T) -> String` (JSON + `\n`).
- `pub fn decode_client(line: &str) -> Result<ClientMsg, ProtocolError>` e `pub fn decode_server(line: &str) -> Result<ServerMsg, ProtocolError>`.
- `pub struct LineReader<R: Read> { … }` com `pub fn new(r: R) -> Self` e `pub fn next_line(&mut self) -> Result<Option<String>, ProtocolError>` (`None` = fim do fluxo; linha > `MAX_LINE` → `ProtocolError::TooLong`; UTF-8 inválido → `Malformed`).
- `#[derive(Debug, thiserror::Error, PartialEq)] pub enum ProtocolError { #[error("mensagem malformada: {0}")] Malformed(String), #[error("linha acima do limite")] TooLong, #[error("erro de E/S: {0}")] Io(String) }`
- Deps: `serde`, `serde_json = "1"` (adicionar ao workspace), `thiserror`. Sem dependência de outros crates do Axis (os tipos espelho evitam acoplamento; a conversão fica no núcleo).

- [ ] **Step 1:** branch `feat/axis-ui` a partir da `main`; testes que falham:

```rust
#[test] fn round_trip_every_message() { /* encode → decode == original para cada variante de ClientMsg e ServerMsg (com SessionInfo e LiveFrame preenchidos) */ }
#[test] fn encoded_message_is_one_line() { /* encode(..) termina com '\n' e não contém outro '\n' */ }
#[test] fn unknown_or_garbage_is_malformed() { /* decode_client("{\"t\":\"explode\"}") e decode_client("lixo") → Err(Malformed(_)) */ }
#[test] fn line_reader_splits_and_limits() { /* "a\nb\n" → Some("a"), Some("b"), None; uma linha de MAX_LINE+1 bytes sem '\n' → Err(TooLong); bytes 0xFF → Err(Malformed) */ }
#[test] fn hello_carries_the_version() { /* encode(Hello{version: PROTOCOL_VERSION}) contém "\"version\":1" */ }
```
- [ ] **Step 2:** RED (`cargo test -p axis-protocol` → erro de compilação). **Step 3:** implementar. **Step 4:** GREEN + clippy. **Step 5:** commit `feat(protocol): mensagens do núcleo para a interface e framing por linha`.

---

### Task 2: worker com amostra ao vivo (`LiveTap`)

**Files:** Create `crates/axis-daemon/src/live.rs`; Modify `worker.rs`

**Interfaces (Produces):**
- `#[derive(Clone, Copy, Debug, Default, PartialEq)] pub struct LiveSample { pub input: InputState, pub output: InputState, pub trace: Trace, pub status: DeviceStatus, pub reports: u64, pub latency_p99_us: u32, pub seq: u64 }` (`reports` = total de reports lidos; `latency_p99_us` = p99 da última janela de 60 s do histograma, 0 antes da primeira)
- `pub struct LiveTap { enabled: AtomicBool, sample: Mutex<LiveSample> }` com `pub fn new() -> Arc<LiveTap>`, `pub fn set_enabled(&self, on: bool)`, `pub fn enabled(&self) -> bool`, `pub fn latest(&self) -> LiveSample`, `pub(crate) fn publish(&self, s: LiveSample)`.
- `spawn_worker` ganha o parâmetro `tap: Arc<LiveTap>`. A cada report: se `tap.enabled()`, usa `pipeline.process_traced`, e publica no máximo a cada **16 ms** (guarda o `Instant` da última publicação; `seq` incrementa); senão, `process` como hoje. Ao perder o sinal ou sair, publica uma amostra com `output = InputState::default()` (e `input` default) para o palco não congelar.
- Sem alocação (tudo `Copy`); o `Mutex` da amostra é segurado só na cópia.

- [ ] **Step 1:** testes que falham (`worker.rs`):

```rust
#[test] fn tap_disabled_publishes_nothing() { /* reports com tap desligado → tap.latest().seq == 0 */ }
#[test] fn tap_enabled_publishes_traced_samples_throttled() { /* 20 reports em <16 ms → seq == 1; depois de 20 ms e mais um report → seq == 2; trace.left.raw == input.left */ }
#[test] fn silence_or_exit_publishes_neutral_sample() { /* tap ligado, report com left.x=1, depois silêncio > 500 ms → latest().output == InputState::default() */ }
```
- [ ] **Step 2:** RED. **Step 3:** implementar e atualizar todos os chamadores de `spawn_worker` (manager e testes) com `LiveTap::new()`. **Step 4:** GREEN (suíte inteira do `axis-daemon`). **Step 5:** commit `feat(daemon): amostra ao vivo por controle só quando inspecionado`.

---

### Task 3: registro para a interface (`UiRegistry`)

**Files:** Modify `crates/axis-daemon/src/live.rs`, `manager.rs`

**Interfaces (Produces):**
- `pub struct UiRegistry { state: Mutex<UiState>, changed: Condvar }` com `UiState { generation: u64, sessions: Vec<axis_protocol::SessionInfo>, taps: HashMap<String, Arc<LiveTap>>, paused: bool, emulation: bool, notices: Vec<String> }`.
  - `pub fn new() -> Arc<UiRegistry>`; `pub fn publish(&self, sessions, taps, paused, emulation)` (incrementa `generation` só se algo mudou; `notify_all`); `pub fn push_notice(&self, text)` (incrementa `generation`); `pub fn snapshot(&self) -> UiState` (clone); `pub fn wait_change(&self, since: u64, timeout: Option<Duration>) -> u64`.
- `Manager` recebe `ui: Arc<UiRegistry>` em `ManagerDeps` e chama `publish` ao fim de cada `handle` (sessões convertidas para `SessionInfo`, bateria/carregamento do `WorkerStatus` de cada sessão, `taps` por id). Cada sessão guarda o seu `Arc<LiveTap>`, criado no `arrive` (reaproveitado na reconexão).
- Conversões `pub(crate) fn to_pad_state(&InputState) -> PadState`, `to_stages(&StickTrace) -> StickStages`, `to_family(DeviceFamily) -> Family` em `live.rs`.
- As notificações do `Notifier` também vão para `ui.push_notice` (o tray continua mostrando o balão).

- [ ] **Step 1:** testes que falham:

```rust
#[test] fn publish_bumps_generation_only_on_change() { /* publish igual duas vezes → generation sobe 1 vez */ }
#[test] fn wait_change_returns_on_publish_and_times_out() { /* thread publica após 20 ms → wait_change(g, Some(1 s)) retorna g+1; sem publicação, timeout 30 ms → retorna g */ }
#[test] fn manager_publishes_sessions_with_battery_and_tap() { /* rig: chegada de 1 controle → ui.snapshot().sessions[0].owner == "Controle 1", taps contém o id */ }
#[test] fn conversions_map_buttons_and_stages() { /* InputState com South e left.x=0.5 → PadState.buttons bit 0, lx 0.5; StickTrace → StickStages campo a campo */ }
```
- [ ] **Step 2–5:** RED, implementar, GREEN, commit `feat(daemon): registro de sessões e torneiras para a interface`.

---

### Task 4: servidor do named pipe

**Files:** Create `crates/axis-daemon/src/ipc.rs`; Modify `app.rs`/`main.rs`

**Interfaces (Produces):**
- `pub fn spawn_ipc_server(pipe_name: &str, ui: Arc<UiRegistry>, events: Sender<Event>) -> JoinHandle<()>`: laço que cria instâncias com `CreateNamedPipeW(PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 4 instâncias, 64 KiB, 64 KiB)` e espera `ConnectNamedPipe` (bloqueante; `ERROR_PIPE_CONNECTED` = já conectado). Cada cliente ganha uma thread leitora e uma escritora.
  - Handshake: primeira linha do cliente deve ser `Hello`; versão diferente → envia `Incompatible { server_version }` e fecha; senão `Welcome` + `Sessions` imediatamente.
  - Leitora (`LineReader`): `Inspect { id }` liga a torneira do id (e desliga a anterior deste cliente); `SetPaused` → `Event::SetPaused`; `Restore` → `Event::RestoreControllers`; erro/linha longa/fim → encerra o cliente (desliga a torneira dele).
  - Escritora: sem inspeção, `wait_change(gen, None)` e envia `Sessions` (e cada `Notice` novo); com inspeção, `wait_change(gen, Some(16 ms))`, envia `Live { frame }` quando `seq` mudou e `Sessions` quando `generation` mudou. Erro de escrita → encerra.
  - Duas interfaces inspecionando o mesmo controle: a torneira só desliga quando nenhum cliente a usa (contador por id dentro do registro: `pub fn inspect(&self, id, on: bool)`).
  - `LiveFrame` vem da `LiveSample`: `rate_hz` = Δ`reports` / Δt entre amostras enviadas (média móvel de ~1 s), `latency_p99_us`, `battery`/`charging` do `status`.
- `main.rs` inicia o servidor depois do `Manager::new` com `axis_protocol::PIPE_NAME`.

- [ ] **Step 1:** testes de integração em `ipc.rs` com um nome de pipe único (`\\.\pipe\axis-test-<pid>-<n>`) e um cliente real (`CreateFileW` no pipe + `LineReader`):

```rust
#[test] fn handshake_welcome_then_sessions() { /* cliente envia Hello{1} → recebe Welcome e Sessions (lista do registro) */ }
#[test] fn wrong_version_gets_incompatible_and_close() { /* Hello{99} → Incompatible{server_version:1}, depois fim do fluxo */ }
#[test] fn inspect_enables_tap_and_streams_live_then_disconnect_disables() { /* registro com 1 tap; Inspect{Some(id)} → tap.enabled(); publicar amostras com seq crescente → cliente recebe Live; cliente fecha → tap desligada em ≤ 1 s */ }
#[test] fn oversized_or_garbage_line_drops_only_that_client() { /* cliente A manda 70 KiB sem '\n' → A desconectado; cliente B continua recebendo Sessions */ }
#[test] fn set_paused_and_restore_become_events() { /* SetPaused{true} e Restore → Receiver<Event> recebe SetPaused(true) e RestoreControllers */ }
```
- [ ] **Step 2–5:** RED, implementar, GREEN (3×, sem instabilidade), commit `feat(daemon): servidor de named pipe para a interface`.

---

### Task 5: `axis-ui`: estrutura, tema e fontes

**Files:** Create `crates/axis-ui/Cargo.toml`, `build.rs`, `src/main.rs`, `src/theme.rs`, `ui/tokens.slint`, `ui/fonts/*` (fornecidas pelo controlador)

**Interfaces (Produces):**
- `pub fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> [u8; 3]` (OKLCH → OKLab → LMS → sRGB linear → gama, com clamp).
- `pub const ACCENTS: [(&str, &str); 7]` (id, rótulo): `laranja`/Laranja (fixo `#d97757`), `vermelho` (h 27, c 0,17), `pink` (355, 0,16), `roxo` (305, 0,14), `indigo`/Índigo (275, 0,14), `azul` (245, 0,13), `verde` (150, 0,12), todos com L 0,67. `pub fn accent_rgb(id: &str) -> [u8; 3]` (id desconhecido → laranja).
- `ui/tokens.slint`: `export global Tokens { in-out property <bool> dark: true; in-out property <color> accent: #d97757; out property <color> bg: dark ? #1c1b1a : #f2f2f3; … surface, text, muted, divider (texto 15%), grid (texto 5%), accent-soft (accent 15%); radius-panel 18px, radius-btn 11px, radius-pill 999px; font-heading "Barlow Condensed", font-body "Barlow"; durações }`. Fontes importadas com `import "fonts/Barlow-Regular.ttf";` etc.
- `Cargo.toml`: `slint = { version = "1.18", default-features = false, features = ["std", "backend-winit", "renderer-software", "compat-1-2"] }`, `slint-build`, `axis-protocol`, `windows-sys` (workspace). Binário `axis-ui`, `#![windows_subsystem = "windows"]`.
- `main.rs` nesta task: só abre uma janela vazia com o fundo `Tokens.bg` e o título "Axis".

- [ ] **Step 1:** testes que falham (`theme.rs`):

```rust
#[test] fn oklch_known_values() { /* oklch(1,0,0) → [255,255,255]; oklch(0,0,0) → [0,0,0]; oklch(0.67, 0.13, 245) fica perto de um azul (b > r) */ }
#[test] fn accent_palette_has_seven_and_falls_back() { /* ACCENTS.len()==7; accent_rgb("laranja")==[0xd9,0x77,0x57]; accent_rgb("???")==accent_rgb("laranja") */ }
#[test] fn accents_share_lightness() { /* luminância relativa de todos (exceto laranja) dentro de ±0,08 entre si */ }
```
- [ ] **Step 2–5:** RED, implementar, GREEN; `cargo build -p axis-ui --release` abre a janela (verificação manual rápida pelo implementador: processo vivo 2 s); commit `feat(ui): estrutura do app Slint, tema e paleta de destaque`.

---

### Task 6: cliente do pipe e estado de conexão

**Files:** Create `crates/axis-ui/src/client.rs`, `src/view.rs`; Modify `src/main.rs`, `ui/app.slint`

**Interfaces (Produces):**
- `pub enum UiUpdate { Connected { paused: bool, emulation: bool }, Incompatible, Disconnected, Sessions(Vec<SessionInfo>), Live(LiveFrame), Notice(String) }`
- `pub struct Client { tx: Sender<ClientMsg> }` com `pub fn spawn(on_update: impl Fn(UiUpdate) + Send + 'static) -> Client` e `pub fn send(&self, m: ClientMsg)`. Thread: conecta (`CreateFileW` em `PIPE_NAME`; `ERROR_PIPE_BUSY` → `WaitNamedPipeW(2 s)`); falhou → `Disconnected` e nova tentativa a cada 1 s (é a única espera periódica do app, e só enquanto desconectado); conectado → `Hello`, lê com `LineReader`, traduz `ServerMsg` em `UiUpdate`; queda → `Disconnected` e volta a tentar. Mensagens de saída vão por uma segunda thread escritora.
- `pub fn start_daemon() -> bool`: inicia `axis.exe` da mesma pasta do `axis-ui.exe` com `--tray` (`std::process::Command`, sem janela); `false` se o arquivo não existe.
- `view.rs` (puro, testável): `pub fn session_line(s: &SessionInfo) -> String` ("Maria · DS4 · Player 1 · 64%" / "… · reconectando…" / "… · só visualização"); `pub fn family_label(f: &Family) -> &'static str` ("DS4", "DualSense", "Xbox"); `pub fn connection_message(state) -> &'static str` ("Conectando ao núcleo…", "O núcleo do Axis não está rodando", "Versão do núcleo incompatível; reinicie o Axis").
- `app.slint`: propriedade `connection: int` (0 conectando, 1 conectado, 2 núcleo parado, 3 incompatível) e uma tela de estado (mensagem + botão "Iniciar" no estado 2) sobre o conteúdo; callback `start-daemon()`.
- Ao iniciar a UI: se a primeira conexão falhar, chama `start_daemon()` uma vez e mostra "Conectando ao núcleo…" por até 3 s antes de "não está rodando".

- [ ] **Step 1:** testes que falham:

```rust
// client.rs, com um servidor falso de pipe no próprio teste (nome único via variável de ambiente AXIS_PIPE_NAME lida por client::pipe_name())
#[test] fn client_connects_hello_and_receives_sessions() { /* servidor falso: lê Hello, envia Welcome e Sessions → on_update recebe Connected e Sessions */ }
#[test] fn incompatible_server_is_reported() { /* servidor envia Incompatible → UiUpdate::Incompatible */ }
#[test] fn server_gone_reports_disconnected_then_reconnects() { /* servidor fecha → Disconnected; novo servidor no mesmo nome → Connected de novo em ≤ 2 s */ }
// view.rs
#[test] fn session_lines() { /* três casos acima, incluindo bateria None → sem o "· %" */ }
```
  (`pipe_name()` usa `AXIS_PIPE_NAME` se definido, senão `PIPE_NAME`; só testes definem a variável.)
- [ ] **Step 2–5:** RED, implementar, GREEN (3×), commit `feat(ui): cliente do núcleo com reconexão e estados de conexão`.

---

### Task 7: moldura (barra de título, barra lateral, controles, aparência)

**Files:** Create `ui/components.slint`; Modify `ui/app.slint`, `src/main.rs`, `src/view.rs`

- Barra de título própria (marca "AXIS" em Barlow Condensed, "Controller Bridge", status "ativo · N controles" ou "pausado"), botões minimizar, maximizar e fechar; a janela usa `no-frame: true` e arrasta pela barra.
- Barra lateral de 232 px: navegação (Visão geral ativa; Mapeamento, Correção e Perfis visíveis porém desabilitados com a dica "em breve"); seção "Controles" com um `PadCard` por sessão (bolinha na cor do dono, nome, `session_line`); clicar seleciona o controle e envia `Inspect { id }` (o primeiro conectado é selecionado ao receber `Sessions`); seção "Aparência" (Escuro/Claro e as 7 bolinhas de destaque, aplicadas na hora; gravar no `settings.toml` fica para o 5c).
- `main.rs`: `UiUpdate::Sessions` → `VecModel<PadCardData>` (recriado só quando a lista muda).
- [ ] **Step 1:** testes puros que falham em `view.rs`: `selected_defaults_to_first_connected` (`fn default_selection(&[SessionInfo]) -> Option<String>`), `pad_card_data_from_session` (cor, nome, linha). **Step 2–5:** RED, implementar, GREEN; captura de tela no modo demo anexada ao relatório; commit `feat(ui): moldura com controles e aparência`.

---

### Task 8: tela Visão geral

**Files:** Create `ui/controller-art.slint`, `ui/overview.slint`; Modify `ui/app.slint`, `src/main.rs`, `src/view.rs`

- **Visão geral** do controle selecionado:
  - cabeçalho: kicker "DISPOSITIVO 0N · USB/BLUETOOTH", título "Controle da Maria" (Barlow Condensed 38 px), linha "Emulando Xbox 360 via XInput" (ou "Só visualização");
  - **palco** (painel raio 18): grade de 24 px; `ControllerArt` do modelo (DS4: o desenho do spike `spikes/slint-stage/ui/app.slint`, refinado; DualSense e Xbox: variações do mesmo componente com formas de botões próprias; símbolos △ ○ □ ✕ desenhados como `Path`); analógicos, gatilhos e botões vivos a partir do `LiveFrame.output`; chamadas pontilhadas com valores; **layout responsivo**: o desenho escala para caber (mín. 1100×700, sem cortar chamadas);
  - "SEU CONTROLE → O JOGO RECEBE": dois plots lado a lado (entrada × saída do analógico esquerdo) e uma fileira de pílulas de etapa `calibração · drift · deadzone · curva · anti` destacadas quando aquela etapa mudou o valor (função pura `view::active_stages(&StickStages) -> [bool; 5]`: etapa ativa se o ponto depois dela difere do anterior em mais de 0,005);
  - painel de métricas: Latência (p99 em µs, "—" sem dado), Polling (`rate_hz`), Bateria ("—" ou %), gatilhos L2/R2 com barra;
  - estado "reconectando…" (sessão não conectada): palco esmaecido, valores em neutro.
- `main.rs`: `UiUpdate` → `slint::invoke_from_event_loop` → modelos (`VecModel<PadCardData>`, propriedades do palco). `Live` só atualiza propriedades (nada de recriar modelos a 60 Hz).

- [ ] **Step 1:** testes que falham (`view.rs`): `active_stages_marks_only_changing_steps`, `metrics_text` (`latency_text(0) == "—"`, `latency_text(48) == "48 µs"`, `battery_text(None) == "—"`), `header_texts` (kicker e subtítulo para BT/USB e view_only).
- [ ] **Step 2:** RED. **Step 3:** implementar telas e ligações. **Step 4:** GREEN; `cargo build -p axis-ui --release`; o implementador captura uma tela com dados de um núcleo falso (modo `AXIS_UI_DEMO=1` que alimenta `UiUpdate`s sintéticos sem pipe — só para captura e desenvolvimento) e anexa ao relatório. **Step 5:** commit `feat(ui): tela Visão geral ao vivo`.

---

### Task 9: "Abrir o Axis" na bandeja

**Files:** Modify `crates/axis-daemon/src/tray.rs`, `app.rs`

- `TrayCommand::OpenUi` e o item **"Abrir o Axis"** logo abaixo do título (negrito, `MFS_DEFAULT`); duplo clique no ícone também abre. Abre `axis-ui.exe` da mesma pasta do `axis.exe`; se já houver uma janela do Axis aberta (`FindWindowW` pela classe da janela Slint ou um mutex nomeado `Local\AxisUi` que a UI cria), traz para frente em vez de abrir outra. Arquivo ausente → notificação "Interface do Axis não encontrada".
- A UI cria o mutex `Local\AxisUi`; uma segunda UI aberta manda a primeira para frente (via `FindWindowW` + `SetForegroundWindow`) e sai.

- [ ] **Step 1:** testes puros: `menu_items_include_open_first` (layout do menu atualizado), `double_click_maps_to_open`. **Step 2–5:** RED, implementar, GREEN; fumaça: núcleo rodando, `WM_COMMAND` do item abre a UI (processo `axis-ui.exe` aparece); commit `feat(daemon): abrir a interface pela bandeja`.

---

### Task 10: verificação com o autor e medições

Tarefa do **controlador com o autor**.

- [ ] **Step 1:** com o núcleo rodando e o DS4 ligado: "Abrir o Axis" pela bandeja → janela em < 500 ms; o cartão "Controle 1 · DS4 · Player 1 · %"; o palco mexe junto com o controle; as pílulas acendem com drift/deadzone; desligar o DS4 → "reconectando…" e palco em neutro; religar → volta; fechar a UI → o núcleo continua (e a amostragem para: CPU do núcleo volta a ~0%); abrir a UI com o núcleo parado → inicia o núcleo sozinha.
- [ ] **Step 2:** medir: tempo até o primeiro quadro, RAM da UI aberta e parada, CPU da UI com o palco animando, CPU do núcleo inspecionando vs. sem UI. Registrar em `docs/benchmarks/ui.md` com uma captura de tela.
- [ ] **Step 3:** commit `test(ui): verificação com o autor e medições da interface`.
