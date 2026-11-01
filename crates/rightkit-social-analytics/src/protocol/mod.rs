//! Payload shapes for the social analytics modules. The channel [`Scope`] is the one
//! `rightkit-social-publish` defines.

pub mod analytics;
pub mod metric_import;
pub mod optimise;

pub use analytics::*;
pub use metric_import::*;
pub use optimise::*;
pub use rightkit_social_publish::protocol::Scope;
