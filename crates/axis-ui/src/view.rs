//! Textos da interface derivados do estado (puro, sem Slint).

use axis_protocol::{Family, SessionInfo};

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

/// Estado na barra de título: "pausado" ou "ativo · N controles".
pub fn status_line(paused: bool, controllers: usize) -> String {
    match (paused, controllers) {
        (true, _) => "pausado".into(),
        (false, 1) => "ativo · 1 controle".into(),
        (false, n) => format!("ativo · {n} controles"),
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
        assert_eq!(status_line(true, 2), "pausado");
        assert_eq!(status_line(false, 0), "ativo · 0 controles");
        assert_eq!(status_line(false, 1), "ativo · 1 controle");
        assert_eq!(status_line(false, 3), "ativo · 3 controles");
    }
}
