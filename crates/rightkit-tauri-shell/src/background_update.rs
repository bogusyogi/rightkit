//! Native background update check: a cadence loop, a one-shot user-initiated check, and the cached
//! last answer, over the Tauri updater (feature `background-update`).
//!
//! The host supplies product values: cadence ([`Schedule`]), accepted manifest tiers, whether the
//! app is ready to check, the request headers of the licensed updater client (an async lookup), who
//! is eligible for a tier, how a status reaches the renderer, and the event sink. The updater
//! endpoint and its signing key come from the app's Tauri updater configuration, never from here.
//!
//! The renderer side is `@rightkit/app-update`'s `createUpdateSession({ background })`: it reads the
//! cached [`status`] through a command the host registers (a one-line wrapper) and listens for the
//! event the host's `publish` emits. [`BackgroundUpdateStatus`] serialises as that package's
//! `BackgroundUpdateStatus`, extended with the `available` fields below.
//!
//! The one check is shared by the loop & [`check_now`], so both use identical headers,
//! tier validation & error wording. This module checks metadata only; it never downloads, installs
//! or treats tier eligibility as authorization for a later install.
//!
//! Lifecycle: call [`start`] once per process after plugin registration. Its detached task holds
//! the app handle until runtime shutdown; no stop/restart API is provided. Manual checks may
//! overlap with it or each other, with status recorded in completion order. The cache is
//! process-wide. Hosts supply nonzero retry/regular delays & bound their header lookup work.
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::UpdaterExt;

/// Event-sink name written after each loop check; the detail is `status=<kind>`.
pub const EVENT_BACKGROUND_CHECK: &str = "background_update_check";
/// Event-sink name written after each [`check_now`]; the detail is `status=<kind>`.
pub const EVENT_MANUAL_CHECK: &str = "manual_update_check";
/// `reason` of an `available` status the host may install (it is eligible for the tier).
pub const REASON_ELIGIBLE: &str = "auto_install_off";
/// `reason` of an `available` status the host is not eligible to install.
pub const REASON_INELIGIBLE: &str = "pro_required";

/// What a check found. Serialises as `{ "kind": "idle" | "up_to_date" | "available" | "error", .. }`.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackgroundUpdateStatus {
    Idle,
    UpToDate,
    Available {
        version: String,
        tier: String,
        eligible: bool,
        reason: String,
    },
    Error {
        message: String,
    },
}

impl BackgroundUpdateStatus {
    /// The fixed `status=<kind>` detail written to the event sink (no version, no message).
    fn log_detail(&self) -> &'static str {
        match self {
            BackgroundUpdateStatus::Idle => "status=idle",
            BackgroundUpdateStatus::UpToDate => "status=up_to_date",
            BackgroundUpdateStatus::Available { .. } => "status=available",
            BackgroundUpdateStatus::Error { .. } => "status=error",
        }
    }
}

/// The cadence of the loop.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    /// Wait after [`start`] before the first check.
    pub initial_delay: Duration,
    /// Wait between checks.
    pub regular_delay: Duration,
    /// Wait before asking [`Host::ready`] again while it answers `false`.
    pub not_ready_delay: Duration,
}

/// The licensed updater client's request headers as `(name, value)` pairs; empty adds none.
pub type HeadersFuture = Pin<Box<dyn Future<Output = Vec<(String, String)>> + Send>>;

/// Everything the host decides. Plain function pointers, so a host builds one anywhere.
pub struct Host<R: Runtime> {
    pub schedule: Schedule,
    /// The manifest `tier` values that are valid; any other (or a missing tier) is an `error` status.
    pub allowed_tiers: &'static [&'static str],
    /// Whether a loop check may run now (the app's own onboarding or consent gate).
    pub ready: fn() -> bool,
    /// Headers for native-configured updater endpoints, looked up at every check.
    /// The host owns endpoint trust & credential scope; never source these from renderer input.
    pub request_headers: fn() -> HeadersFuture,
    /// Whether status should report eligibility for this tier. Install must authorize separately.
    pub tier_eligible: fn(&str) -> bool,
    /// Delivers a new status to the renderer (and anywhere else the host wants it).
    pub publish: fn(&AppHandle<R>, &BackgroundUpdateStatus),
    /// `(event, detail)` diagnostics.
    pub sink: fn(&str, &str),
}

static STATUS: LazyLock<RwLock<BackgroundUpdateStatus>> =
    LazyLock::new(|| RwLock::new(BackgroundUpdateStatus::Idle));

/// The last status, `Idle` before the first check. The host's command returns this.
pub fn status() -> BackgroundUpdateStatus {
    STATUS
        .read()
        .map(|status| status.clone())
        .unwrap_or(BackgroundUpdateStatus::Idle)
}

fn record<R: Runtime>(app: &AppHandle<R>, host: &Host<R>, status: BackgroundUpdateStatus) {
    if let Ok(mut current) = STATUS.write() {
        *current = status.clone();
    }
    (host.publish)(app, &status);
}

/// Build the licensed updater client shared by the loop and any user-initiated check. Kept as one
/// function so the two callers can never drift on auth headers or error wording.
async fn build_updater<R: Runtime>(
    app: &AppHandle<R>,
    host: &Host<R>,
) -> Result<tauri_plugin_updater::Updater, BackgroundUpdateStatus> {
    let mut builder = app.updater_builder();
    for (name, value) in (host.request_headers)().await {
        builder = match builder.header(name, value) {
            Ok(builder) => builder,
            Err(error) => {
                return Err(BackgroundUpdateStatus::Error {
                    message: format!("configure update check: {error}"),
                });
            }
        };
    }

    builder
        .build()
        .map_err(|error| BackgroundUpdateStatus::Error {
            message: format!("configure updater: {error}"),
        })
}

async fn check_once<R: Runtime>(app: &AppHandle<R>, host: &Host<R>) -> BackgroundUpdateStatus {
    let updater = match build_updater(app, host).await {
        Ok(updater) => updater,
        Err(status) => return status,
    };

    match updater.check().await {
        Ok(Some(update)) => {
            let Some(tier) = update
                .raw_json
                .get("tier")
                .and_then(serde_json::Value::as_str)
            else {
                return BackgroundUpdateStatus::Error {
                    message: "update manifest is missing its tier".to_string(),
                };
            };
            if !host.allowed_tiers.contains(&tier) {
                return BackgroundUpdateStatus::Error {
                    message: "update manifest has an invalid tier".to_string(),
                };
            }
            let eligible = (host.tier_eligible)(tier);
            BackgroundUpdateStatus::Available {
                version: update.version,
                tier: tier.to_string(),
                eligible,
                reason: if eligible {
                    REASON_ELIGIBLE.to_string()
                } else {
                    REASON_INELIGIBLE.to_string()
                },
            }
        }
        Ok(None) => BackgroundUpdateStatus::UpToDate,
        Err(error) => BackgroundUpdateStatus::Error {
            message: format!("background update check: {error}"),
        },
    }
}

/// Start the loop: wait `initial_delay`, then check every `regular_delay`; while `ready` answers
/// `false`, ask again every `not_ready_delay`. Call once, from setup, after the updater plugin is
/// registered.
pub fn start<R: Runtime>(app: AppHandle<R>, host: Host<R>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(host.schedule.initial_delay).await;
        loop {
            if !(host.ready)() {
                tokio::time::sleep(host.schedule.not_ready_delay).await;
                continue;
            }
            let status = check_once(&app, &host).await;
            (host.sink)(EVENT_BACKGROUND_CHECK, status.log_detail());
            record(&app, &host, status);
            tokio::time::sleep(host.schedule.regular_delay).await;
        }
    });
}

/// User-initiated check (a menu item). Reuses the loop's client and status machinery, so the result
/// reaches the renderer through the same `publish` the loop uses; it ignores `ready`.
pub fn check_now<R: Runtime>(app: AppHandle<R>, host: Host<R>) {
    tauri::async_runtime::spawn(async move {
        let status = check_once(&app, &host).await;
        (host.sink)(EVENT_MANUAL_CHECK, status.log_detail());
        record(&app, &host, status);
    });
}
