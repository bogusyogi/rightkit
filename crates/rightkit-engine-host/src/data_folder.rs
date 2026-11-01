//! The isolated-data-folder rule for engine commands.
//!
//! An engine command that would otherwise use the user's real per-user data folder refuses when
//! the caller is not the user's own interactive tool: an explicit folder (`<PREFIX>_APP_DATA` or
//! `--data-dir`) always passes, a QA-marked process (`<PREFIX>_QA` set and not `0`) never
//! defaults to the real folder, a debug build never does either, and a release build run by the
//! user keeps the default.

use crate::EngineSpec;

/// Why an engine refused to default to the real per-user folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFolderRefusal {
    /// The full message, with the hint to pass an isolated folder.
    pub message: String,
}

/// The pure decision: `None` passes, `Some(reason)` refuses.
pub fn default_refusal(explicit: bool, qa_marked: bool, debug_build: bool) -> Option<&'static str> {
    if explicit {
        None
    } else if qa_marked {
        Some("the QA marker is set and no data folder was given; a test must never use the user's real data folder")
    } else if debug_build {
        Some("this is a debug build and no data folder was given; a debug engine never defaults to the user's real data folder")
    } else {
        None
    }
}

/// Whether `<PREFIX>_QA` marks this process as a test run (set, non-empty, not `0`).
pub fn qa_marked(spec: &EngineSpec) -> bool {
    std::env::var_os(spec.env_name("QA")).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Apply the rule to this process.
pub fn require_isolated(spec: &EngineSpec) -> Result<(), DataFolderRefusal> {
    let data_env = spec.env_name("APP_DATA");
    let explicit = std::env::var_os(&data_env).is_some_and(|v| !v.is_empty());
    match default_refusal(explicit, qa_marked(spec), cfg!(debug_assertions)) {
        None => Ok(()),
        Some(why) => Err(DataFolderRefusal {
            message: format!(
                "{why}; pass an isolated data folder: set {data_env}=<dir> or use --data-dir <dir>"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_always_passes_and_qa_or_debug_refuse() {
        assert_eq!(default_refusal(true, true, true), None);
        assert!(default_refusal(false, true, false).is_some());
        assert!(default_refusal(false, false, true).is_some());
        assert_eq!(default_refusal(false, false, false), None);
    }
}
