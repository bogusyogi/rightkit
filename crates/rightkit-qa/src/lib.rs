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
//! integration tests wrapped by [`harness::Harness::scenario`] (tier gating, requires-skip,
//! per-scenario evidence, orphan check). Knobs reach managed (`rightkit cargo`) test runs
//! through cargo features plus a [`settings::Settings`] file, never the caller's environment.
//!
//! Validators: [`media`] (WAV stats, PNG/frame non-uniformity) and [`loudness`]
//! (BS.1770 integrated loudness, range, true peak), pure Rust except where a compressed
//! container needs a caller-supplied ffmpeg. [`mock::MockServer`] stands in for paid providers.
pub mod audio;
#[cfg(feature = "browser")]
pub mod browser;
pub mod check;
pub mod config;
pub mod control;
pub mod engine;
pub mod evidence;
pub mod guard;
pub mod harness;
pub mod http;
pub mod lock;
pub mod loudness;
pub mod media;
pub mod mock;
pub mod page;
pub mod process;
pub mod scenario;
pub mod settings;
pub mod storage;
pub mod suite;
pub mod util;
pub mod workspace;

pub use util::{Error, Result};
