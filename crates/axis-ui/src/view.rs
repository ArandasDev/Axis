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

#[allow(dead_code)] // usado pela lista de controles (próxima tarefa)
pub fn family_label(f: &Family) -> &'static str {
    match f {
        Family::DualShock4 => "DS4",
        Family::DualSense => "DualSense",
        Family::Xbox => "Xbox",
    }
}

/// "Maria · DS4 · Player 1 · 64%"; desconectado: "… · reconectando…";
/// sem controle virtual: "… · só visualização".
#[allow(dead_code)] // usado pela lista de controles (próxima tarefa)
pub fn session_line(s: &SessionInfo) -> String {
    let mut parts = vec![s.owner.clone(), family_label(&s.family).to_string()];
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
}
