//! Windows launcher services for Start menu and Store apps, open windows, snippet expansion, type-ahead and launcher chat.
//! Install the application's services through `host` and its translation and logging through
//! rightkit-launcher. The application composes and styles its launcher.

#[cfg(windows)]
pub mod apps;

#[cfg(windows)]
pub mod chat;

#[cfg(windows)]
pub mod host;

#[cfg(windows)]
pub mod snippets;

#[cfg(windows)]
pub mod store;

#[cfg(windows)]
pub mod type_ahead;
