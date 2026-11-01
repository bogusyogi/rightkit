//! Payload shapes for the content-plan modules. The channel [`Scope`] is the one
//! `rightkit-social-publish` defines.

pub mod calendar;
pub mod drafts;
pub mod hook_test;
pub mod variants;

pub use calendar::*;
pub use drafts::*;
pub use hook_test::*;
pub use rightkit_social_publish::protocol::Scope;
pub use variants::*;
