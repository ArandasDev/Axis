pub mod error;
pub mod input;
pub mod settings;
pub mod stages;

pub use error::{SchemaError, ValidationError};
pub use input::{Button, Buttons, InputState, Vec2};
pub use settings::{
    ControllerRecord, Corrections, CurveSpec, DeadzoneSpec, DeviceFamily, Profile, SCHEMA_VERSION,
    StickCorrection, StickSettings, TriggerCalibration, TriggerSettings, check_schema,
};
