#![forbid(unsafe_code)]

pub mod builder;
pub mod error;
pub mod l1_port;
pub mod l1b_port;
pub mod model;
pub mod receipt;
pub mod source;
pub mod token;

use std::sync::OnceLock;

/// Application-owned names the handoff prompts and validators use: the CLI
/// program and verb that run handoff scripts, and the product name shown to
/// the receiving chat. Declared once with [`set_handoff_host`]; otherwise the
/// neutral default (`rightkit script`, `RightKit`) applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffHost {
    /// Program that runs handoff scripts (first command token).
    pub program: String,
    /// Verb that selects script execution (second command token).
    pub script_verb: String,
    /// Product name shown in generated prompts.
    pub product: String,
}

impl Default for HandoffHost {
    fn default() -> Self {
        Self {
            program: "rightkit".to_string(),
            script_verb: "script".to_string(),
            product: "RightKit".to_string(),
        }
    }
}

impl HandoffHost {
    /// The command prefix for a handoff script, e.g. `rightkit script handoff/validate-handoff`.
    pub fn script_command(&self, script: &str) -> String {
        format!("{} {} {script}", self.program, self.script_verb)
    }
}

static HANDOFF_HOST: OnceLock<HandoffHost> = OnceLock::new();

/// Declare the application's handoff host. The first call wins; it returns
/// `false` when a host was already set.
pub fn set_handoff_host(host: HandoffHost) -> bool {
    HANDOFF_HOST.set(host).is_ok()
}

/// The handoff host in effect for this process.
pub fn handoff_host() -> &'static HandoffHost {
    HANDOFF_HOST.get_or_init(HandoffHost::default)
}

pub use builder::HandoffBuilder;
pub use error::{HandoffError, Result, SourceError, SourceErrorCode};
pub use model::{HandoffEntry, HandoffPacket, HandoffSection, Omission};
pub use receipt::HandoffReceipt;
pub use source::{
    ArtifactReader, EventCursor, EventPage, HandoffQuery, MemorySearch, Record, RecordCategory,
    RecordKind, RecordSource, SessionEventReader, SourceSet,
};
pub use token::{CharTokenizer, TiktokenTokenizer, TokenAccounting, TokenBudget, Tokenizer};
