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
pub fn header(index: usize, s: &SessionInfo, emulation: bool) -> Header {
    let subtitle = if !s.connected {
        "Reconectando…"
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
pub fn stage_status(connected: bool, has_live: bool) -> &'static str {
    match (connected, has_live) {
        (false, _) => "Reconectando…",
        (true, true) => "Ao vivo",
        (true, false) => "Aguardando dados",
    }
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
        let h = header(0, &maria(), true);
        assert_eq!(h.kicker, "DISPOSITIVO 01 · DS4");
        assert_eq!(h.title, "Controle de Maria");
        assert_eq!(h.subtitle, "Emulando Xbox 360 via XInput");

        let ds = SessionInfo {
            family: Family::DualSense,
            view_only: true,
            ..maria()
        };
        let h = header(2, &ds, true);
        assert_eq!(h.kicker, "DISPOSITIVO 03 · DUALSENSE");
        assert_eq!(h.subtitle, "Só visualização");

        // Núcleo sem barramento virtual: tudo é só visualização.
        assert_eq!(header(0, &maria(), false).subtitle, "Só visualização");

        let gone = SessionInfo {
            connected: false,
            family: Family::Xbox,
            ..maria()
        };
        let h = header(9, &gone, true);
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
        assert_eq!(header(0, &named("Controle 1"), true).title, "Controle 1");
        assert_eq!(
            header(0, &named("  controle azul "), true).title,
            "controle azul"
        );
        assert_eq!(header(0, &named("CONTROLE"), true).title, "CONTROLE");
        assert_eq!(header(0, &named("Maria"), true).title, "Controle de Maria");
        // Só o começo conta.
        assert_eq!(
            header(0, &named("Ana Controle"), true).title,
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
    fn stage_status_texts() {
        assert_eq!(stage_status(true, true), "Ao vivo");
        assert_eq!(stage_status(true, false), "Aguardando dados");
        assert_eq!(stage_status(false, true), "Reconectando…");
        assert_eq!(stage_status(false, false), "Reconectando…");
    }
}
