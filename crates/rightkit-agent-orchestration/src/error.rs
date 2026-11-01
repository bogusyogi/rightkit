//! Orchestration error types.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OrchestrationError {
    #[error("no agent available for: {0}")]
    NoAgent(String),
    #[error("governance denied: {0}")]
    Governance(String),
}
