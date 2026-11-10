//! Textos da interface derivados do estado (puro, sem Slint).

use axis_protocol::{Family, SessionInfo, StickStages};

/// Estado da conexão com o núcleo, como a tela o mostra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Stopped,
    Incompatible,
}

impl Connection {
    /// Valor da propriedade `connection` do `app.slint`.
    pub fn code(self) -> i32 {
        match self {
            Connection::Connecting => 0,
            Connection::Connected => 1,
            Connection::Stopped => 2,
            Connection::Incompatible => 3,
        }
    }
}

/// Mensagem da tela de estado; vazia quando conectado.
pub fn connection_message(state: Connection) -> &'static str {
    match state {
        Connection::Connecting => "Conectando ao núcleo…",
        Connection::Connected => "",
        Connection::Stopped => "O núcleo do Axis não está rodando",
        Connection::Incompatible => "Versão do núcleo incompatível; reinicie o Axis",
    }
}

pub fn family_label(f: &Family) -> &'static str {
    match f {
        Family::DualShock4 => "DS4",
        Family::DualSense => "DualSense",
        Family::Xbox => "Xbox",
    }
}

/// "Maria · DS4 · Player 1 · 64%"; desconectado: "… · reconectando…";
/// sem controle virtual: "… · só visualização".
pub fn session_line(s: &SessionInfo) -> String {
    format!("{} · {}", s.owner, details_line(s))
}

/// A linha da sessão sem o nome do dono: "DS4 · Player 1 · 64%".
fn details_line(s: &SessionInfo) -> String {
    let mut parts = vec![family_label(&s.family).to_string()];
    if let Some(p) = s.player {
        parts.push(format!("Player {p}"));
    }
    if !s.connected {
        parts.push("reconectando…".into());
    } else {
        if s.view_only {
            parts.push("só visualização".into());
        }
        if let Some(b) = s.battery {
            parts.push(format!("{b}%"));
        }
    }
    parts.join(" · ")
}

/// Cartão da barra lateral: bolinha na cor do dono, nome e a linha da sessão
/// (sem repetir o nome, que já fica em cima).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadCard {
    pub id: String,
    pub color: [u8; 3],
    pub owner: String,
    pub line: String,
    /// Linha completa, com o nome, para leitores de tela.
    pub label: String,
}

pub fn pad_card(s: &SessionInfo) -> PadCard {
    PadCard {
        id: s.id.clone(),
        color: s.color,
        owner: s.owner.clone(),
        line: details_line(s),
        label: session_line(s),
    }
}

/// O primeiro controle conectado, se houver.
pub fn default_selection(sessions: &[SessionInfo]) -> Option<String> {
    sessions.iter().find(|s| s.connected).map(|s| s.id.clone())
}

/// Seleção depois de uma nova lista: mantém a atual enquanto o controle
/// existir; senão, o primeiro conectado.
pub fn next_selection(current: Option<&str>, sessions: &[SessionInfo]) -> Option<String> {
    match current {
        Some(id) if sessions.iter().any(|s| s.id == id) => Some(id.to_string()),
        _ => default_selection(sessions),
    }
}

/// Estado na barra de título: "pausado" ou "ativo · N controles" (só os
/// conectados); sem barramento virtual, "só visualização · N controles".
pub fn status_line(paused: bool, emulation: bool, connected: usize) -> String {
    if paused {
        return "pausado".into();
    }
    let mode = if emulation {
        "ativo"
    } else {
        "só visualização"
    };
    match connected {
        1 => format!("{mode} · 1 controle"),
        n => format!("{mode} · {n} controles"),
    }
}

/// Quantos controles estão conectados agora.
pub fn connected_count(sessions: &[SessionInfo]) -> usize {
    sessions.iter().filter(|s| s.connected).count()
}

/// Diferença mínima entre etapas para a pílula acender.
const STAGE_EPSILON: f32 = 0.005;

/// Etapas `calibração · drift · deadzone · curva · anti` que mudaram o ponto:
/// cada uma comparada com a anterior (a primeira com o valor cru).
pub fn active_stages(s: &StickStages) -> [bool; 5] {
    let chain = [s.raw, s.calibrated, s.drift, s.deadzone, s.curve, s.anti];
    std::array::from_fn(|i| {
        let (a, b) = (chain[i], chain[i + 1]);
        // NaN compara falso: valor inválido não acende a etapa.
        (a[0] - b[0]).abs() > STAGE_EPSILON || (a[1] - b[1]).abs() > STAGE_EPSILON
    })
}

/// Latência p99; "—" sem medida (0).
pub fn latency_text(us: u32) -> String {
    if us == 0 {
        "—".into()
    } else {
        format!("{us} µs")
    }
}

/// Taxa de relatórios do controle; "—" sem medida (0).
pub fn rate_text(hz: u32) -> String {
    if hz == 0 {
        "—".into()
    } else {
        format!("{hz} Hz")
    }
}

pub fn battery_text(battery: Option<u8>) -> String {
    battery.map_or_else(|| "—".into(), |b| format!("{b}%"))
}

/// Gatilho de 0 a 1 em porcentagem inteira ("37%").
pub fn percent_text(v: f32) -> String {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    format!("{}%", (v * 100.0).round() as u32)
}

/// Um eixo com duas casas, vírgula decimal e sinal de menos tipográfico.
fn axis_text(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.2}");
    // "-0.00" vira "0,00".
    let s = if s.trim_start_matches('-') == "0.00" {
        "0.00".to_string()
    } else {
        s
    };
    s.replace('.', ",").replace('-', "−")
}

/// Posição de um analógico: "0,42 · −0,10".
pub fn stick_text(x: f32, y: f32) -> String {
    format!("{} · {}", axis_text(x), axis_text(y))
}

/// Cabeçalho da Visão geral do controle selecionado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub kicker: String,
    pub title: String,
    pub subtitle: String,
}

/// `index` é a posição do controle na lista (0 → "DISPOSITIVO 01").
/// O protocolo não informa USB/Bluetooth; o kicker mostra a família.
/// Pausado, o subtítulo diz com todas as letras que o jogo não recebe nada.
pub fn header(index: usize, s: &SessionInfo, emulation: bool, paused: bool) -> Header {
    let subtitle = if !s.connected {
        "Reconectando…"
    } else if paused {
        "Emulação pausada — o jogo não recebe este controle"
    } else if s.view_only || !emulation {
        "Só visualização"
    } else {
        "Emulando Xbox 360 via XInput"
    };
    Header {
        kicker: format!(
            "DISPOSITIVO {:02} · {}",
            index + 1,
            family_label(&s.family).to_uppercase()
        ),
        title: title(&s.owner),
        subtitle: subtitle.into(),
    }
}

/// "Controle de Maria"; um nome que já começa com "Controle" (o padrão
/// "Controle 1") fica como está.
fn title(owner: &str) -> String {
    let name = owner.trim();
    let prefix = "controle";
    let starts = name
        .get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix));
    if starts {
        name.to_string()
    } else {
        format!("Controle de {name}")
    }
}

/// Um quadro ao vivo só vale para o controle selecionado e conectado.
pub fn live_accepted(selected: Option<&str>, sessions: &[SessionInfo], frame_id: &str) -> bool {
    selected == Some(frame_id) && sessions.iter().any(|s| s.id == frame_id && s.connected)
}

/// Estado no canto do palco.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageBadge {
    /// Bolinha no destaque: quadros chegando e o jogo recebendo.
    Live,
    Waiting,
    Reconnecting,
    /// Pílula de aviso: o jogo não recebe este controle.
    Paused,
    ViewOnly,
}

impl StageBadge {
    pub fn text(self) -> &'static str {
        match self {
            StageBadge::Live => "Ao vivo",
            StageBadge::Waiting => "Aguardando dados",
            StageBadge::Reconnecting => "Reconectando…",
            StageBadge::Paused => "Pausado",
            StageBadge::ViewOnly => "Só visualização",
        }
    }

    /// Mostrado como pílula de aviso em vez da bolinha.
    pub fn is_pill(self) -> bool {
        matches!(self, StageBadge::Paused | StageBadge::ViewOnly)
    }

    /// Bolinha acesa no destaque.
    pub fn is_live(self) -> bool {
        self == StageBadge::Live
    }
}

/// Reconectando vence tudo (o palco nem tem dados); depois a pausa; depois
/// a só visualização (sem barramento virtual ou sessão sem controle virtual).
pub fn stage_badge(
    connected: bool,
    paused: bool,
    emulation: bool,
    view_only: bool,
    has_live: bool,
) -> StageBadge {
    if !connected {
        StageBadge::Reconnecting
    } else if paused {
        StageBadge::Paused
    } else if !emulation || view_only {
        StageBadge::ViewOnly
    } else if has_live {
        StageBadge::Live
    } else {
        StageBadge::Waiting
    }
}

/// A bolinha da barra de título só fica no destaque com a emulação ativa.
pub fn status_active(paused: bool, emulation: bool) -> bool {
    !paused && emulation
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maria() -> SessionInfo {
        SessionInfo {
            id: "aa:01".into(),
            owner: "Maria".into(),
            color: [1, 2, 3],
            family: Family::DualShock4,
            player: Some(1),
            connected: true,
            experimental: false,
            view_only: false,
            battery: Some(64),
            charging: false,
            transport: None,
            disconnects: 0,
            corrections: Default::default(),
        }
    }

    #[test]
    fn session_lines() {
        assert_eq!(session_line(&maria()), "Maria · DS4 · Player 1 · 64%");

        let reconnecting = SessionInfo {
            connected: false,
            ..maria()
        };
        assert_eq!(
            session_line(&reconnecting),
            "Maria · DS4 · Player 1 · reconectando…"
        );

        let view_only = SessionInfo {
            view_only: true,
            player: None,
            battery: None,
            family: Family::DualSense,
            ..maria()
        };
        assert_eq!(
            session_line(&view_only),
            "Maria · DualSense · só visualização"
        );

        let no_battery = SessionInfo {
            battery: None,
            family: Family::Xbox,
            ..maria()
        };
        assert_eq!(session_line(&no_battery), "Maria · Xbox · Player 1");
    }

    #[test]
    fn family_labels() {
        assert_eq!(family_label(&Family::DualShock4), "DS4");
        assert_eq!(family_label(&Family::DualSense), "DualSense");
        assert_eq!(family_label(&Family::Xbox), "Xbox");
    }

    #[test]
    fn connection_messages_and_codes() {
        assert_eq!(
            connection_message(Connection::Connecting),
            "Conectando ao núcleo…"
        );
        assert_eq!(
            connection_message(Connection::Stopped),
            "O núcleo do Axis não está rodando"
        );
        assert_eq!(
            connection_message(Connection::Incompatible),
            "Versão do núcleo incompatível; reinicie o Axis"
        );
        assert_eq!(connection_message(Connection::Connected), "");
        let codes = [
            Connection::Connecting,
            Connection::Connected,
            Connection::Stopped,
            Connection::Incompatible,
        ]
        .map(Connection::code);
        assert_eq!(codes, [0, 1, 2, 3]);
    }

    fn session(id: &str, connected: bool) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            connected,
            ..maria()
        }
    }

    #[test]
    fn selected_defaults_to_first_connected() {
        assert_eq!(default_selection(&[]), None);
        let all_down = [session("a", false), session("b", false)];
        assert_eq!(default_selection(&all_down), None);
        let mixed = [session("a", false), session("b", true), session("c", true)];
        assert_eq!(default_selection(&mixed), Some("b".to_string()));
    }

    #[test]
    fn selection_survives_session_updates() {
        let list = [session("a", true), session("b", false)];
        // A escolha do usuário fica enquanto o controle existir, mesmo reconectando.
        assert_eq!(next_selection(Some("b"), &list), Some("b".to_string()));
        // Sem escolha ou com o controle removido, volta ao primeiro conectado.
        assert_eq!(next_selection(None, &list), Some("a".to_string()));
        assert_eq!(next_selection(Some("zz"), &list), Some("a".to_string()));
        assert_eq!(next_selection(Some("a"), &[]), None);
    }

    #[test]
    fn pad_card_data_from_session() {
        let card = pad_card(&maria());
        assert_eq!(
            card,
            PadCard {
                id: "aa:01".into(),
                color: [1, 2, 3],
                owner: "Maria".into(),
                line: "DS4 · Player 1 · 64%".into(),
                label: "Maria · DS4 · Player 1 · 64%".into(),
            }
        );
        let reconnecting = pad_card(&SessionInfo {
            connected: false,
            ..maria()
        });
        assert_eq!(reconnecting.line, "DS4 · Player 1 · reconectando…");
    }

    #[test]
    fn status_texts() {
        assert_eq!(status_line(true, true, 2), "pausado");
        assert_eq!(status_line(false, true, 0), "ativo · 0 controles");
        assert_eq!(status_line(false, true, 1), "ativo · 1 controle");
        assert_eq!(status_line(false, true, 3), "ativo · 3 controles");
        // Sem barramento virtual: o núcleo só mostra os controles.
        assert_eq!(
            status_line(false, false, 2),
            "só visualização · 2 controles"
        );
        assert_eq!(status_line(true, false, 2), "pausado");
    }

    #[test]
    fn status_counts_only_connected() {
        let list = [session("a", true), session("b", false), session("c", true)];
        assert_eq!(connected_count(&list), 2);
        assert_eq!(connected_count(&[]), 0);
    }

    fn stages_from(points: [[f32; 2]; 6]) -> StickStages {
        StickStages {
            raw: points[0],
            calibrated: points[1],
            drift: points[2],
            deadzone: points[3],
            curve: points[4],
            anti: points[5],
        }
    }

    #[test]
    fn active_stages_marks_only_changing_steps() {
        let p = [0.5, -0.25];
        assert_eq!(active_stages(&stages_from([p; 6])), [false; 5]);

        // drift muda x; curva muda y; o resto repete o ponto anterior.
        let s = stages_from([
            [0.5, -0.25],
            [0.5, -0.25],
            [0.46, -0.25],
            [0.46, -0.25],
            [0.46, -0.2],
            [0.46, -0.2],
        ]);
        assert_eq!(active_stages(&s), [false, true, false, true, false]);

        // Até 0,005 não conta como mudança; acima conta.
        let tiny = stages_from([
            [0.0, 0.0],
            [0.004, 0.0],
            [0.004, -0.004],
            [0.004, 0.006],
            [0.004, 0.006],
            [0.0, 0.006],
        ]);
        assert_eq!(active_stages(&tiny), [false, false, true, false, false]);

        // Valores inválidos não acendem etapas.
        let nan = stages_from([[f32::NAN, 0.0]; 6]);
        assert_eq!(active_stages(&nan), [false; 5]);
    }

    #[test]
    fn metrics_text() {
        assert_eq!(latency_text(0), "—");
        assert_eq!(latency_text(48), "48 µs");
        assert_eq!(latency_text(1250), "1250 µs");
        assert_eq!(rate_text(0), "—");
        assert_eq!(rate_text(1000), "1000 Hz");
        assert_eq!(battery_text(None), "—");
        assert_eq!(battery_text(Some(64)), "64%");
        assert_eq!(percent_text(0.0), "0%");
        assert_eq!(percent_text(0.374), "37%");
        assert_eq!(percent_text(1.0), "100%");
        assert_eq!(percent_text(1.7), "100%");
        assert_eq!(percent_text(f32::NAN), "0%");
        assert_eq!(stick_text(0.4249, -0.1), "0,42 · −0,10");
        assert_eq!(stick_text(0.0, -0.001), "0,00 · 0,00");
        assert_eq!(stick_text(f32::NAN, 1.0), "0,00 · 1,00");
    }

    #[test]
    fn header_texts() {
        let h = header(0, &maria(), true, false);
        assert_eq!(h.kicker, "DISPOSITIVO 01 · DS4");
        assert_eq!(h.title, "Controle de Maria");
        assert_eq!(h.subtitle, "Emulando Xbox 360 via XInput");

        let ds = SessionInfo {
            family: Family::DualSense,
            view_only: true,
            ..maria()
        };
        let h = header(2, &ds, true, false);
        assert_eq!(h.kicker, "DISPOSITIVO 03 · DUALSENSE");
        assert_eq!(h.subtitle, "Só visualização");

        // Núcleo sem barramento virtual: tudo é só visualização.
        assert_eq!(
            header(0, &maria(), false, false).subtitle,
            "Só visualização"
        );

        let gone = SessionInfo {
            connected: false,
            family: Family::Xbox,
            ..maria()
        };
        let h = header(9, &gone, true, false);
        assert_eq!(h.kicker, "DISPOSITIVO 10 · XBOX");
        assert_eq!(h.subtitle, "Reconectando…");
    }

    #[test]
    fn title_does_not_repeat_controle() {
        let named = |owner: &str| SessionInfo {
            owner: owner.into(),
            ..maria()
        };
        // Nome padrão: "Controle 1", não "Controle de Controle 1".
        assert_eq!(
            header(0, &named("Controle 1"), true, false).title,
            "Controle 1"
        );
        assert_eq!(
            header(0, &named("  controle azul "), true, false).title,
            "controle azul"
        );
        assert_eq!(header(0, &named("CONTROLE"), true, false).title, "CONTROLE");
        assert_eq!(
            header(0, &named("Maria"), true, false).title,
            "Controle de Maria"
        );
        // Só o começo conta.
        assert_eq!(
            header(0, &named("Ana Controle"), true, false).title,
            "Controle de Ana Controle"
        );
    }

    #[test]
    fn live_frames_only_for_connected_selection() {
        let list = [session("a", true), session("b", false)];
        assert!(live_accepted(Some("a"), &list, "a"));
        assert!(!live_accepted(Some("a"), &list, "b"));
        assert!(!live_accepted(Some("b"), &list, "b"));
        assert!(!live_accepted(None, &list, "a"));
        assert!(!live_accepted(Some("zz"), &list, "zz"));
    }

    #[test]
    fn stage_badges() {
        use StageBadge::*;
        // stage_badge(connected, paused, emulation, view_only, has_live)
        assert_eq!(stage_badge(true, false, true, false, true), Live);
        assert_eq!(stage_badge(true, false, true, false, false), Waiting);
        // Pausa vence a falta de dados e a só visualização.
        assert_eq!(stage_badge(true, true, true, false, true), Paused);
        assert_eq!(stage_badge(true, true, true, false, false), Paused);
        assert_eq!(stage_badge(true, true, false, true, true), Paused);
        // Sem barramento virtual ou sessão sem controle virtual.
        assert_eq!(stage_badge(true, false, false, false, true), ViewOnly);
        assert_eq!(stage_badge(true, false, true, true, true), ViewOnly);
        // Reconectando vence tudo: o palco nem tem dados.
        assert_eq!(stage_badge(false, true, true, false, true), Reconnecting);
        assert_eq!(stage_badge(false, false, true, false, false), Reconnecting);
    }

    #[test]
    fn stage_badge_texts_and_styles() {
        use StageBadge::*;
        let all = [Live, Waiting, Reconnecting, Paused, ViewOnly];
        assert_eq!(
            all.map(StageBadge::text),
            [
                "Ao vivo",
                "Aguardando dados",
                "Reconectando…",
                "Pausado",
                "Só visualização"
            ]
        );
        // Pausado e só visualização viram pílula; o resto fica com a bolinha.
        assert_eq!(
            all.map(StageBadge::is_pill),
            [false, false, false, true, true]
        );
        assert_eq!(
            all.map(StageBadge::is_live),
            [true, false, false, false, false]
        );
    }

    #[test]
    fn paused_header_subtitle() {
        let paused = "Emulação pausada — o jogo não recebe este controle";
        assert_eq!(header(0, &maria(), true, true).subtitle, paused);
        // A pausa vale também com a emulação desligada ou só visualização.
        assert_eq!(header(0, &maria(), false, true).subtitle, paused);
        let gone = SessionInfo {
            connected: false,
            ..maria()
        };
        assert_eq!(header(0, &gone, true, true).subtitle, "Reconectando…");
    }

    #[test]
    fn status_dot_active_only_when_emulating() {
        assert!(status_active(false, true));
        assert!(!status_active(true, true));
        assert!(!status_active(false, false));
        assert!(!status_active(true, false));
    }
}
