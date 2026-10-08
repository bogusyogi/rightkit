use std::fmt;

/// Why a review input or record was refused.
#[derive(Debug)]
pub enum ReviewError {
    /// A rule set is malformed: empty or duplicate ids, or a threshold outside its domain.
    InvalidRuleSet(String),
    /// A subject description cannot be interpreted.
    InvalidSubject(String),
    /// A repair cap above the hard limit was requested.
    RepairCapOutOfRange { requested: u32, max: u32 },
    /// A verdict record names another schema or a newer version than this build reads.
    UnsupportedRecord { schema: String, version: String },
    /// JSON did not parse into the expected shape.
    Json(serde_json::Error),
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReviewError::InvalidRuleSet(message) => write!(f, "invalid rule set: {message}"),
            ReviewError::InvalidSubject(message) => write!(f, "invalid subject: {message}"),
            ReviewError::RepairCapOutOfRange { requested, max } => {
                write!(f, "repair cap {requested} exceeds the hard limit of {max}")
            }
            ReviewError::UnsupportedRecord { schema, version } => {
                write!(f, "unsupported verdict record `{schema}` version {version}")
            }
            ReviewError::Json(error) => write!(f, "verdict record JSON: {error}"),
        }
    }
}

impl std::error::Error for ReviewError {}

impl From<serde_json::Error> for ReviewError {
    fn from(error: serde_json::Error) -> Self {
        ReviewError::Json(error)
    }
}
