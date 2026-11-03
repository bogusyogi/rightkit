//! The engine's data folder is the app's real per-user data folder (an explicit
//! `<PREFIX>_APP_DATA` or `--data-dir` still wins). No command refuses to use it: the owner's data is
//! the data, and QA protects it by snapshot and restore (`rightkit-qa`), not by redirecting it.

use crate::EngineSpec;

/// Whether `<PREFIX>_QA` marks this process as a QA run (set, non-empty, not `0`). Reported by
/// `doctor`; it changes no data location.
pub fn qa_marked(spec: &EngineSpec) -> bool {
    std::env::var_os(spec.env_name("QA")).is_some_and(|v| !v.is_empty() && v != "0")
}
