use std::fmt;

#[derive(Debug, Clone)]
pub enum PluginError {
    NotFound(String),
    Io(String),
    Parse(String),
    /// The request cannot be done; the text says why in plain words (contract R9).
    Refused(String),
    /// The plugin is already installed and the request did not ask to replace it.
    Conflict(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::NotFound(msg) => write!(f, "Plugin not found: {}", msg),
            PluginError::Io(msg) => write!(f, "IO error: {}", msg),
            PluginError::Parse(msg) => write!(f, "Parse error: {}", msg),
            PluginError::Refused(msg) | PluginError::Conflict(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for PluginError {}
