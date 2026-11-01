//! Terminal hyperlink rendering for the composer.
//!
//! The crate owns one canonical donor-derived hyperlink module at
//! [`crate::terminal::hyperlinks`] (Codex donor `terminal_hyperlinks`
//! @ `444da310`). This shim re-exports it so `composer::terminal_hyperlinks::*`
//! paths used by the lifted `textarea` code stay stable without a second,
//! diverging copy of the ~1300-line module.
//!
//! The composer-local seam note from the previous copy still applies: the
//! donor's `markdown_copy::CopyLine` provenance type is parent-owned; the
//! canonical module carries the adapted [`LogicalLineSource`] surface.

pub(crate) use crate::terminal::hyperlinks::*;
