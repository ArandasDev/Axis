use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{SchemaError, ValidationError};
use crate::input::Button;
use crate::stages::deadzone::DeadzoneMode;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CurveSpec {
    Linear,
    Exponent { e: f32 },
    Points { points: Vec<[f32; 2]> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeadzoneSpec {
    pub mode: DeadzoneMode,
    pub inner: f32,
    pub outer: f32,
    pub anti: f32,
}

impl Default for DeadzoneSpec {
    fn default() -> Self {
        Self {
            mode: DeadzoneMode::Radial,
            inner: 0.05,
            outer: 1.0,
            anti: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickSettings {
    pub deadzone: DeadzoneSpec,
    pub curve: CurveSpec,
}

impl Default for StickSettings {
    fn default() -> Self {
        Self {
            deadzone: DeadzoneSpec::default(),
            curve: CurveSpec::Linear,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerSettings {
    pub inner: f32,
    pub curve: CurveSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hair_trigger: Option<f32>,
}

impl Default for TriggerSettings {
    fn default() -> Self {
        Self {
            inner: 0.0,
            curve: CurveSpec::Linear,
            hair_trigger: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub schema_version: u32,
    pub name: String,
    pub left_stick: StickSettings,
    pub right_stick: StickSettings,
    pub l2: TriggerSettings,
    pub r2: TriggerSettings,
    pub button_map: BTreeMap<Button, Button>,
    pub disabled_buttons: BTreeSet<Button>,
    pub vibration: f32,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: "Padrão".to_string(),
            left_stick: StickSettings::default(),
            right_stick: StickSettings::default(),
            l2: TriggerSettings::default(),
            r2: TriggerSettings::default(),
            button_map: BTreeMap::new(),
            disabled_buttons: BTreeSet::new(),
            vibration: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickCorrection {
    pub center: [f32; 2],
    pub outer_radius: [f32; 16],
    pub drift_radius: f32,
}

impl Default for StickCorrection {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0],
            outer_radius: [1.0; 16],
            drift_radius: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerCalibration {
    pub rest: f32,
    pub max: f32,
}

impl Default for TriggerCalibration {
    fn default() -> Self {
        Self {
            rest: 0.0,
            max: 1.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Corrections {
    pub left: StickCorrection,
    pub right: StickCorrection,
    pub l2: TriggerCalibration,
    pub r2: TriggerCalibration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceFamily {
    DualShock4,
    DualSense,
    Xbox,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControllerRecord {
    pub schema_version: u32,
    pub id: String,
    pub family: DeviceFamily,
    pub owner_name: String,
    pub owner_color: [u8; 3],
    pub active_profile: String,
    pub corrections: Corrections,
}

fn fail<T>(field: &str, reason: &str) -> Result<T, ValidationError> {
    Err(ValidationError {
        field: field.to_string(),
        reason: reason.to_string(),
    })
}

/// Checks that `v` is finite and within `[lo, hi]`.
fn check_range(field: &str, v: f32, lo: f32, hi: f32) -> Result<(), ValidationError> {
    if !v.is_finite() {
        return fail(field, "must be a finite number");
    }
    if v < lo || v > hi {
        return fail(field, &format!("must be within [{lo}, {hi}]"));
    }
    Ok(())
}

fn validate_deadzone(path: &str, d: &DeadzoneSpec) -> Result<(), ValidationError> {
    let inner = format!("{path}.inner");
    let outer = format!("{path}.outer");
    let anti = format!("{path}.anti");
    check_range(&inner, d.inner, 0.0, 1.0)?;
    check_range(&outer, d.outer, 0.0, 1.0)?;
    if d.inner >= d.outer {
        return fail(&inner, "must be below outer");
    }
    if !d.anti.is_finite() || d.anti < 0.0 || d.anti >= 1.0 {
        return fail(&anti, "must be within [0, 1)");
    }
    Ok(())
}

fn validate_curve(path: &str, c: &CurveSpec) -> Result<(), ValidationError> {
    match c {
        CurveSpec::Linear => Ok(()),
        CurveSpec::Exponent { e } => check_range(&format!("{path}.e"), *e, 0.2, 5.0),
        CurveSpec::Points { points } => {
            let field = format!("{path}.points");
            if points.is_empty() || points.len() > 8 {
                return fail(&field, "must have between 1 and 8 points");
            }
            let mut prev_x = 0.0_f32;
            let mut prev_y = 0.0_f32;
            for &[x, y] in points {
                if !x.is_finite() || !y.is_finite() {
                    return fail(&field, "coordinates must be finite");
                }
                if x <= prev_x || x >= 1.0 {
                    return fail(&field, "x must be strictly increasing within (0, 1)");
                }
                if y < prev_y || y > 1.0 {
                    return fail(&field, "y must be non-decreasing within [0, 1]");
                }
                prev_x = x;
                prev_y = y;
            }
            Ok(())
        }
    }
}

fn validate_stick(path: &str, s: &StickSettings) -> Result<(), ValidationError> {
    validate_deadzone(&format!("{path}.deadzone"), &s.deadzone)?;
    validate_curve(&format!("{path}.curve"), &s.curve)
}

fn validate_trigger(path: &str, t: &TriggerSettings) -> Result<(), ValidationError> {
    let inner = format!("{path}.inner");
    if !t.inner.is_finite() || t.inner < 0.0 || t.inner >= 1.0 {
        return fail(&inner, "must be within [0, 1)");
    }
    validate_curve(&format!("{path}.curve"), &t.curve)?;
    if let Some(h) = t.hair_trigger {
        let field = format!("{path}.hair_trigger");
        if !h.is_finite() || h <= 0.0 || h > 1.0 {
            return fail(&field, "must be within (0, 1]");
        }
    }
    Ok(())
}

fn validate_stick_correction(path: &str, c: &StickCorrection) -> Result<(), ValidationError> {
    for (i, v) in c.center.iter().enumerate() {
        let field = format!("{path}.center");
        if !v.is_finite() || v.abs() >= 0.5 {
            return fail(&field, &format!("component {i} must satisfy |c| < 0.5"));
        }
    }
    for (i, v) in c.outer_radius.iter().enumerate() {
        let field = format!("{path}.outer_radius");
        if !v.is_finite() || !(0.3..=1.5).contains(v) {
            return fail(&field, &format!("entry {i} must be within [0.3, 1.5]"));
        }
    }
    check_range(&format!("{path}.drift_radius"), c.drift_radius, 0.0, 0.5)
}

fn validate_trigger_calibration(path: &str, t: &TriggerCalibration) -> Result<(), ValidationError> {
    let rest = format!("{path}.rest");
    check_range(&rest, t.rest, 0.0, 1.0)?;
    check_range(&format!("{path}.max"), t.max, 0.0, 1.0)?;
    if t.rest >= t.max {
        return fail(&rest, "must be below max");
    }
    Ok(())
}

impl Profile {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema(self.schema_version)?;
        validate_stick("left_stick", &self.left_stick)?;
        validate_stick("right_stick", &self.right_stick)?;
        validate_trigger("l2", &self.l2)?;
        validate_trigger("r2", &self.r2)?;
        check_range("vibration", self.vibration, 0.0, 1.0)
    }
}

fn validate_schema(found: u32) -> Result<(), ValidationError> {
    check_schema(found).or_else(|e| fail("schema_version", &e.to_string()))
}

impl ControllerRecord {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema(self.schema_version)?;
        self.corrections.validate()
    }
}

/// Minimal view of a stored file used to read only its schema version.
/// Loaders deserialize this first and call [`check_schema`] before parsing
/// the full type, so a newer file is never misread or discarded as corrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct SchemaProbe {
    pub schema_version: u32,
}

impl Corrections {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_stick_correction("left", &self.left)?;
        validate_stick_correction("right", &self.right)?;
        validate_trigger_calibration("l2", &self.l2)?;
        validate_trigger_calibration("r2", &self.r2)
    }
}

pub fn check_schema(found: u32) -> Result<(), SchemaError> {
    match found {
        0 => Err(SchemaError::Zero),
        f if f > SCHEMA_VERSION => Err(SchemaError::TooNew {
            found: f,
            supported: SCHEMA_VERSION,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_and_corrections_are_valid() {
        assert_eq!(Profile::default().validate(), Ok(()));
        assert_eq!(Corrections::default().validate(), Ok(()));
    }

    #[test]
    fn inner_not_below_outer_is_rejected_with_field_path() {
        let mut p = Profile::default();
        p.left_stick.deadzone.inner = 0.9;
        p.left_stick.deadzone.outer = 0.9;
        assert_eq!(p.validate().unwrap_err().field, "left_stick.deadzone.inner");
    }

    #[test]
    fn curve_points_with_repeated_x_are_rejected() {
        let mut p = Profile::default();
        p.right_stick.curve = CurveSpec::Points {
            points: vec![[0.4, 0.2], [0.4, 0.5]],
        };
        assert_eq!(p.validate().unwrap_err().field, "right_stick.curve.points");
    }

    #[test]
    fn trigger_rest_not_below_max_is_rejected() {
        let c = Corrections {
            r2: TriggerCalibration {
                rest: 0.6,
                max: 0.6,
            },
            ..Corrections::default()
        };
        assert_eq!(c.validate().unwrap_err().field, "r2.rest");
    }

    #[test]
    fn nan_anywhere_is_rejected() {
        let p = Profile {
            vibration: f32::NAN,
            ..Profile::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn profile_round_trips_through_toml() {
        let mut p = Profile::default();
        p.button_map.insert(Button::South, Button::East);
        p.disabled_buttons.insert(Button::Touchpad);
        p.l2.hair_trigger = Some(0.1);
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Profile>(&text).unwrap(), p);
    }

    #[test]
    fn controller_record_round_trips_through_toml() {
        let r = ControllerRecord {
            schema_version: SCHEMA_VERSION,
            id: "a4:ae:12:00:be:ef".into(),
            family: DeviceFamily::DualSense,
            owner_name: "Maria".into(),
            owner_color: [95, 179, 122],
            active_profile: "Padrão".into(),
            corrections: Corrections::default(),
        };
        let text = toml::to_string(&r).unwrap();
        assert_eq!(toml::from_str::<ControllerRecord>(&text).unwrap(), r);
    }

    #[test]
    fn newer_schema_is_an_explicit_error() {
        assert_eq!(
            check_schema(SCHEMA_VERSION + 1),
            Err(SchemaError::TooNew {
                found: 2,
                supported: 1
            })
        );
        assert_eq!(check_schema(0), Err(SchemaError::Zero));
        assert_eq!(check_schema(1), Ok(()));
    }

    #[test]
    fn newer_schema_profile_is_rejected_by_validate() {
        let text = toml::to_string(&Profile::default())
            .unwrap()
            .replace("schema_version = 1", "schema_version = 2");
        let p = toml::from_str::<Profile>(&text).unwrap();
        assert_eq!(p.schema_version, 2);
        assert_eq!(p.validate().unwrap_err().field, "schema_version");
    }

    #[test]
    fn probe_reads_version_even_when_full_parse_fails() {
        let text = "schema_version = 2\n[left_stick.curve]\nkind = \"bezier\"\n";
        assert!(toml::from_str::<Profile>(text).is_err());
        let probe = toml::from_str::<SchemaProbe>(text).unwrap();
        assert_eq!(probe.schema_version, 2);
        assert_eq!(
            check_schema(probe.schema_version),
            Err(SchemaError::TooNew {
                found: 2,
                supported: 1
            })
        );
    }

    #[test]
    fn controller_record_validate_checks_schema_and_corrections() {
        let valid = ControllerRecord {
            schema_version: SCHEMA_VERSION,
            id: "a4:ae:12:00:be:ef".into(),
            family: DeviceFamily::DualSense,
            owner_name: "Maria".into(),
            owner_color: [95, 179, 122],
            active_profile: "Padrão".into(),
            corrections: Corrections::default(),
        };
        assert_eq!(valid.validate(), Ok(()));

        let newer = ControllerRecord {
            schema_version: 2,
            ..valid.clone()
        };
        assert_eq!(newer.validate().unwrap_err().field, "schema_version");

        let bad = ControllerRecord {
            corrections: Corrections {
                r2: TriggerCalibration {
                    rest: 0.6,
                    max: 0.6,
                },
                ..Corrections::default()
            },
            ..valid
        };
        assert_eq!(bad.validate().unwrap_err().field, "r2.rest");
    }
}
