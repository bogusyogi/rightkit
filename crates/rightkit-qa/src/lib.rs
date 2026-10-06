//! rightkit-qa: the Rust QA harness for Right Suite apps (macOS and Windows).
//!
//! Replaces `@rightkit/qa` (embedded-WebDriver launch, run lock, evidence, guard,
//! frames/paint/panes) and the Node e2e harness pattern (`tests/e2e/lib.mjs`,
//! `ui.mjs`, `scripts/e2e.mjs`). Everything is Rust; no Node or Python runtime.
//!
//! Targets:
//! * engine: [`engine::EngineTarget`] drives the app engine over CLI words and the
//!   framed stdio sidecar.
//! * app UI: [`control::Control`] launches the real Tauri app hidden/backgrounded
//!   and drives it through the in-app `rightkit-control` server.
//! * Chrome: `browser` feature, through `rightkit-browser`.
//!
//! Scenarios are declarative TOML ([`scenario`]) run by the CLI, or plain Rust
//! integration tests that use the same library types directly.
pub mod audio;
#[cfg(feature = "browser")]
pub mod browser;
pub mod check;
pub mod config;
pub mod control;
pub mod engine;
pub mod evidence;
pub mod guard;
pub mod http;
pub mod lock;
pub mod media;
pub mod mock;
pub mod page;
pub mod process;
pub mod scenario;
pub mod suite;
pub mod util;
pub mod workspace;

pub use util::{Error, Result};
