//! Payload shapes for the signed webhook inbox ([`inbox`]) and DM automation ([`automation`]). The channel
//! [`Scope`] is the one `rightkit-social-publish` defines.

pub mod automation;
pub mod inbox;

pub use automation::*;
pub use inbox::*;
pub use rightkit_social_publish::protocol::Scope;
