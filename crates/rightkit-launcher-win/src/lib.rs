//! The Windows launcher panel over the portable core (`rightkit-launcher`): the layered search
//! window (`panel`), the in-launcher assistant (`chat`), snippet expansion (`snippets`),
//! the owner-side handover overlay (`spot_takeover`), Store apps (`store`) and the type-ahead
//! held for the panel while it opens (`type_ahead`, a handler on the `rightkit-hotkeys` hook hub).
//! Windows only; the crate is empty elsewhere.
//!
//! The application installs `host::PanelHost` for the panel's services (settings, the engine,
//! the hub, drives) and for what is its own: its product name, its pages, its usage
//! readings and its cleanup scan. It also installs the core's `rightkit_launcher::host::Host` for
//! translation, HTTPS and logging.

#[cfg(windows)]
pub mod chat;
#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod panel;
#[cfg(windows)]
pub mod snippets;
#[cfg(windows)]
pub mod spot_takeover;
#[cfg(windows)]
pub mod store;
#[cfg(windows)]
pub mod type_ahead;
