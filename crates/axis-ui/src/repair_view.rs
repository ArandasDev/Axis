//! Textos e estados da tela Correção (puro, sem Slint).

use axis_protocol::{
    CaptureKindDto, CaptureResultDto, CaptureState, CorrectionsDto, StickCorrectionDto,
    TriggerCalDto,
};

/// Aviso de desgaste (cópia exata da spec): `|centro| + raio de drift > 0,15`.
pub const WORN: &str = "Este analógico está desgastado: a compensação necessária passa de 15% \
                        do curso. Limpar ou trocar o módulo resolve de vez.";
const ABORTED: &str = "O controle desconectou durante a medição.";
/// O núcleo caiu (a lista inteira de controles sumiu) no meio da medição.
pub const CORE_GONE: &str = "O núcleo do Axis parou durante a medição.";
/// Resultado que não dá para aplicar (lista de raios errada, NaN…).
const INVALID: &str = "Resultado inválido do núcleo.";
const NONE: &str = "Sem correção";
const NOT_A_NUMBER: &str = "digite um número, como 0,05";
/// Setores do alcance (`StickCorrectionDto::outer_radius`).
const SECTORS: usize = 16;

/// Analógico medido pelo assistente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stick {
    Left,
    Right,
}

impl Stick {
    pub fn from_right(right: bool) -> Stick {
        if right { Stick::Right } else { Stick::Left }
    }
}

fn stick_mut(c: &mut CorrectionsDto, s: Stick) -> &mut StickCorrectionDto {
    match s {
        Stick::Left => &mut c.left,
        Stick::Right => &mut c.right,
    }
}

/// As correções atuais com só o centro e o raio de drift daquele analógico
/// trocados.
pub fn merge_drift(
    cur: &CorrectionsDto,
    stick: Stick,
    center: [f32; 2],
    radius: f32,
) -> CorrectionsDto {
    let mut out = cur.clone();
    let s = stick_mut(&mut out, stick);
    s.center = center;
    s.drift_radius = radius;
    out
}

/// As correções atuais com só os raios do alcance daquele analógico trocados.
pub fn merge_range(cur: &CorrectionsDto, stick: Stick, outer_radius: &[f32]) -> CorrectionsDto {
    let mut out = cur.clone();
    stick_mut(&mut out, stick).outer_radius = outer_radius.to_vec();
    out
}

/// Faixa de cada campo do modo especialista (a mesma de
/// `Corrections::validate` no núcleo, que revalida).
pub fn validate_field(name: &str, v: f32) -> Result<(), String> {
    let (ok, msg) = match name {
        "center" => (v.abs() < 0.5, "centro precisa estar entre −0,49 e 0,49"),
        "drift_radius" => ((0.0..=0.5).contains(&v), "raio de drift entre 0 e 0,5"),
        "outer_radius" => ((0.3..=1.5).contains(&v), "raio entre 0,3 e 1,5"),
        _ => return Err(format!("campo desconhecido: {name}")),
    };
    // NaN falha nas comparações acima; infinito também fica fora.
    if ok && v.is_finite() {
        Ok(())
    } else {
        Err(msg.into())
    }
}

/// O resultado pode ser mostrado e aplicado: o núcleo é outro processo, então
/// a lista de raios e os números vêm sem garantia.
fn result_ok(r: &CaptureResultDto) -> bool {
    match r {
        CaptureResultDto::Drift {
            center,
            noise,
            radius,
            ..
        } => center.iter().chain([noise, radius]).all(|v| v.is_finite()),
        CaptureResultDto::Range { outer_radius, .. } => {
            outer_radius.len() == SECTORS && outer_radius.iter().all(|v| v.is_finite())
        }
        _ => true,
    }
}

/// Modo leigo: as correções a enviar para um resultado válido de drift ou de
/// alcance; recusas, abortos e resultados inválidos não aplicam nada.
pub fn auto_apply(
    cur: &CorrectionsDto,
    stick: Stick,
    r: &CaptureResultDto,
) -> Option<CorrectionsDto> {
    if !result_ok(r) {
        return None;
    }
    match r {
        CaptureResultDto::Drift { center, radius, .. } => {
            Some(merge_drift(cur, stick, *center, *radius))
        }
        CaptureResultDto::Range { outer_radius, .. } => Some(merge_range(cur, stick, outer_radius)),
        _ => None,
    }
}

/// "Restaurar padrão": valores neutros só daquele analógico e etapa.
pub fn neutral(cur: &CorrectionsDto, w: Wizard, stick: Stick) -> Option<CorrectionsDto> {
    match w {
        Wizard::Drift => Some(merge_drift(cur, stick, [0.0, 0.0], 0.0)),
        Wizard::Range => Some(merge_range(cur, stick, &[1.0; SECTORS])),
        Wizard::Triggers | Wizard::Connection => None,
    }
}

/// Um número editável do modo especialista.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub label: String,
    pub value: String,
}

const DRIFT_FIELDS: [(&str, &str); 3] = [
    ("center", "Centro x"),
    ("center", "Centro y"),
    ("drift_radius", "Raio de drift"),
];

/// Direção do setor `i` do alcance ("22,5°").
fn sector_label(i: usize) -> String {
    let deg = i as f32 * 360.0 / SECTORS as f32;
    if deg.fract() == 0.0 {
        format!("{deg:.0}°")
    } else {
        format!("{deg:.1}°").replace('.', ",")
    }
}

/// Campos do modo especialista preenchidos com o que foi medido.
pub fn fields_for(r: &CaptureResultDto) -> Vec<Field> {
    match r {
        CaptureResultDto::Drift { center, radius, .. } => DRIFT_FIELDS
            .iter()
            .zip([center[0], center[1], *radius])
            .map(|((_, label), v)| Field {
                label: (*label).into(),
                value: fmt2(v),
            })
            .collect(),
        CaptureResultDto::Range { outer_radius, .. } => outer_radius
            .iter()
            .enumerate()
            .map(|(i, v)| Field {
                label: sector_label(i),
                value: fmt2(*v),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Um campo digitado: número (vírgula ou ponto) dentro da faixa.
fn field_value(key: &str, text: &str) -> Result<f32, String> {
    let v = parse_decimal(text).ok_or_else(|| NOT_A_NUMBER.to_string())?;
    validate_field(key, v).map(|()| v)
}

/// "Aplicar" do modo especialista: todos os campos valem ou nada é aplicado;
/// o erro diz o índice do campo e a mensagem ("Centro y: …").
pub fn corrections_from_fields(
    cur: &CorrectionsDto,
    w: Wizard,
    stick: Stick,
    texts: &[String],
) -> Result<CorrectionsDto, (usize, String)> {
    let keyed: Vec<(&str, String)> = match w {
        Wizard::Drift => DRIFT_FIELDS
            .iter()
            .map(|(k, l)| (*k, (*l).to_string()))
            .collect(),
        Wizard::Range => (0..SECTORS)
            .map(|i| ("outer_radius", sector_label(i)))
            .collect(),
        Wizard::Triggers | Wizard::Connection => {
            return Err((0, "nada para aplicar aqui".into()));
        }
    };
    if texts.len() != keyed.len() {
        return Err((0, "campos incompletos; meça de novo".into()));
    }
    let mut values = Vec::with_capacity(texts.len());
    for (i, ((key, label), text)) in keyed.iter().zip(texts).enumerate() {
        let v = field_value(key, text).map_err(|e| (i, format!("{label}: {e}")))?;
        values.push(v);
    }
    Ok(match w {
        Wizard::Drift => merge_drift(cur, stick, [values[0], values[1]], values[2]),
        _ => merge_range(cur, stick, &values),
    })
}

/// Polígono dos 16 raios (comandos de `Path`, unidades do analógico, y para
/// cima vira y negativo); vazio para um resultado inválido.
pub fn range_polygon(r: &CaptureResultDto) -> String {
    let CaptureResultDto::Range { outer_radius, .. } = r else {
        return String::new();
    };
    if !result_ok(r) {
        return String::new();
    }
    let step = std::f32::consts::TAU / SECTORS as f32;
    let mut out = String::new();
    for (i, radius) in outer_radius.iter().enumerate() {
        let a = i as f32 * step;
        let (x, y) = (radius * a.cos(), -radius * a.sin());
        let cmd = if i == 0 { "M" } else { " L" };
        out.push_str(&format!("{cmd} {:.4} {:.4}", clean(x), clean(y)));
    }
    out.push_str(" Z");
    out
}

/// Zero sem sinal (um seno de 180° dá −8e−8).
fn clean(v: f32) -> f32 {
    if v.abs() < 5e-5 { 0.0 } else { v }
}

/// Contador do alcance a partir do progresso: "volta 2 de 3".
pub fn lap_text(progress: f32) -> String {
    let p = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lap = ((p * 3.0).floor() as u32 + 1).min(3);
    format!("volta {lap} de 3")
}

/// Aviso de desgaste do resultado do drift (cópia exata), à parte da mensagem.
pub fn capture_warning(r: &CaptureResultDto) -> Option<&'static str> {
    match r {
        CaptureResultDto::Drift { worn: true, .. } if result_ok(r) => Some(WORN),
        _ => None,
    }
}

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
    if !result_ok(r) {
        return INVALID.into();
    }
    match r {
        CaptureResultDto::Drift {
            center,
            noise,
            radius,
            ..
        } => format!(
            "Centro {} · {} · ruído {} · raio sugerido {}",
            fmt2(center[0]),
            fmt2(center[1]),
            fmt2(*noise),
            fmt2(*radius)
        ),
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
        Some(r) if !result_ok(r) => WizardStep::Error,
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
            "Centro 0,04 · −0,03 · ruído 0,01 · raio sugerido 0,03"
        );
        let worn = CaptureResultDto::Drift {
            center: [0.12, 0.0],
            noise: 0.03,
            radius: 0.06,
            worn: true,
        };
        assert_eq!(
            capture_message(&worn),
            "Centro 0,12 · 0,00 · ruído 0,03 · raio sugerido 0,06"
        );
        // O aviso de desgaste sai à parte (caixa de aviso), cópia exata.
        assert_eq!(
            capture_warning(&worn),
            Some(
                "Este analógico está desgastado: a compensação necessária passa de 15% do \
                 curso. Limpar ou trocar o módulo resolve de vez."
            )
        );
        assert_eq!(capture_warning(&worn), Some(WORN));
        assert_eq!(capture_warning(&CaptureResultDto::Aborted), None);
        assert_eq!(
            capture_warning(&CaptureResultDto::Drift {
                center: [0.0, 0.0],
                noise: 0.01,
                radius: 0.03,
                worn: false
            }),
            None
        );
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
    fn merge_drift_touches_only_that_stick() {
        let cur = corrected();
        let out = merge_drift(&cur, Stick::Right, [0.02, -0.01], 0.04);
        assert_eq!(out.right.center, [0.02, -0.01]);
        assert_eq!(out.right.drift_radius, 0.04);
        // O alcance do mesmo analógico, o outro analógico e os gatilhos ficam.
        assert_eq!(out.right.outer_radius, cur.right.outer_radius);
        assert_eq!(out.left, cur.left);
        assert_eq!((out.l2, out.r2), (cur.l2, cur.r2));
        let left = merge_drift(&cur, Stick::Left, [0.0, 0.0], 0.0);
        assert_eq!(left.left.center, [0.0, 0.0]);
        assert_eq!(left.left.drift_radius, 0.0);
        assert_eq!(left.right, cur.right);
    }

    #[test]
    fn merge_range_keeps_center_and_drift() {
        let cur = corrected();
        let radii: Vec<f32> = (0..16).map(|i| 0.9 + i as f32 * 0.01).collect();
        let out = merge_range(&cur, Stick::Left, &radii);
        assert_eq!(out.left.outer_radius, radii);
        assert_eq!(out.left.center, cur.left.center);
        assert_eq!(out.left.drift_radius, cur.left.drift_radius);
        assert_eq!(out.right, cur.right);
        assert_eq!((out.l2, out.r2), (cur.l2, cur.r2));
    }

    #[test]
    fn validate_field_ranges() {
        let center = "centro precisa estar entre −0,49 e 0,49";
        let drift = "raio de drift entre 0 e 0,5";
        let radius = "raio entre 0,3 e 1,5";
        for ok in [0.0, 0.49, -0.49, 0.499] {
            assert_eq!(validate_field("center", ok), Ok(()), "{ok}");
        }
        for bad in [0.5, -0.5, 1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(validate_field("center", bad), Err(center.into()), "{bad}");
        }
        for ok in [0.0, 0.03, 0.5] {
            assert_eq!(validate_field("drift_radius", ok), Ok(()), "{ok}");
        }
        for bad in [-0.01, 0.51, f32::NAN] {
            assert_eq!(validate_field("drift_radius", bad), Err(drift.into()));
        }
        for ok in [0.3, 1.0, 1.5] {
            assert_eq!(validate_field("outer_radius", ok), Ok(()), "{ok}");
        }
        for bad in [0.29, 1.51, f32::NEG_INFINITY, f32::NAN] {
            assert_eq!(validate_field("outer_radius", bad), Err(radius.into()));
        }
        assert!(validate_field("rest", 0.1).is_err());
    }

    #[test]
    fn aborted_goes_to_error_step() {
        let aborted = state(Some(CaptureResultDto::Aborted));
        assert_eq!(step_for(Some(&aborted), true), WizardStep::Error);
        assert_eq!(
            capture_message(&CaptureResultDto::Aborted),
            "O controle desconectou durante a medição."
        );
        assert_eq!(CORE_GONE, "O núcleo do Axis parou durante a medição.");
        // Recusas e abortos nunca viram correção aplicada.
        let cur = corrected();
        for bad in [
            CaptureResultDto::Aborted,
            CaptureResultDto::Rejected { reason: "x".into() },
            CaptureResultDto::Incomplete { reason: "y".into() },
        ] {
            assert_eq!(auto_apply(&cur, Stick::Left, &bad), None);
        }
    }

    #[test]
    fn untrusted_range_results_go_to_error() {
        let cur = corrected();
        for radii in [vec![1.0; 15], vec![1.0; 17], Vec::new(), {
            let mut v = vec![1.0; 16];
            v[4] = f32::NAN;
            v
        }] {
            let r = CaptureResultDto::Range {
                outer_radius: radii,
                circularity_pct: 1.0,
            };
            assert_eq!(
                step_for(Some(&state(Some(r.clone()))), true),
                WizardStep::Error
            );
            assert_eq!(capture_message(&r), "Resultado inválido do núcleo.");
            assert_eq!(auto_apply(&cur, Stick::Left, &r), None);
            assert_eq!(range_polygon(&r), "");
        }
        let nan_drift = CaptureResultDto::Drift {
            center: [f32::NAN, 0.0],
            noise: 0.01,
            radius: 0.03,
            worn: false,
        };
        assert_eq!(
            step_for(Some(&state(Some(nan_drift.clone()))), true),
            WizardStep::Error
        );
        assert_eq!(auto_apply(&cur, Stick::Left, &nan_drift), None);
    }

    #[test]
    fn auto_apply_merges_only_the_wizard_fields() {
        let cur = corrected();
        let drift = CaptureResultDto::Drift {
            center: [0.01, 0.02],
            noise: 0.008,
            radius: 0.03,
            worn: false,
        };
        assert_eq!(
            auto_apply(&cur, Stick::Right, &drift),
            Some(merge_drift(&cur, Stick::Right, [0.01, 0.02], 0.03))
        );
        let radii = vec![0.97; 16];
        let range = CaptureResultDto::Range {
            outer_radius: radii.clone(),
            circularity_pct: 2.0,
        };
        assert_eq!(
            auto_apply(&cur, Stick::Left, &range),
            Some(merge_range(&cur, Stick::Left, &radii))
        );
        // Neutro de cada etapa, só daquele analógico.
        let n = neutral(&cur, Wizard::Drift, Stick::Left).unwrap();
        assert_eq!(n.left.center, [0.0, 0.0]);
        assert_eq!(n.left.drift_radius, 0.0);
        assert_eq!(n.right, cur.right);
        let n = neutral(&cur, Wizard::Range, Stick::Right).unwrap();
        assert_eq!(n.right.outer_radius, vec![1.0; 16]);
        assert_eq!(n.right.drift_radius, cur.right.drift_radius);
        assert_eq!(neutral(&cur, Wizard::Triggers, Stick::Left), None);
    }

    #[test]
    fn expert_fields_round_trip() {
        let cur = corrected();
        let drift = CaptureResultDto::Drift {
            center: [0.04, -0.03],
            noise: 0.008,
            radius: 0.03,
            worn: false,
        };
        let f = fields_for(&drift);
        assert_eq!(
            f.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(),
            ["Centro x", "Centro y", "Raio de drift"]
        );
        let texts: Vec<String> = f.iter().map(|f| f.value.clone()).collect();
        assert_eq!(texts, ["0,04", "−0,03", "0,03"]);
        assert_eq!(
            corrections_from_fields(&cur, Wizard::Drift, Stick::Left, &texts),
            Ok(merge_drift(&cur, Stick::Left, [0.04, -0.03], 0.03))
        );
        // Ponto também vale; texto, vazio e fora da faixa apontam o campo.
        let typed = ["0.02".to_string(), "0".into(), "0,1".into()];
        assert_eq!(
            corrections_from_fields(&cur, Wizard::Drift, Stick::Left, &typed),
            Ok(merge_drift(&cur, Stick::Left, [0.02, 0.0], 0.1))
        );
        let bad = ["0,02".to_string(), "abc".into(), "0,1".into()];
        assert_eq!(
            corrections_from_fields(&cur, Wizard::Drift, Stick::Left, &bad),
            Err((1, "Centro y: digite um número, como 0,05".into()))
        );
        let far = ["0,6".to_string(), "0".into(), "0,1".into()];
        assert_eq!(
            corrections_from_fields(&cur, Wizard::Drift, Stick::Left, &far),
            Err((
                0,
                "Centro x: centro precisa estar entre −0,49 e 0,49".into()
            ))
        );
        let range = CaptureResultDto::Range {
            outer_radius: (0..16).map(|i| 0.9 + i as f32 * 0.01).collect(),
            circularity_pct: 2.0,
        };
        let f = fields_for(&range);
        assert_eq!(f.len(), 16);
        assert_eq!(f[0].label, "0°");
        assert_eq!(f[1].label, "22,5°");
        assert_eq!(f[4].label, "90°");
        assert_eq!(f[15].label, "337,5°");
        let mut texts: Vec<String> = f.iter().map(|f| f.value.clone()).collect();
        let out = corrections_from_fields(&cur, Wizard::Range, Stick::Right, &texts).unwrap();
        assert_eq!(out.right.outer_radius.len(), 16);
        assert!((out.right.outer_radius[15] - 1.05).abs() < 1e-6);
        assert_eq!(out.right.center, cur.right.center);
        texts[9] = "1,6".into();
        assert_eq!(
            corrections_from_fields(&cur, Wizard::Range, Stick::Right, &texts),
            Err((9, "202,5°: raio entre 0,3 e 1,5".into()))
        );
        // Quantidade errada de campos não aplica nada.
        assert!(corrections_from_fields(&cur, Wizard::Range, Stick::Right, &texts[..3]).is_err());
        assert!(corrections_from_fields(&cur, Wizard::Triggers, Stick::Left, &[]).is_err());
    }

    #[test]
    fn range_polygon_and_lap_texts() {
        let r = CaptureResultDto::Range {
            outer_radius: vec![1.0; 16],
            circularity_pct: 0.0,
        };
        let p = range_polygon(&r);
        // 16 vértices, o primeiro no ângulo 0 (direita) e y para cima.
        assert!(p.starts_with("M 1.0000 0.0000 L "), "{p}");
        assert!(p.contains("L 0.0000 -1.0000"), "{p}");
        assert!(p.ends_with(" Z"), "{p}");
        assert_eq!(p.matches(" L ").count(), 15);
        assert_eq!(lap_text(0.0), "volta 1 de 3");
        assert_eq!(lap_text(0.4), "volta 2 de 3");
        assert_eq!(lap_text(0.7), "volta 3 de 3");
        assert_eq!(lap_text(1.0), "volta 3 de 3");
        assert_eq!(lap_text(f32::NAN), "volta 1 de 3");
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
