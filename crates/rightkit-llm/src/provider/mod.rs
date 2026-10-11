#[cfg(feature = "cli")]
pub mod cli;

use std::ops::ControlFlow;

use crate::error::LlmError;
use crate::types::{ChatRequest, ChatResponse, StreamEvent};

/// A chat-completion backend. Streaming callbacks return `Break` to cancel.
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError>;
    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError>;
}
