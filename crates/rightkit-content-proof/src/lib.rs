//! `rightkit-content-proof`: store-free pieces of claim and proof review for published content.
//!
//! - [`numbers`]: exact decimal reading and tokenization of the numbers a claim states.
//!
//! - [`proof`]: claim review, review epochs, collected-proof expiry and the approval check over the
//!   host channel store.
//! - [`acquisition`]: the acquisition chain (request behind a recorded approval, import, labels,
//!   correlation, playbook, stages).
//! - [`protocol`]: their payload shapes.
//! - [`host`]: the seams the host registers (channel store opener, approver, engine name).
//!
//! The channel store itself belongs to the host app.

pub mod acquisition;
pub mod host;
pub mod numbers;
pub mod proof;
pub mod protocol;

/// An error: a stable machine-readable code and a human-readable message.
pub type Fail = rightkit_social_publish::Fail;
