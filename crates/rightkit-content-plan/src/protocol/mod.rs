//! Payload shapes for the content-plan modules. The channel [`Scope`] is the one
//! `rightkit-social-publish` defines.

pub mod drafts;

pub use drafts::*;
pub use rightkit_social_publish::protocol::Scope;
