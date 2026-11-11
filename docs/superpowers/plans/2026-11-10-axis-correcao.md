# Axis: Correção (kit de reparo) — plano de implementação 5b

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a tela **Correção** com os quatro assistentes da spec 7 (Drift, Alcance e círculo, Gatilhos, Conexão), em modo leigo e especialista, salvando o resultado no `ControllerRecord` e aplicando na hora no pipeline.

**Architecture:** a medição acontece **no núcleo**, na taxa real do controle: o worker entra em modo captura e acumula a entrada **crua**; a análise é pura, em `axis-core::repair`; o progresso e o resultado viajam no `LiveFrame` que a UI já recebe enquanto inspeciona. A UI só desenha, mostra os números e manda `SetCorrections`; o núcleo valida, grava (`store.save_record`), recompila e troca o pipeline (`WorkerCommand::SetPipeline`). As correções atuais chegam à UI dentro do `SessionInfo`.

**Tech Stack:** Rust 1.99 msvc, edition 2024; Slint 1.18 (software renderer); serde/serde_json; named pipe `\\.\pipe\axis-v1`.

**Spec:** `docs/superpowers/specs/2026-11-09-axis-design.md` (5.1, 6.1, 7, 8.2). Plano anterior: `docs/superpowers/plans/2026-11-10-axis-ui-overview.md`. Visual: `docs/spikes/fase-0-c-slint.md`, `docs/benchmarks/ui-overview-ds4.png`.

## Global Constraints

- Ordem fixa por analógico: `cru → calibração → drift → deadzone → curva → anti-deadzone`; gatilhos: `calibração (repouso, máximo) → deadzone → curva → hair trigger`.
- Correção pertence ao **controle físico** (`ControllerRecord.corrections`), nunca ao perfil.
- Limites de validação já existentes (`Corrections::validate`): `|center| < 0,5`; `outer_radius` em `[0,3; 1,5]` (16 setores, `axis_core::stages::calibration::SECTORS`); `drift_radius` em `[0; 0,5]`; gatilho `0 ≤ rest < max ≤ 1`.
- Honestidade (cópia exata): se `|centro| + raio de drift > 0,15`, mostrar **"Este analógico está desgastado: a compensação necessária passa de 15% do curso. Limpar ou trocar o módulo resolve de vez."**
- Conexão: sem RSSI. Mostra taxa (Hz), jitter, desconexões, bateria, USB/Bluetooth.
- Caminho crítico sem alocação: o buffer de captura é alocado **ao iniciar** a captura (comando), nunca por report.
- UI não depende de crates de hardware nem de `axis-daemon`; `axis-ui` pode depender de `axis-protocol` apenas (a análise fica no núcleo).
- Live ≤ 60/s; nada periódico no núcleo sem UI inspecionando.
- `PROTOCOL_VERSION` passa a **2** (UI e núcleo andam juntos; versão antiga vê "incompatível").
- Todas as cores/raios/fontes via `Tokens`; texto da interface em pt-BR; números com vírgula decimal.
- Commits Conventional, sem `Co-Authored-By` nem atribuição; branch `feat/axis-correcao`, merge local `--no-ff`.
- Testes nunca usam o pipe real, nunca iniciam `axis.exe`, nunca alteram HidHide nem a chave Run real; `axis-ui` só roda com `AXIS_UI_DEMO=1` fora da verificação com o autor.

## Review Focus

1. **Analógico mexido durante a medição de drift** → resultado `Rejected("O analógico mexeu durante a medição. Solte e tente de novo.")`, nada aplicado (teste em Task 1).
2. **Controle desconecta ou o worker sai no meio da captura** → estado `Aborted`, a UI volta ao passo inicial com "O controle desconectou durante a medição." (testes em Task 3 e Task 6).
3. **Captura incompleta** (menos de 3 voltas ou setores sem cobertura; gatilho apertado < 3 vezes) → após o limite de 30 s, `Incomplete` dizendo o que faltou; nunca aplica parcial (Task 1).
4. **Valor inválido no modo especialista** (vazio, texto, `NaN`, `rest ≥ max`, fora da faixa) → a UI recusa localmente com a mensagem do campo e o núcleo revalida e responde com aviso, mantendo a correção anterior (Tasks 4 e 7).
5. **Trocar de controle ou sair da tela durante a captura** → a UI manda `CancelCapture`; o resultado de outro id nunca é mostrado (Task 6).

---

## Estrutura de arquivos

| Arquivo | Responsabilidade |
|---|---|
| `crates/axis-core/src/repair.rs` (novo) | captura incremental e análise pura: drift, alcance, gatilhos |
| `crates/axis-protocol/src/lib.rs` | v2: `CorrectionsDto`, `Transport`, campos novos, mensagens de captura |
| `crates/axis-daemon/src/worker.rs` | modo captura, jitter |
| `crates/axis-daemon/src/live.rs` | `LiveSample.capture/jitter_us`, conversões core ↔ protocolo |
| `crates/axis-daemon/src/manager.rs` | aplicar correções, contar desconexões, transporte, rotear captura |
| `crates/axis-daemon/src/ipc.rs` | novas `ClientMsg` → eventos; `LiveFrame` com captura e jitter |
| `crates/axis-ui/src/repair_view.rs` (novo) | funções puras da tela Correção (textos, parse decimal, estados) |
| `crates/axis-ui/ui/repair.slint` (novo), `ui/repair-plots.slint` (novo) | tela e gráficos |
| `crates/axis-ui/src/main.rs`, `ui/app.slint`, `ui/overview.slint` | navegação, ligação, kicker USB/Bluetooth |

---

### Task 1: análise pura em `axis-core::repair`

**Files:** Create `crates/axis-core/src/repair.rs`; Modify `crates/axis-core/src/lib.rs` (`pub mod repair;`)

**Interfaces (Produces):**
- `pub enum Stick { Left, Right }`, `pub enum CaptureKind { Drift(Stick), Range(Stick), Triggers }` (Copy, Debug, PartialEq).
- `pub struct Capture` com `pub fn new(kind: CaptureKind, center: Vec2) -> Capture` (aloca: drift 4096, alcance 16384, gatilhos 16384 amostras; `center` = centro atual do analógico, usado pelo alcance), `pub fn push(&mut self, input: &InputState, t_secs: f32)` (sem alocação; ignora amostras além da capacidade), `pub fn progress(&self) -> f32` (0..=1), `pub fn is_done(&self) -> bool`, `pub fn finish(&self) -> Outcome`.
- Limites: drift termina em **3,0 s**; alcance termina com **≥ 3 voltas e os 16 setores cobertos** ou em 30 s; gatilhos terminam com **3 apertos em cada gatilho** ou em 30 s.
- `pub enum Outcome { Drift(DriftReport), Range(RangeReport), Triggers(TriggerReport), Rejected(&'static str), Incomplete(&'static str), Aborted }` (`Aborted` nunca sai de `finish()`; o worker o usa na Task 3)
- `pub struct DriftReport { pub center: Vec2, pub noise: f32, pub radius: f32, pub worn: bool, pub samples: u32 }` — `center` = média; `noise` = p99 da distância à média; `radius = clamp(noise·1,25 + 0,02, 0,03, 0,5)`; `worn = center.length() + radius > 0,15`; rejeita se `noise > 0,25` ("O analógico mexeu durante a medição. Solte e tente de novo.") ou `|center| ≥ 0,5`.
- `pub struct RangeReport { pub outer_radius: [f32; 16], pub circularity_pct: f32 }` — por setor (índice como em `calibrate`: ângulo `atan2(y,x)` em [0, 2π) / (2π/16)), o máximo de `|cru − center|`, limitado a `[0,3; 1,5]`; `circularity_pct = média(|r_i − média(r)|) / média(r) · 100`. Voltas = ângulo acumulado (desembrulhado) de amostras com magnitude > 0,5, dividido por 2π. Incompleto: "Faltou girar até a borda em todas as direções." Setor só conta com magnitude > 0,5.
- `pub struct TriggerReport { pub l2: Option<TriggerCalibration>, pub r2: Option<TriggerCalibration>, pub l2_presses: u8, pub r2_presses: u8 }` (de `crate::settings::TriggerCalibration`) — aperto = sobe acima de 0,8 do intervalo observado e volta abaixo de 0,2 (histerese); `rest` = máximo das amostras em repouso (abaixo de 0,2 do intervalo) + 0,01; `max` = máximo observado − 0,01; `None` para o gatilho com < 3 apertos ou `max − rest < 0,3`. Ambos `None` → `Incomplete("Aperte cada gatilho até o fim e solte, 3 vezes.")`.

- [ ] **Step 1:** testes que falham em `repair.rs`:

```rust
#[test] fn drift_finds_offset_and_noise() { /* 3 s de amostras em torno de (0,04, −0,03) com ruído ±0,01 a 250 Hz → center ≈ (0,04, −0,03) ±0,005; 0,03 ≤ radius ≤ 0,06; worn == false; progress()==1 e is_done() em t=3,0 */ }
#[test] fn drift_flags_worn_stick() { /* centro (0,12, 0) + ruído ±0,03 → worn == true */ }
#[test] fn drift_rejects_moving_stick() { /* metade das amostras em (0,6, 0) → Outcome::Rejected(msg) com a mensagem exata */ }
#[test] fn range_measures_sectors_and_circularity() { /* 3 voltas num quadrado de "gate" (magnitude 1/max(|cos|,|sin|) limitada a 1,3) → setores diagonais > setores cardeais; circularity_pct > 5; 3 voltas num círculo de raio 0,95 → todos ≈ 0,95 e circularity_pct < 1 */ }
#[test] fn range_incomplete_without_full_turns() { /* 1 volta e meia, t=30 → Incomplete com a mensagem exata; is_done() == true */ }
#[test] fn range_subtracts_center() { /* círculo raio 0,9 deslocado em (0,05, 0) com center (0,05,0) → todos ≈ 0,9 */ }
#[test] fn triggers_count_presses_and_measure_rest_max() { /* L2 repouso 0,04, pico 0,97, 3 apertos; R2 2 apertos → l2 = Some(rest≈0,05, max≈0,96), r2 = None, r2_presses == 2 */ }
#[test] fn push_never_grows_beyond_capacity() { /* 50 000 pushes em drift não entra em pânico e finish() funciona */ }
#[test] fn nan_samples_are_ignored() { /* amostras NaN no meio não alteram o resultado */ }
```
- [ ] **Step 2:** `cargo test -p axis-core repair` → falha de compilação (RED).
- [ ] **Step 3:** implementar.
- [ ] **Step 4:** `cargo test -p axis-core` verde; `cargo clippy -p axis-core --all-targets -- -D warnings`.
- [ ] **Step 5:** commit `feat(core): captura e análise para o kit de correção`.

---

### Task 2: protocolo v2

**Files:** Modify `crates/axis-protocol/src/lib.rs`

**Interfaces (Produces):**
- `PROTOCOL_VERSION = 2`.
- `pub enum Transport { Usb, Bluetooth }`.
- `pub struct StickCorrectionDto { pub center: [f32; 2], pub outer_radius: Vec<f32>, pub drift_radius: f32 }`, `pub struct TriggerCalDto { pub rest: f32, pub max: f32 }`, `pub struct CorrectionsDto { pub left: StickCorrectionDto, pub right: StickCorrectionDto, pub l2: TriggerCalDto, pub r2: TriggerCalDto }` (todos `Default` = valores neutros: centro 0, 16 × 1,0, drift 0, rest 0, max 1).
- `SessionInfo` ganha `transport: Option<Transport>` (None no Xbox/XInput), `disconnects: u32`, `corrections: CorrectionsDto`.
- `pub enum CaptureKindDto { DriftLeft, DriftRight, RangeLeft, RangeRight, Triggers }`.
- `pub enum CaptureResultDto { Drift { center: [f32; 2], noise: f32, radius: f32, worn: bool }, Range { outer_radius: Vec<f32>, circularity_pct: f32 }, Triggers { l2: Option<TriggerCalDto>, r2: Option<TriggerCalDto>, l2_presses: u8, r2_presses: u8 }, Rejected { reason: String }, Incomplete { reason: String }, Aborted }`.
- `pub struct CaptureState { pub kind: CaptureKindDto, pub progress: f32, pub result: Option<CaptureResultDto> }`.
- `LiveFrame` ganha `jitter_us: u32` e `capture: Option<CaptureState>`.
- `ClientMsg` ganha `StartCapture { id: String, kind: CaptureKindDto }`, `CancelCapture { id: String }`, `SetCorrections { id: String, corrections: CorrectionsDto }`.

- [ ] **Step 1:** testes: round-trip de cada mensagem nova e de `SessionInfo`/`LiveFrame` com os campos novos; `PROTOCOL_VERSION == 2`; `CorrectionsDto::default().left.outer_radius.len() == 16`; tag `"start_capture"` fixada em string.
- [ ] **Step 2–4:** RED, implementar, GREEN (`cargo test -p axis-protocol`). Os outros crates vão quebrar a compilação nos construtores de `SessionInfo`/`LiveFrame`: preencher os campos novos com valores neutros (`transport: None`, `disconnects: 0`, `corrections: Default`, `jitter_us: 0`, `capture: None`) só para compilar; `cargo test --workspace` verde.
- [ ] **Step 5:** commit `feat(protocol): versão 2 com correções e captura`.

---

### Task 3: worker em modo captura e jitter

**Files:** Modify `crates/axis-daemon/src/worker.rs`, `crates/axis-daemon/src/live.rs`

**Interfaces:**
- Consumes: `axis_core::repair::{Capture, CaptureKind, Outcome}` (Task 1).
- Produces: `WorkerCommand::StartCapture(Box<Capture>)` e `WorkerCommand::CancelCapture`; `LiveSample` ganha `capture: Option<CaptureSnapshot>` e `jitter_us: u32`, com `pub struct CaptureSnapshot { pub kind: CaptureKind, pub progress: f32, pub outcome: Option<Outcome> }` (em `live.rs`).
- Comportamento: com captura ativa, cada report chama `capture.push(&report.input /* cru */, t)`; ao `is_done()`, guarda `outcome = finish()` e solta o buffer. A captura roda mesmo pausado ou só visualização (usa só a entrada crua). Publicação continua sujeita à torneira (a UI inspeciona durante o assistente); o resultado final fica no `LiveSample` até um novo `StartCapture`/`CancelCapture`. Link caído (silêncio) ou saída do worker com captura ativa → `outcome = Some(Outcome::Aborted)`, publicado antes do neutro de saída.
- Jitter: anel fixo `[u32; 256]` de intervalos entre reports (µs); `jitter_us` = p99 de `|intervalo − média|` calculado só na hora de publicar (≤ 60/s), sem alocação (cópia em array na pilha).

- [ ] **Step 1:** testes com o `testkit` (controle falso):
  - `capture_collects_raw_input_and_reports_outcome` — inicia captura de drift com reports parados em (0,04, 0) por 3,1 s de tempo simulado → `LiveSample.capture.outcome` é `Drift` com centro ≈ 0,04, mesmo com o pipeline aplicando deadzone (prova que usa entrada crua).
  - `capture_aborted_on_link_down` — silêncio além de 500 ms durante captura → `Aborted`.
  - `cancel_clears_capture`.
  - `jitter_reflects_interval_spread` — intervalos alternando 4 ms/6 ms → `jitter_us` entre 800 e 1200; intervalos constantes → < 100.
- [ ] **Step 2–4:** RED, implementar, GREEN; `cargo test -p axis-daemon`.
- [ ] **Step 5:** commit `feat(daemon): captura no worker e jitter da conexão`.

---

### Task 4: manager e pipe — aplicar correções e rotear captura

**Files:** Modify `crates/axis-daemon/src/manager.rs`, `ipc.rs`, `live.rs`

**Interfaces:**
- Consumes: Tasks 2 e 3.
- Produces: `Event::StartCapture { id, kind: CaptureKindDto }`, `Event::CancelCapture { id }`, `Event::SetCorrections { id, corrections: CorrectionsDto }`; conversões em `live.rs`: `to_corrections_dto(&Corrections) -> CorrectionsDto`, `from_corrections_dto(&CorrectionsDto) -> Result<Corrections, String>` (16 setores exatos, senão erro), `to_capture_state(&CaptureSnapshot) -> CaptureState`.
- `SetCorrections`: converte, `Corrections::validate()`; erro → aviso `"Ajuste não aplicado: {campo} {motivo}"` e nada muda; ok → `record.corrections = c`, `store.save_record`, `Pipeline::compile(&c, &perfil ativo)`, `WorkerCommand::SetPipeline`, publica `Sessions` (com `corrections` novas). Erro de gravação → aviso `"Não foi possível salvar a correção"` mas o pipeline novo vale nesta sessão.
- `StartCapture`: monta `CaptureKind` + `center` do analógico a partir de `record.corrections` e envia `WorkerCommand::StartCapture(Box::new(Capture::new(..)))` ao worker da sessão conectada; id desconhecido ou desconectado → aviso `"Controle não está conectado"`.
- `SessionInfo`: `transport` vindo do `Controller::transport()` aberto (Xbox/XInput → `None`); `disconnects` = contador por id desde o início do núcleo (link caído + saída do worker não-`Shutdown`); `corrections` do record.
- `LiveFrame`: `jitter_us` e `capture` mapeados.
- ipc: `ClientMsg::StartCapture/CancelCapture/SetCorrections` → eventos acima.

- [ ] **Step 1:** testes:
  - manager: `set_corrections_validates_saves_and_swaps_pipeline` (record no store atualizado; worker recebeu `SetPipeline`; `Sessions` publicado com as correções); `invalid_corrections_are_rejected_with_notice` (rest ≥ max → aviso, record igual); `start_capture_uses_record_center`; `disconnects_are_counted`; `transport_is_reported`.
  - ipc (pipe real, nome único): `start_capture_message_reaches_manager`; `live_frame_carries_capture_state`.
  - live: round-trip `to_corrections_dto`/`from_corrections_dto`; `outer_radius` com 15 entradas → `Err`.
- [ ] **Step 2–4:** RED, implementar, GREEN; `cargo test -p axis-daemon`.
- [ ] **Step 5:** commit `feat(daemon): aplicar correções e medir pelo pipe`.

---

### Task 5: tela Correção — moldura, modos e ligação

**Files:** Create `crates/axis-ui/src/repair_view.rs`, `crates/axis-ui/ui/repair.slint`; Modify `src/main.rs`, `src/view.rs`, `ui/app.slint`, `ui/components.slint`, `ui/overview.slint`

**Interfaces (Produces):**
- Navegação: "Correção" habilitada (sem "em breve"); Mapeamento e Perfis continuam "em breve".
- Tela: cabeçalho como a Visão geral ("CORREÇÃO · {dono}"), alternador **Leigo / Especialista** (estado só da sessão da UI), e quatro cartões à esquerda: Drift, Alcance e círculo, Gatilhos, Conexão — cada um com a linha de estado de `repair_view::summary(kind, &CorrectionsDto)` (ex.: "Esquerdo: centro 0,04 · −0,03 · raio 0,05" / "Sem correção"); à direita o painel do assistente escolhido (Tasks 6–7 preenchem).
- Seletor de analógico **Esquerdo / Direito** nos assistentes de drift e alcance.
- `repair_view.rs` (puro): `pub fn parse_decimal(s: &str) -> Option<f32>` (aceita "0,06" e "0.06", recusa vazio/NaN/inf), `pub fn fmt2(v: f32) -> String` ("0,06", "−0,03"), `pub fn summary(...) -> String`, `pub fn capture_message(&CaptureResultDto) -> String` (cópias exatas da Review Focus e da spec), `pub enum WizardStep { Intro, Measuring, Result, Error }` e `pub fn step_for(state: Option<&CaptureState>, id_matches: bool) -> WizardStep`.
- Envio: "Começar" → `ClientMsg::StartCapture`; trocar de controle, de assistente ou de tela durante `Measuring` → `CancelCapture` (Review Focus 5); `Live` de outro id é ignorado (já existe).
- Visão geral: kicker passa a "DISPOSITIVO 0N · USB" / "· BLUETOOTH" quando `transport` existe.
- Demo (`AXIS_UI_DEMO=1`): `demo_updates()` ganha correções e o controle falso responde a `StartCapture` com progresso sintético até um resultado (drift ok; alcance ok; gatilhos com R2 incompleto), para as capturas de tela.

- [ ] **Step 1:** testes puros em `repair_view.rs`: `parse_decimal_accepts_comma_and_dot` ("0,06"→0,06; "0.06"→0,06; ""/"abc"/"NaN"→None); `fmt2_uses_comma_and_minus_sign`; `summary_texts`; `step_for_ignores_other_ids_and_maps_results`; `capture_messages_are_exact` (inclui a frase de desgaste da Global Constraints).
- [ ] **Step 2–4:** RED, implementar, GREEN; captura de tela em demo (padrão e 1100×700, só a janela do axis-ui via `PrintWindow`).
- [ ] **Step 5:** commit `feat(ui): tela Correção com modos leigo e especialista`.

---

### Task 6: assistentes de Drift e Alcance

**Files:** Create `crates/axis-ui/ui/repair-plots.slint`; Modify `ui/repair.slint`, `src/main.rs`, `src/repair_view.rs`

- **Drift:** Intro "Solte o analógico {esquerdo|direito} e não toque nele" + "Começar" → Medindo (barra de progresso 3 s; **nuvem de pontos** dos últimos 240 `input` crus recebidos nos `Live`, num quadro ±0,25 com cruz no centro) → Resultado: centro, ruído, raio sugerido (círculo desenhado na nuvem), aviso de desgaste quando `worn`.
- **Alcance e círculo:** Intro "Gire o analógico devagar encostando na borda, 3 voltas" → Medindo (rastro dos pontos num quadro ±1,3; contador "volta N de 3" a partir de `progress`) → Resultado: polígono dos 16 raios sobre o círculo ideal e "erro de circularidade X%".
- **Leigo:** resultado válido é aplicado automaticamente (`SetCorrections` com as correções atuais + só os campos do assistente) e mostra "Pronto: correção aplicada" com **Desfazer** (reenvia as correções de antes). **Especialista:** mostra os números editáveis (centro x/y, raio de drift; alcance: os 16 raios em grade 4×4) validados por `parse_decimal` + faixa da Global Constraints, e "Aplicar"; "Restaurar padrão" envia os valores neutros só daquele analógico/etapa.
- `Rejected`/`Incomplete`/`Aborted` → passo Erro com a mensagem e "Tentar de novo"; nunca aplica.
- `repair_view.rs`: `pub fn merge_drift(cur: &CorrectionsDto, stick, center, radius) -> CorrectionsDto`, `pub fn merge_range(cur, stick, outer_radius) -> CorrectionsDto`, `pub fn validate_field(name: &str, v: f32) -> Result<(), String>` (mensagens: "centro precisa estar entre −0,49 e 0,49", "raio de drift entre 0 e 0,5", "raio entre 0,3 e 1,5").

- [ ] **Step 1:** testes puros: `merge_drift_touches_only_that_stick`, `merge_range_keeps_center_and_drift`, `validate_field_ranges`, `aborted_goes_to_error_step`.
- [ ] **Step 2–4:** RED, implementar, GREEN; capturas em demo dos três passos de cada assistente.
- [ ] **Step 5:** commit `feat(ui): assistentes de drift e alcance`.

---

### Task 7: assistentes de Gatilhos e Conexão

**Files:** Modify `ui/repair.slint`, `ui/repair-plots.slint`, `src/main.rs`, `src/repair_view.rs`

- **Gatilhos:** Intro "Aperte L2 e R2 até o fim e solte, 3 vezes cada" → Medindo (duas barras verticais ao vivo com marcas de repouso/máximo detectados e "L2: 2 de 3 · R2: 3 de 3") → Resultado por gatilho (repouso, máximo); gatilho incompleto mostra "não medido" e não é alterado. Leigo aplica o que foi medido, com Desfazer; especialista edita `rest`/`max` (`rest < max`, ambos em [0; 1]) e "Restaurar padrão".
- **Conexão (passivo):** sem botão de medir; mostra ao vivo USB/Bluetooth, taxa (`rate_hz`), jitter (`jitter_us` em µs, "—" sem dado), desconexões desde que o núcleo iniciou, bateria e "carregando". Dica fixa: "O Windows não informa a força do sinal Bluetooth de forma confiável."
- `repair_view.rs`: `pub fn merge_triggers(cur, l2: Option<TriggerCalDto>, r2: Option<TriggerCalDto>) -> CorrectionsDto`, `pub fn jitter_text(us: u32) -> String`, `pub fn transport_text(Option<Transport>) -> &'static str` ("USB", "Bluetooth", "XInput").

- [ ] **Step 1:** testes puros: `merge_triggers_skips_unmeasured`, `trigger_field_rejects_rest_ge_max`, `jitter_and_transport_texts`.
- [ ] **Step 2–4:** RED, implementar, GREEN; capturas em demo.
- [ ] **Step 5:** commit `feat(ui): assistentes de gatilhos e conexão`.

---

### Task 8: verificação com o autor

Tarefa do **controlador com o autor**, com o DS4 por Bluetooth, núcleo e UI em release.

- [ ] **Step 1:** Drift no analógico esquerdo e direito (leigo): resultado plausível, aplicado, sobrevive a desligar/religar o controle e a reiniciar o núcleo (lido do `controllers/<id>.toml`); Desfazer volta.
- [ ] **Step 2:** mexer no analógico durante o drift → mensagem de "mexeu"; desligar o controle no meio → "desconectou".
- [ ] **Step 3:** Alcance (3 voltas) com erro de circularidade; Gatilhos (3 apertos); especialista: editar um número com vírgula e um inválido.
- [ ] **Step 4:** Conexão mostra Bluetooth, ~200 Hz, jitter e bateria; desconectar/religar incrementa "desconexões".
- [ ] **Step 5:** registrar em `docs/benchmarks/repair.md` (resultados do DS4 do autor + capturas) e marcar no `docs/hardware-checklist.md`; commit `test(repair): verificação com o autor`.
