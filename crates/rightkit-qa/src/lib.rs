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
//!   and drives it through the in-app server (`embedded`, feature `control`).
//! * Chrome: `browser` feature, through `rightkit-browser`.
//! * Android: `android::AndroidSession` (feature `android`) installs and launches the dev APK on an
//!   emulator or USB device through `adb`, records logcat by pid, and forwards the in-app control
//!   server to a loopback port.
//!
//! Scenarios are declarative TOML ([`scenario`]) run by the CLI, or plain Rust
//! integration tests wrapped by [`harness::Harness::scenario`] (tier gating, requires-skip,
//! per-scenario evidence, orphan check). Knobs reach managed (`rustright cargo`) test runs
//! through cargo features plus a [`settings::Settings`] file, never the caller's environment.
//!
//! Run kinds and evidence: a run is `functional` (the default: clicks, state and text, no
//! screenshots, no recordings) or `visual` (the audit-visual run that captures screens), see
//! [`storage::RunKind`]. Each run writes into its own directory under the managed runs root and
//! [`retention`] prunes only directories that carry the run marker.
//!
//! Validators: [`media`] (WAV stats, PNG/frame non-uniformity) and [`loudness`]
//! (BS.1770 integrated loudness, range, true peak), pure Rust except where a compressed
//! container needs a caller-supplied ffmpeg. [`mock::MockServer`] stands in for paid providers.
/// Accessibility evidence for UI scenarios: contrast, focus indicator, role/name assertions.
pub mod a11y;
/// Android journeys through `adb` (emulator or USB device); feature `android`.
#[cfg(feature = "android")]
pub mod android;
#[cfg(all(feature = "control", not(debug_assertions)))]
compile_error!("rightkit-qa control surface cannot be built without debug assertions");
pub mod audio;
pub mod binary;
#[cfg(feature = "browser")]
pub mod browser;
pub mod budget;
pub mod check;
pub mod config;
pub mod control;
/// QA-only in-app control server. Compiled only with the explicit `control` feature AND debug
/// assertions (dev builds, installed dev builds included); a release build cannot carry it.
#[cfg(all(feature = "control", debug_assertions))]
pub mod embedded;
pub mod engine;
pub mod evidence;
pub mod frametrace;
pub mod guard;
pub mod harness;
pub mod http;
/// Tier-2 `InApp` client for the embedded control server (`control` feature, debug builds only).
#[cfg(all(feature = "control", debug_assertions))]
pub mod in_app;
pub mod inplace;
pub mod installed;
/// Run a journey in the logged-on user's interactive session from a session-0 (SSH) process.
pub mod interactive;
/// End-to-end journeys: `journeys.toml`, adapter protocol v1 and the `rightkit.journeys.receipt/1` validator.
pub mod journeys;
pub mod lock;
pub mod loudness;
pub mod media;
pub mod mock;
pub mod page;
pub mod plugin;
pub mod pool;
pub mod process;
pub mod retention;
pub mod scenario;
pub mod settings;
pub mod steps;
pub mod storage;
pub mod suite;
pub mod supervise;
pub mod tart;
pub mod userdata;
pub mod util;
pub mod worker;
pub mod worker_main;
pub mod workspace;

pub use util::{Error, Result};
