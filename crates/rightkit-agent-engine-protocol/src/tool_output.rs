//! Live tool output hook: a plain-data sink so a long-running process tool
//! (shell/exec) can report output chunks while it runs without owning any
//! session state. The engine installs a sink per tool call and turns the
//! (coalesced) chunks into `toolOutputDelta` protocol events.

use std::fmt;
use std::sync::Arc;

/// Which pipe a chunk was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutputStream {
    Stdout,
    Stderr,
}

impl ToolOutputStream {
    /// Wire spelling used by `toolOutputDelta.stream`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

type OutputFn = dyn Fn(ToolOutputStream, &str) + Send + Sync;

/// Cloneable, optional output sink. Default is a no-op, so a tool can call
/// [`ToolOutputNotifier::notify`] unconditionally.
#[derive(Clone, Default)]
pub struct ToolOutputNotifier(Option<Arc<OutputFn>>);

impl ToolOutputNotifier {
    pub fn new(f: impl Fn(ToolOutputStream, &str) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(f)))
    }

    pub fn is_set(&self) -> bool {
        self.0.is_some()
    }

    pub fn notify(&self, stream: ToolOutputStream, text: &str) {
        if let Some(f) = &self.0 {
            f(stream, text);
        }
    }
}

impl fmt::Debug for ToolOutputNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "ToolOutputNotifier(set)"
        } else {
            "ToolOutputNotifier(none)"
        })
    }
}

impl PartialEq for ToolOutputNotifier {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for ToolOutputNotifier {}
