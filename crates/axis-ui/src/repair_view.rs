//! Textos e estados da tela Correção (puro, sem Slint).

use axis_protocol::{
    CaptureKindDto, CaptureResultDto, CaptureState, CorrectionsDto, StickCorrectionDto,
    TriggerCalDto,
};

/// Aviso de desgaste (cópia exata da spec): `|centro| + raio de drift > 0,15`.
pub const WORN: &str = "Este analógico está desgastado: a compensação necessária passa de 15% \
                        do curso. Limpar ou trocar o módulo resolve de vez.";
const ABORTED: &str = "O controle desconectou durante a medição.";
const NONE: &str = "Sem correção";

/// Os quatro assistentes, na ordem dos cartões da tela.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wizard {
    #[default]
    Drift,
    Range,
    Triggers,
    Connection,
}

impl Wizard {
    /// Posição do cartão (valor da propriedade `wizard` do Slint).
    pub fn index(self) -> i32 {
        match self {
            Wizard::Drift => 0,
            Wizard::Range => 1,
            Wizard::Triggers => 2,
            Wizard::Connection => 3,
        }
    }

    /// Índice fora da faixa volta ao primeiro assistente.
    pub fn from_index(i: i32) -> Wizard {
        match i {
            1 => Wizard::Range,
            2 => Wizard::Triggers,
            3 => Wizard::Connection,
            _ => Wizard::Drift,
        }
    }
}

/// Passo do assistente.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WizardStep {
    #[default]
    Intro,
    Measuring,
    Result,
    Error,
}

impl WizardStep {
    /// Valor da propriedade `repair-step` do Slint.
    pub fn code(self) -> i32 {
        match self {
            WizardStep::Intro => 0,
            WizardStep::Measuring => 1,
            WizardStep::Result => 2,
            WizardStep::Error => 3,
        }
    }
}

/// Número digitado no modo especialista: vírgula ou ponto decimal, sinal de
/// menos tipográfico aceito; vazio, texto, NaN e infinito são recusados.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "o modo especialista (Tasks 6–7) usa; tirar este atributo quando usar"
    )
)]
pub fn parse_decimal(s: &str) -> Option<f32> {
    let s = s.trim().replace('−', "-").replace(',', ".");
    if s.is_empty() || s.matches('.').count() > 1 {
        return None;
    }
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Duas casas, vírgula decimal e sinal de menos tipográfico ("−0,03");
/// "−0,00" vira "0,00"; valor inválido vira "—".
pub fn fmt2(v: f32) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    let s = format!("{v:.2}");
    let s = if s.trim_start_matches('-') == "0.00" {
        "0.00".to_string()
    } else {
        s
    };
    s.replace('.', ",").replace('-', "−")
}

fn sides<T>(left: T, right: T) -> [(&'static str, T); 2] {
    [("Esquerdo", left), ("Direito", right)]
}

fn drift_line(s: &StickCorrectionDto) -> Option<String> {
    let neutral = s.center == [0.0, 0.0] && s.drift_radius == 0.0;
    (!neutral).then(|| {
        format!(
            "centro {} · {} · raio {}",
            fmt2(s.center[0]),
            fmt2(s.center[1]),
            fmt2(s.drift_radius)
        )
    })
}

fn range_line(s: &StickCorrectionDto) -> Option<String> {
    let r = &s.outer_radius;
    if r.iter().all(|&v| v == 1.0) {
        return None;
    }
    let min = r.iter().copied().fold(f32::INFINITY, f32::min);
    let max = r.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    Some(format!("raios de {} a {}", fmt2(min), fmt2(max)))
}

fn trigger_line(t: &TriggerCalDto) -> String {
    format!("repouso {} · máximo {}", fmt2(t.rest), fmt2(t.max))
}

fn join_or_none(lines: Vec<String>) -> String {
    if lines.is_empty() {
        NONE.into()
    } else {
        lines.join("\n")
    }
}

/// Linha de estado do cartão de cada assistente: o que está aplicado agora.
pub fn summary(kind: Wizard, c: &CorrectionsDto) -> String {
    let stick = |line: fn(&StickCorrectionDto) -> Option<String>| {
        sides(&c.left, &c.right)
            .into_iter()
            .filter_map(|(name, s)| line(s).map(|l| format!("{name}: {l}")))
            .collect()
    };
    match kind {
        Wizard::Drift => join_or_none(stick(drift_line)),
        Wizard::Range => join_or_none(stick(range_line)),
        Wizard::Triggers => {
            let neutral = TriggerCalDto::default();
            let lines = [("L2", &c.l2), ("R2", &c.r2)]
                .into_iter()
                .filter(|(_, t)| **t != neutral)
                .map(|(name, t)| format!("{name}: {}", trigger_line(t)))
                .collect();
            join_or_none(lines)
        }
        Wizard::Connection => "Passivo: taxa, jitter e desconexões".into(),
    }
}

/// Mensagem do resultado de uma medição (as recusas vêm do núcleo já em
/// pt-BR e passam como estão).
pub fn capture_message(r: &CaptureResultDto) -> String {
    match r {
        CaptureResultDto::Drift {
            center,
            radius,
            worn,
            ..
        } => {
            let line = format!(
                "Centro {} · {} · raio sugerido {}",
                fmt2(center[0]),
                fmt2(center[1]),
                fmt2(*radius)
            );
            if *worn {
                format!("{line}\n{WORN}")
            } else {
                line
            }
        }
        CaptureResultDto::Range {
            circularity_pct, ..
        } => {
            let pct = if circularity_pct.is_finite() {
                format!("{circularity_pct:.1}").replace('.', ",")
            } else {
                "—".into()
            };
            format!("Erro de circularidade {pct}%")
        }
        CaptureResultDto::Triggers {
            l2,
            r2,
            l2_presses,
            r2_presses,
        } => [("L2", l2, l2_presses), ("R2", r2, r2_presses)]
            .into_iter()
            .map(|(name, cal, presses)| match cal {
                Some(t) => format!("{name}: {}", trigger_line(t)),
                None => format!("{name}: não medido ({presses} de 3 apertos)"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        CaptureResultDto::Rejected { reason } | CaptureResultDto::Incomplete { reason } => {
            reason.clone()
        }
        CaptureResultDto::Aborted => ABORTED.into(),
    }
}

/// Passo do assistente a partir do estado de captura do último quadro.
/// `id_matches` é falso quando o quadro é de outro controle: aí nada da
/// medição dele aparece.
pub fn step_for(state: Option<&CaptureState>, id_matches: bool) -> WizardStep {
    let Some(state) = state.filter(|_| id_matches) else {
        return WizardStep::Intro;
    };
    match &state.result {
        None => WizardStep::Measuring,
        Some(
            CaptureResultDto::Rejected { .. }
            | CaptureResultDto::Incomplete { .. }
            | CaptureResultDto::Aborted,
        ) => WizardStep::Error,
        Some(_) => WizardStep::Result,
    }
}

/// O que "Começar" mede; a Conexão é passiva e não mede nada.
pub fn capture_kind(w: Wizard, right: bool) -> Option<CaptureKindDto> {
    match (w, right) {
        (Wizard::Drift, false) => Some(CaptureKindDto::DriftLeft),
        (Wizard::Drift, true) => Some(CaptureKindDto::DriftRight),
        (Wizard::Range, false) => Some(CaptureKindDto::RangeLeft),
        (Wizard::Range, true) => Some(CaptureKindDto::RangeRight),
        (Wizard::Triggers, _) => Some(CaptureKindDto::Triggers),
        (Wizard::Connection, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axis_protocol::{
        CaptureKindDto, CaptureResultDto, CaptureState, CorrectionsDto, TriggerCalDto,
    };

    #[test]
    fn parse_decimal_accepts_comma_and_dot() {
        assert_eq!(parse_decimal("0,06"), Some(0.06));
        assert_eq!(parse_decimal("0.06"), Some(0.06));
        assert_eq!(parse_decimal(" −0,03 "), Some(-0.03));
        assert_eq!(parse_decimal("-1"), Some(-1.0));
        for bad in [
            "", "  ", "abc", "NaN", "nan", "inf", "-inf", "1,2,3", "0,0.6",
        ] {
            assert_eq!(parse_decimal(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fmt2_uses_comma_and_minus_sign() {
        assert_eq!(fmt2(0.06), "0,06");
        assert_eq!(fmt2(-0.03), "−0,03");
        assert_eq!(fmt2(1.0), "1,00");
        assert_eq!(fmt2(-0.001), "0,00");
        assert_eq!(fmt2(f32::NAN), "—");
        // O que sai de fmt2 volta por parse_decimal.
        assert_eq!(parse_decimal(&fmt2(-0.25)), Some(-0.25));
    }

    fn corrected() -> CorrectionsDto {
        let mut c = CorrectionsDto::default();
        c.left.center = [0.04, -0.03];
        c.left.drift_radius = 0.05;
        c.right.outer_radius[2] = 0.92;
        c.right.outer_radius[9] = 1.08;
        c.l2 = TriggerCalDto {
            rest: 0.04,
            max: 0.97,
        };
        c
    }

    #[test]
    fn summary_texts() {
        let neutral = CorrectionsDto::default();
        for w in [Wizard::Drift, Wizard::Range, Wizard::Triggers] {
            assert_eq!(summary(w, &neutral), "Sem correção");
        }
        let c = corrected();
        assert_eq!(
            summary(Wizard::Drift, &c),
            "Esquerdo: centro 0,04 · −0,03 · raio 0,05"
        );
        assert_eq!(summary(Wizard::Range, &c), "Direito: raios de 0,92 a 1,08");
        assert_eq!(
            summary(Wizard::Triggers, &c),
            "L2: repouso 0,04 · máximo 0,97"
        );
        // Os dois lados corrigidos: uma linha para cada.
        let mut both = c.clone();
        both.right.drift_radius = 0.1;
        both.r2 = TriggerCalDto {
            rest: 0.1,
            max: 0.9,
        };
        assert_eq!(
            summary(Wizard::Drift, &both),
            "Esquerdo: centro 0,04 · −0,03 · raio 0,05\nDireito: centro 0,00 · 0,00 · raio 0,10"
        );
        assert_eq!(
            summary(Wizard::Triggers, &both),
            "L2: repouso 0,04 · máximo 0,97\nR2: repouso 0,10 · máximo 0,90"
        );
        // A conexão é passiva: não tem correção.
        assert_eq!(
            summary(Wizard::Connection, &c),
            "Passivo: taxa, jitter e desconexões"
        );
        // Lista de raios vinda do pipe sem nada não quebra.
        let mut empty = CorrectionsDto::default();
        empty.left.outer_radius.clear();
        assert_eq!(summary(Wizard::Range, &empty), "Sem correção");
    }

    fn state(result: Option<CaptureResultDto>) -> CaptureState {
        CaptureState {
            kind: CaptureKindDto::DriftLeft,
            progress: 0.4,
            result,
        }
    }

    #[test]
    fn step_for_ignores_other_ids_and_maps_results() {
        assert_eq!(step_for(None, true), WizardStep::Intro);
        assert_eq!(step_for(Some(&state(None)), true), WizardStep::Measuring);
        // Quadro de outro controle: nunca mostra a medição dele.
        assert_eq!(step_for(Some(&state(None)), false), WizardStep::Intro);
        let ok = state(Some(CaptureResultDto::Drift {
            center: [0.0, 0.0],
            noise: 0.01,
            radius: 0.03,
            worn: false,
        }));
        assert_eq!(step_for(Some(&ok), true), WizardStep::Result);
        assert_eq!(step_for(Some(&ok), false), WizardStep::Intro);
        let range = state(Some(CaptureResultDto::Range {
            outer_radius: vec![1.0; 16],
            circularity_pct: 2.0,
        }));
        assert_eq!(step_for(Some(&range), true), WizardStep::Result);
        let triggers = state(Some(CaptureResultDto::Triggers {
            l2: None,
            r2: None,
            l2_presses: 0,
            r2_presses: 0,
        }));
        assert_eq!(step_for(Some(&triggers), true), WizardStep::Result);
        for bad in [
            CaptureResultDto::Rejected { reason: "x".into() },
            CaptureResultDto::Incomplete { reason: "y".into() },
            CaptureResultDto::Aborted,
        ] {
            assert_eq!(step_for(Some(&state(Some(bad))), true), WizardStep::Error);
        }
    }

    #[test]
    fn capture_messages_are_exact() {
        let moved = "O analógico mexeu durante a medição. Solte e tente de novo.";
        assert_eq!(
            capture_message(&CaptureResultDto::Rejected {
                reason: moved.into()
            }),
            moved
        );
        let missing = "Faltou girar até a borda em todas as direções.";
        assert_eq!(
            capture_message(&CaptureResultDto::Incomplete {
                reason: missing.into()
            }),
            missing
        );
        assert_eq!(
            capture_message(&CaptureResultDto::Aborted),
            "O controle desconectou durante a medição."
        );
        assert_eq!(
            capture_message(&CaptureResultDto::Drift {
                center: [0.04, -0.03],
                noise: 0.008,
                radius: 0.03,
                worn: false,
            }),
            "Centro 0,04 · −0,03 · raio sugerido 0,03"
        );
        let worn = capture_message(&CaptureResultDto::Drift {
            center: [0.12, 0.0],
            noise: 0.03,
            radius: 0.06,
            worn: true,
        });
        assert_eq!(
            worn,
            "Centro 0,12 · 0,00 · raio sugerido 0,06\nEste analógico está desgastado: a \
             compensação necessária passa de 15% do curso. Limpar ou trocar o módulo resolve \
             de vez."
        );
        assert_eq!(WORN, worn.lines().nth(1).unwrap());
        assert_eq!(
            capture_message(&CaptureResultDto::Range {
                outer_radius: vec![1.0; 16],
                circularity_pct: 4.24,
            }),
            "Erro de circularidade 4,2%"
        );
        assert_eq!(
            capture_message(&CaptureResultDto::Triggers {
                l2: Some(TriggerCalDto {
                    rest: 0.05,
                    max: 0.96
                }),
                r2: None,
                l2_presses: 3,
                r2_presses: 2,
            }),
            "L2: repouso 0,05 · máximo 0,96\nR2: não medido (2 de 3 apertos)"
        );
    }

    #[test]
    fn wizard_indexes_round_trip() {
        for w in [
            Wizard::Drift,
            Wizard::Range,
            Wizard::Triggers,
            Wizard::Connection,
        ] {
            assert_eq!(Wizard::from_index(w.index()), w);
        }
        assert_eq!(Wizard::from_index(-1), Wizard::Drift);
        assert_eq!(Wizard::from_index(99), Wizard::Drift);
        assert_eq!(
            [
                WizardStep::Intro,
                WizardStep::Measuring,
                WizardStep::Result,
                WizardStep::Error
            ]
            .map(WizardStep::code),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn capture_kind_follows_wizard_and_stick() {
        use CaptureKindDto::*;
        assert_eq!(capture_kind(Wizard::Drift, false), Some(DriftLeft));
        assert_eq!(capture_kind(Wizard::Drift, true), Some(DriftRight));
        assert_eq!(capture_kind(Wizard::Range, false), Some(RangeLeft));
        assert_eq!(capture_kind(Wizard::Range, true), Some(RangeRight));
        assert_eq!(capture_kind(Wizard::Triggers, true), Some(Triggers));
        assert_eq!(capture_kind(Wizard::Connection, false), None);
    }
}
