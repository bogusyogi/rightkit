use std::fmt;

#[derive(Debug, Clone)]
pub enum PluginError {
    NotFound(String),
    Io(String),
    Parse(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::NotFound(msg) => write!(f, "Plugin not found: {}", msg),
            PluginError::Io(msg) => write!(f, "IO error: {}", msg),
            PluginError::Parse(msg) => write!(f, "Parse error: {}", msg),
        }
    }
}

impl std::error::Error for PluginError {}
