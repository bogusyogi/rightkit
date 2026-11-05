//! Agent demo mode: a person consents, an agent drives one catalog app through `right-ctl demo`, and
//! the host application records that app's window into a sealed local bundle.
//!
//! Release gating: a host should depend on this crate only behind a non-default feature of its own, so a release build
//! contains none of it (the `demo.*` service, the kill hotkey, the badge window and the recorder).
//! The wire contract belongs to `rightkit-agent-tools` (feature `demo`); this crate reuses its types.
//!
//! The driver and the recorder live in this process: the kill switch is a process static, so a kill
//! here stops both. `right-ctl` is a thin client of the service and never drives for itself.
//!
//! | file | owns |
//! | --- | --- |
//! | `session.rs` | phase transitions & bundle ids; host uses RightKit session credentials |
//! | `host.rs` | consent, orchestration, stop, kill, seal |
//! | `service.rs` | the `rightkit.agent.demo` daemon handler |
//! | `driver.rs` | one automation engine per session, the policy, snapshot and act |
//! | `recorder.rs` | the recorder worker thread (capture, pump, events, verify) |
//! | `badge.rs`, `kill.rs`, `tray.rs` | the always-visible badge window, the kill hotkey and the tray "Stop demo" item |
//! | `catalog.rs`, `store.rs` | the app catalog and deny list; the flag, bundles and audit log |
//! | `commands.rs` | the Tauri commands the UI calls |
//!
//! Platform scope: the driver needs `rightkit-automation`'s native backend and the recorder needs
//! `rightkit-record`'s native backend, and both exist only on macOS and Windows. On other targets the
//! crate compiles to nothing, so no demo code is built there.

#![cfg(any(target_os = "macos", windows))]

use std::sync::{Arc, OnceLock};

use serde::Serialize;
use tauri::{Manager, Wry};

pub mod badge;
pub mod catalog;
pub mod commands;
pub mod driver;
pub mod host;
pub mod kill;
pub mod recorder;
pub mod service;
pub mod session;
pub mod store;
pub mod tray;

pub use host::DemoHost;
use session::Phase;

/// Who the host product is. The host sets it once through [`plugin`]; every user-facing string and the
/// "never drive the host itself" deny entry come from it.
#[derive(Clone, Debug)]
pub struct DemoIdentity {
    /// Display name of the host product, as shown in consent text and error messages.
    pub product_name: String,
    /// Lowercase slug used in service, tray and thread names.
    pub slug: String,
    /// `(macOS bundle id, lowercase Windows exe name)` of the host product itself. The demo never drives it.
    pub own_ids: (String, String),
    /// Host product version, reported as the demo service version.
    pub version: String,
}

impl DemoIdentity {
    /// Identity for a host product.
    pub fn new(
        product_name: impl Into<String>,
        slug: impl Into<String>,
        mac_bundle_id: impl Into<String>,
        windows_exe: impl Into<String>,
    ) -> Self {
        Self {
            product_name: product_name.into(),
            slug: slug.into(),
            own_ids: (
                mac_bundle_id.into(),
                windows_exe.into().to_ascii_lowercase(),
            ),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Sets the host product version reported by the demo service.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }
}

impl Default for DemoIdentity {
    fn default() -> Self {
        Self::new("This app", "app", "app.invalid.host", "host.exe")
    }
}

static IDENTITY: OnceLock<DemoIdentity> = OnceLock::new();

/// The identity set by [`plugin`], or the neutral default when none was set.
pub fn identity() -> &'static DemoIdentity {
    IDENTITY.get_or_init(DemoIdentity::default)
}

pub const EVENT_STATE: &str = "demo://state";
pub const EVENT_CONSENT: &str = "demo://consent";
pub const EVENT_RECEIPT: &str = "demo://receipt";

/// `demo://state` payload and part of `demo_status`. Never carries the session token.
#[derive(Clone, Serialize)]
pub struct StateDto {
    pub phase: Phase,
    pub session: Option<SessionDto>,
    pub bundle: Option<BundleDto>,
    pub message: Option<String>,
    pub hotkey: String,
}

#[derive(Clone, Serialize)]
pub struct SessionDto {
    pub id: String,
    /// `agent` or `person`.
    pub kind: &'static str,
    pub app: String,
    pub app_name: String,
    pub agent_name: Option<String>,
    pub started_ms: Option<u64>,
}

/// A sealed bundle. `name` is the folder name the reveal and discard commands take.
#[derive(Clone, Serialize)]
pub struct BundleDto {
    pub name: String,
    pub id: String,
    pub dir: String,
    /// `complete`, `killed`, `quarantined` or `invalid`.
    pub state: String,
    pub verified: bool,
    pub problems: Vec<String>,
    pub reason: Option<String>,
}

/// `demo://consent` payload: what the dialog shows. `agent_exe` and `agent_pid` are the operating
/// system's account of the caller; `agent_name` is only what the agent claims.
#[derive(Clone, Serialize)]
pub struct ConsentDto {
    pub session: String,
    pub app: String,
    pub app_name: String,
    pub agent_name: String,
    pub agent_exe: Option<String>,
    pub agent_pid: Option<u32>,
    pub will_do: String,
}

/// `demo://receipt` payload. Content-free: no typed text, no element label, no value.
#[derive(Clone, Serialize)]
pub struct ReceiptDto {
    pub receipt_id: String,
    pub session: String,
    pub action: String,
    pub result: &'static str,
    pub denied_by: Option<&'static str>,
    pub tier: Option<&'static str>,
    pub ts_ms: u64,
}

#[derive(Clone, Serialize)]
pub struct StatusDto {
    /// Always true here; the UI sees `false` (command missing) in a build without the feature.
    pub compiled: bool,
    pub enabled: bool,
    pub hotkey: String,
    pub state: StateDto,
    pub pending_consent: Option<ConsentDto>,
}

#[derive(Clone, Serialize)]
pub struct PrepareDto {
    pub accessibility: bool,
    pub screen_recording: bool,
    pub recorder: String,
    pub notes: Vec<String>,
}

/// Lifecycle plugin: creates the host, starts the service when the saved flag is on, and shuts a
/// running session down at exit. It registers no commands (the host lists those from [`commands`] in its own handler list), so
/// it needs no capability entry.
pub fn plugin(identity: DemoIdentity) -> tauri::plugin::TauriPlugin<Wry> {
    // The first identity wins; a second plugin() call in one process keeps the original.
    let _ = IDENTITY.set(identity);
    tauri::plugin::Builder::<Wry>::new("agent-demo")
        .setup(|app, _api| {
            let app_root = app.state::<rightkit_daemon::AppRoot>();
            let roots = app.state::<rightkit_daemon::mode::AppRoots>();
            let host = DemoHost::new(app.clone(), &app_root, roots.suite.clone())?;
            app.manage(host.clone());
            host.restore();
            if let Err(e) = tray::install(app) {
                eprintln!("demo: tray kill button unavailable: {e}");
            }
            Ok(())
        })
        .on_event(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(host) = app.try_state::<Arc<DemoHost>>() {
                    host.inner().shutdown();
                }
            }
        })
        .build()
}
