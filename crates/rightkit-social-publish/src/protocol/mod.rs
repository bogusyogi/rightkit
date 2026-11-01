//! Payload shapes shared by the social-publish modules. [`Scope`] is the exact key of a channel
//! (venture, channel, account); [`publish`] holds the durable publish job and the commands that drive it.

use serde::{Deserialize, Serialize};

pub mod accounts;
pub mod publish;
pub use accounts::*;
pub use publish::*;

/// The exact key of a channel: venture, channel and account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope {
    pub venture: String,
    pub channel: String,
    pub account: String,
}
