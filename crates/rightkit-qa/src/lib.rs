//! rightkit-qa: the Rust QA harness for Right Suite apps (macOS and Windows).
//!
//! This crate and the Node packages `@rightkit/qa` / `@rightkit/qa-chrome` are two runners
//! that coexist; neither replaces the other. This one drives native Rust, Tauri and engine
//! targets (the real macOS app, the Windows WebView2 app through its in-app control server,
//! the engine) with no Node or Python runtime. The Node packages own browser-page QA
//! (Chrome for Testing plus a CDP page driver, evidence, pane/frame/paint checks). See the
//! ownership table in this crate's README.
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
