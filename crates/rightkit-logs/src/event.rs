use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const EVENT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub schema_version: u16,
    pub ts_ms: i64,
    pub level: Level,
    pub event: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl EventRecord {
    pub fn new(ts_ms: i64, level: Level, event: impl Into<String>) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            ts_ms,
            level,
            event: event.into(),
            fields: Map::new(),
        }
    }

    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        let key = key.into();
        if !matches!(key.as_str(), "schema_version" | "ts_ms" | "level" | "event") {
            self.fields.insert(key, value);
        }
        self
    }
}
