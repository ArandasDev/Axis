#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{field}: {reason}")]
pub struct ValidationError {
    pub field: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SchemaError {
    #[error("schema version {found} is newer than the supported version {supported}")]
    TooNew { found: u32, supported: u32 },
    #[error("schema version 0 is invalid")]
    Zero,
}
