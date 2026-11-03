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
// Linux is no supported QA host (`util::platform` refuses it) and its unit tests are desktop-only, so desktop-only
// helpers read as unused there.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
//! container needs a caller-supplied ffmpeg. [`mock::MockServer`] stands in for paid providers.
/// Accessibility evidence for UI scenarios: contrast, focus indicator, role/name assertions.
pub mod a11y;
/// Android journeys through `adb` (emulator or USB device). Always compiled (it needs nothing beyond
/// this crate): the snapshot of an Android app's data (`[snapshot.android]`) uses it. The `android`
/// feature is kept as an empty alias.
pub mod android;
pub mod audio;
pub mod binary;
#[cfg(feature = "browser")]
pub mod browser;
pub mod budget;
pub mod check;
/// `rightkit-qa install`, `exec` and `proc`: the commands consumer scripts call (rightkit#430).
pub mod cli_tools;
pub mod config;
pub mod control;
/// macOS permission-dialog evidence.
pub mod dialog;
/// macOS display accessibility preferences and the scroller style, read and required for QA.
pub mod display_prefs;
/// QA-only in-app control server. Compiled only with the explicit `control` feature, which only an
/// app's `qa-native` feature enables. A dev build is the release profile plus `qa-native`
/// (GATE-CONTRACT GC-A07); `right-release` refuses to seal or
/// upload a release or update build whose binary carries `QA_CONTROL_MARKER`.
#[cfg(feature = "control")]
pub mod embedded;
pub mod engine;
pub mod evidence;
pub mod frametrace;
pub mod guard;
pub mod harness;
pub mod http;
/// Tier-2 `InApp` client for the embedded control server (`control` feature only).
#[cfg(feature = "control")]
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
/// Play an owner's recording as an app's microphone input through a virtual loopback device.
pub mod mic;
pub mod mock;
pub mod page;
pub mod plugin;
pub mod pool;
pub mod process;
pub mod retention;
/// `rightkit-qa run-tests`: compile test binaries in the broker, run them outside it, write one receipt.
pub mod run_tests;
pub mod scenario;
pub mod settings;
/// Signing evidence of a gate-signed QA run: parse `RIGHTKIT_QA_SIGNING_EVIDENCE` and require a launched file to hash to its signed record.
pub mod signing;
/// Snapshot and restore of the owner's real app data and credential items around a journey run (rightkit#1437).
pub mod snapshot;
/// The Android part of the snapshot: the app's private data dir through `run-as`.
pub(crate) mod snapshot_android;
pub mod steps;
pub mod storage;
pub mod suite;
pub mod supervise;
pub mod tart;
pub mod util;
pub mod worker;
pub mod worker_main;
pub mod workspace;

pub use util::{Error, Result};
