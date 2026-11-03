//! Typed page capture bound to the browser session that produced it (rightkit#821).
//!
//! [`capture`] launches its own session, loads one real URL at one viewport, and returns a
//! [`CaptureReport`]: the load result, a PNG screenshot, navigation and paint metrics, the console
//! and uncaught exceptions, and (feature `axe`) an axe-core accessibility result, each as a bounded
//! artifact written under a private output directory with its length and SHA-256. The report binds
//! all of it to the session, process, profile, page, frame, loader and execution context that
//! produced it and to the caller's plan, source, probe and action digests, and ends with the
//! terminal outcome of cleanup ([`StopReport`]), not a lifecycle event.
//!
//! Decision (rightkit#821, option 2): capture is built on [`BrowserSession`] and real pages, not
//! on `FixtureSession`. A fixture session serves a sealed bundle of synthetic files and exposes no
//! script; a Legion live-surface capture targets the project's real running pages. The terminal
//! cleanup result that `shutdown()` discarded is now returned by [`BrowserSession::shutdown_report`]
//! and [`BrowserSession::cancel_report`]. No general script evaluation is exposed: the runtime
//! reads fixed observations, and axe runs in an isolated world so the page cannot replace it.
//!
//! Failure, timeout and cancellation still return what was collected plus the cleanup outcome;
//! only a request that is invalid before anything launches returns `Err`.

use crate::config::LaunchOptions;
use crate::error::{cdp, BrowserError, Result};
use crate::page::BrowserPage;
use crate::policy::NetworkPolicy;
use crate::session::{BrowserSession, PageSetup, StopReport};
use chromiumoxide::cdp::browser_protocol::page::{CreateIsolatedWorldParams, GetFrameTreeParams};
use chromiumoxide::cdp::js_protocol::runtime::{
    EnableParams as RuntimeEnableParams, EvaluateParams, EventExecutionContextCreated,
    ExecutionContextId,
};
use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

/// Schema tag of every [`CaptureReport`].
pub const CAPTURE_SCHEMA: &str = "rightkit.browser.capture/1";
/// Most console records kept.
pub const MAX_CAPTURE_CONSOLE_RECORDS: usize = 500;
/// Largest screenshot accepted, in bytes.
pub const MAX_CAPTURE_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
/// Largest axe result kept, in bytes of JSON.
pub const MAX_CAPTURE_AXE_BYTES: usize = 4 * 1024 * 1024;
/// Largest viewport edge, in CSS pixels.
pub const MAX_CAPTURE_VIEWPORT_EDGE: u32 = 16384;
/// Longest accepted deadline.
pub const MAX_CAPTURE_DEADLINE: Duration = Duration::from_secs(600);
/// The vendored axe-core version (feature `axe`).
#[cfg(feature = "axe")]
pub const AXE_VERSION: &str = "4.14.0";
/// SHA-256 of the vendored `axe.min.js` (feature `axe`); checked before every use.
#[cfg(feature = "axe")]
pub const AXE_SHA256: &str = "20c09fe157a8a34a30e241aaa1fcdade657734f08ab379ecfbeb7d45cc46e878";
#[cfg(feature = "axe")]
const AXE_SOURCE: &str = include_str!("../third-party/axe-core/4.14.0/axe.min.js");

const MAX_DIGEST_CHARS: usize = 200;
#[cfg(feature = "axe")]
const MAX_AXE_TAGS: usize = 16;
#[cfg(feature = "axe")]
const MAX_AXE_TAG_CHARS: usize = 40;
const SETTLE_WAIT: Duration = Duration::from_secs(10);

/// The caller's own identity for the work being captured, carried verbatim into the report.
/// Each digest is a non-empty string of at most 200 characters with no control characters; the
/// format (for example `sha256:<hex>`) is the caller's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureBinding {
    pub plan_digest: String,
    pub source_digest: String,
    pub probe_digest: String,
    pub action_digest: String,
}

/// An accessibility check to run with the vendored axe-core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxeSpec {
    /// axe rule tags (for example `wcag2a`, `wcag2aa`); one to 16 of `a-z0-9-`, at most 40 characters each.
    pub tags: Vec<String>,
}

impl AxeSpec {
    /// WCAG 2.0 level A and AA.
    pub fn wcag2_aa() -> Self {
        Self {
            tags: vec!["wcag2a".into(), "wcag2aa".into()],
        }
    }
}

/// One capture of one URL.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub binding: CaptureBinding,
    /// `http` or `https` URL of the real page.
    pub url: String,
    /// Viewport in CSS pixels, each edge 1 to [`MAX_CAPTURE_VIEWPORT_EDGE`].
    pub viewport: (u32, u32),
    /// Existing absolute directory; the capture writes a private `rk-capture-*` directory in it.
    pub output_root: PathBuf,
    /// Budget for the whole load and observation run (1 s to [`MAX_CAPTURE_DEADLINE`]). Launch
    /// has its own bound; cleanup always runs.
    pub deadline: Duration,
    /// Network policy of the capture session. The default refuses loopback and private
    /// addresses; allow a dev server's host explicitly.
    pub network: NetworkPolicy,
    /// Explicit browser executable; `None` resolves Chrome for Testing as a session does.
    pub chrome_path: Option<PathBuf>,
    /// Run axe-core with these tags; needs feature `axe`.
    pub axe: Option<AxeSpec>,
}

impl CaptureRequest {
    /// A request for `url` at 1280x800 with a 60 s budget, the default network policy and no axe.
    pub fn new(
        url: impl Into<String>,
        binding: CaptureBinding,
        output_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binding,
            url: url.into(),
            viewport: (1280, 800),
            output_root: output_root.into(),
            deadline: Duration::from_secs(60),
            network: NetworkPolicy::default(),
            chrome_path: None,
            axe: None,
        }
    }

    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("plan_digest", &self.binding.plan_digest),
            ("source_digest", &self.binding.source_digest),
            ("probe_digest", &self.binding.probe_digest),
            ("action_digest", &self.binding.action_digest),
        ] {
            if value.is_empty()
                || value.chars().count() > MAX_DIGEST_CHARS
                || value.chars().any(char::is_control)
            {
                return Err(BrowserError::Invalid(format!(
                    "{name} must be 1 to {MAX_DIGEST_CHARS} characters without control characters"
                )));
            }
        }
        let url = url::Url::parse(&self.url)
            .map_err(|e| BrowserError::Invalid(format!("capture url: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(BrowserError::Invalid(
                "capture url must be http or https".into(),
            ));
        }
        let (w, h) = self.viewport;
        if !(1..=MAX_CAPTURE_VIEWPORT_EDGE).contains(&w)
            || !(1..=MAX_CAPTURE_VIEWPORT_EDGE).contains(&h)
        {
            return Err(BrowserError::Invalid(format!(
                "viewport edges must be 1 to {MAX_CAPTURE_VIEWPORT_EDGE}"
            )));
        }
        if self.deadline < Duration::from_secs(1) || self.deadline > MAX_CAPTURE_DEADLINE {
            return Err(BrowserError::Invalid(
                "capture deadline must be 1 s to 600 s".into(),
            ));
        }
        if !self.output_root.is_absolute() || !self.output_root.is_dir() {
            return Err(BrowserError::Invalid(
                "output_root must be an existing absolute directory".into(),
            ));
        }
        if let Some(axe) = &self.axe {
            #[cfg(not(feature = "axe"))]
            {
                let _ = axe;
                return Err(BrowserError::Unsupported(
                    "axe capture needs the rightkit-browser feature `axe`".into(),
                ));
            }
            #[cfg(feature = "axe")]
            {
                let ok = !axe.tags.is_empty()
                    && axe.tags.len() <= MAX_AXE_TAGS
                    && axe.tags.iter().all(|t| {
                        !t.is_empty()
                            && t.len() <= MAX_AXE_TAG_CHARS
                            && t.bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    });
                if !ok {
                    return Err(BrowserError::Invalid(
                        "axe tags: 1 to 16 tags of a-z, 0-9 and '-', at most 40 characters each"
                            .into(),
                    ));
                }
                if hex(&Sha256::digest(AXE_SOURCE.as_bytes())) != AXE_SHA256 {
                    return Err(BrowserError::Invalid(
                        "the vendored axe-core does not match its pinned hash".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Sticky, cloneable cancellation signal for [`capture`].
#[derive(Clone)]
pub struct CaptureCancellation {
    cancelled: tokio::sync::watch::Sender<bool>,
}

impl Default for CaptureCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureCancellation {
    pub fn new() -> Self {
        let (cancelled, _) = tokio::sync::watch::channel(false);
        Self { cancelled }
    }

    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    async fn cancelled(&self) {
        let mut rx = self.cancelled.subscribe();
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// How the capture ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureStatus {
    /// The page loaded successfully, every requested observation was obtained whole, and cleanup
    /// succeeded.
    Complete,
    /// The run finished but something is missing or wrong: a failed load, an unavailable or
    /// truncated observation, or incomplete cleanup. `reasons` says which.
    Incomplete,
    /// A step failed and stopped the run.
    Failed,
    /// The deadline passed.
    TimedOut,
    /// The caller cancelled.
    Cancelled,
}

/// One file the capture wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureArtifact {
    pub path: PathBuf,
    pub len: u64,
    pub sha256: String,
}

/// What produced the capture: the browser instance and the exact document observed, bound to the
/// caller's digests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureIdentity {
    pub binding: CaptureBinding,
    /// SHA-256 of the canonical request (url, viewport, axe tags and hash, binding).
    pub request_digest: String,
    pub browser_session_id: String,
    pub browser_pid: Option<u32>,
    /// The profile directory of this session (a temporary directory removed at cleanup).
    pub profile_dir: PathBuf,
    pub page_id: String,
    pub target_id: String,
    pub frame_id: String,
    pub loader_id: String,
    /// CDP unique id of the main frame's default execution context, when seen.
    pub unique_context_id: Option<String>,
    pub requested_url: String,
    /// The frame's URL after loading.
    pub final_url: String,
    pub viewport: (u32, u32),
}

/// The navigation result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadObservation {
    /// The navigation finished and the main document's HTTP status was below 400.
    pub ok: bool,
    /// HTTP status of the main document; `None` when the browser did not report one.
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// PNG screenshot of the viewport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotArtifact {
    pub artifact: CaptureArtifact,
    pub width: u32,
    pub height: u32,
}

/// Navigation and paint timings; values the browser did not report are JSON `null`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsObservation {
    pub artifact: CaptureArtifact,
    pub values: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleRecord {
    pub level: String,
    pub text: String,
}

/// Console output and uncaught exceptions from page creation to the end of observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleObservation {
    pub artifact: CaptureArtifact,
    pub records: Vec<ConsoleRecord>,
    pub error_records: usize,
    pub exceptions: Vec<String>,
    /// The console buffer reached its cap, so earlier lines may be missing.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AxeStatus {
    /// axe ran and found no violation for the tags.
    Passed,
    /// axe ran and reported violations (see the artifact).
    Violations,
    /// The result exceeded [`MAX_CAPTURE_AXE_BYTES`]; the count is known, the detail was not kept.
    Truncated,
}

/// The axe-core result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AxeObservation {
    pub version: String,
    pub script_sha256: String,
    pub tags: Vec<String>,
    pub status: AxeStatus,
    pub violation_count: usize,
    pub artifact: Option<CaptureArtifact>,
}

/// Terminal cleanup, as achieved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    /// A browser was launched (when false there was nothing to clean up).
    pub launched: bool,
    /// The session's own stop outcome; `None` when nothing launched.
    pub stop: Option<StopReport>,
    /// Every launched resource was released without error.
    pub complete: bool,
}

/// Everything one capture produced.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureReport {
    pub schema: &'static str,
    pub status: CaptureStatus,
    /// Why the status is not [`CaptureStatus::Complete`].
    pub reasons: Vec<String>,
    pub identity: Option<CaptureIdentity>,
    pub load: Option<LoadObservation>,
    pub screenshot: Option<ScreenshotArtifact>,
    pub metrics: Option<MetricsObservation>,
    pub console: Option<ConsoleObservation>,
    pub axe: Option<AxeObservation>,
    /// Private directory holding the artifacts and `report.json`; kept for the caller.
    pub output_dir: PathBuf,
    pub cleanup: CleanupReport,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_artifact(dir: &Path, name: &str, bytes: &[u8]) -> Result<CaptureArtifact> {
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(CaptureArtifact {
        path,
        len: bytes.len() as u64,
        sha256: hex(&Sha256::digest(bytes)),
    })
}

/// `(width, height)` from a PNG's IHDR, or `None` when the bytes are not a PNG.
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 24 || &png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

const METRICS_SCRIPT: &str = r#"(() => {
  const num = (v) => (typeof v === 'number' && isFinite(v)) ? v : null;
  const nav = performance.getEntriesByType('navigation')[0];
  const paint = {};
  for (const p of performance.getEntriesByType('paint')) { paint[p.name] = num(p.startTime); }
  return {
    navigation: nav ? {
      type: nav.type,
      responseStart: num(nav.responseStart),
      responseEnd: num(nav.responseEnd),
      domContentLoadedEventEnd: num(nav.domContentLoadedEventEnd),
      loadEventEnd: nav.loadEventEnd > 0 ? num(nav.loadEventEnd) : null,
      duration: num(nav.duration),
      transferSize: num(nav.transferSize),
      encodedBodySize: num(nav.encodedBodySize),
      decodedBodySize: num(nav.decodedBodySize),
      responseStatus: num(nav.responseStatus)
    } : null,
    paint: {
      firstPaint: num(paint['first-paint']),
      firstContentfulPaint: num(paint['first-contentful-paint'])
    },
    resourceCount: performance.getEntriesByType('resource').length
  };
})()"#;

/// Evaluate `expression` in the isolated execution context `context` and return its value.
async fn eval_isolated(
    page: &BrowserPage,
    context: ExecutionContextId,
    expression: String,
) -> Result<Value> {
    let params = EvaluateParams::builder()
        .expression(expression)
        .context_id(context)
        .return_by_value(true)
        .await_promise(true)
        .build()
        .map_err(BrowserError::Invalid)?;
    let response = page.raw().execute(params).await.map_err(cdp)?;
    if let Some(exception) = response.result.exception_details {
        return Err(BrowserError::Cdp(format!(
            "observation script failed: {}",
            exception.text
        )));
    }
    Ok(response.result.result.value.unwrap_or(Value::Null))
}

/// Capture `request`. See the module docs for what is returned on each ending.
///
/// Returns `Err` only when the request is invalid or the output directory cannot be created,
/// before any browser launches. A browser that cannot launch yields a [`CaptureStatus::Failed`]
/// report with `cleanup.launched == false`.
pub async fn capture(
    request: CaptureRequest,
    cancel: &CaptureCancellation,
) -> Result<CaptureReport> {
    request.validate()?;
    let output_dir = tempfile::Builder::new()
        .prefix("rk-capture-")
        .tempdir_in(&request.output_root)?
        .keep();
    let mut report = CaptureReport {
        schema: CAPTURE_SCHEMA,
        status: CaptureStatus::Failed,
        reasons: Vec::new(),
        identity: None,
        load: None,
        screenshot: None,
        metrics: None,
        console: None,
        axe: None,
        output_dir: output_dir.clone(),
        cleanup: CleanupReport {
            launched: false,
            stop: None,
            complete: true,
        },
    };
    let launch = LaunchOptions {
        headless: true,
        mute_audio: true,
        viewport: request.viewport,
        chrome_path: request.chrome_path.clone(),
        network: request.network.clone(),
        launch_timeout: request.deadline.min(Duration::from_secs(60)),
        request_timeout: request.deadline.min(Duration::from_secs(30)),
        ..LaunchOptions::default()
    };
    let session = match BrowserSession::launch(launch).await {
        Ok(session) => session,
        Err(error) => {
            report
                .reasons
                .push(format!("browser did not launch: {error}"));
            finish_report(&report);
            return Ok(report);
        }
    };
    report.cleanup.launched = true;

    let outcome = tokio::select! {
        result = tokio::time::timeout(
            request.deadline,
            observe(&request, &session, &output_dir, &mut report),
        ) => match result {
            Ok(Ok(())) => CaptureStatus::Complete,
            Ok(Err(error)) => {
                report.reasons.push(format!("capture stopped: {error}"));
                CaptureStatus::Failed
            }
            Err(_) => {
                report.reasons.push("the capture deadline passed".into());
                CaptureStatus::TimedOut
            }
        },
        _ = cancel.cancelled() => {
            report.reasons.push("capture was cancelled".into());
            CaptureStatus::Cancelled
        }
    };

    let stop = if outcome == CaptureStatus::Complete {
        session.shutdown_report().await
    } else {
        session.cancel_report().await
    };
    report.cleanup.complete = stop.process_stopped && stop.profile_released;
    if let Some(error) = &stop.process_error {
        report.reasons.push(format!("browser process: {error}"));
    }
    if let Some(error) = &stop.profile_error {
        report.reasons.push(format!("browser profile: {error}"));
    }
    report.cleanup.stop = Some(stop);
    drop(session);

    report.status = if outcome == CaptureStatus::Complete
        && (!report.reasons.is_empty() || !report.cleanup.complete)
    {
        CaptureStatus::Incomplete
    } else {
        outcome
    };
    finish_report(&report);
    Ok(report)
}

/// Write `report.json` beside the artifacts; best effort, the returned report is the record.
fn finish_report(report: &CaptureReport) {
    if let Ok(bytes) = serde_json::to_vec_pretty(report) {
        let _ = std::fs::write(report.output_dir.join("report.json"), bytes);
    }
}

fn request_digest(request: &CaptureRequest) -> String {
    #[cfg(feature = "axe")]
    let axe_hash = request.axe.as_ref().map(|_| AXE_SHA256);
    #[cfg(not(feature = "axe"))]
    let axe_hash: Option<&str> = None;
    let canonical = serde_json::json!({
        "schema": CAPTURE_SCHEMA,
        "url": request.url,
        "viewport": [request.viewport.0, request.viewport.1],
        "axeTags": request.axe.as_ref().map(|a| a.tags.clone()),
        "axeSha256": axe_hash,
        "binding": request.binding,
    });
    format!(
        "sha256:{}",
        hex(&Sha256::digest(canonical.to_string().as_bytes()))
    )
}

/// The observation run. Fills `report` as it goes so a timeout or cancellation keeps the partial
/// output.
async fn observe(
    request: &CaptureRequest,
    session: &BrowserSession,
    out: &Path,
    report: &mut CaptureReport,
) -> Result<()> {
    let page = session
        .new_page_with("about:blank", PageSetup::default())
        .await?;
    page.set_viewport(request.viewport.0, request.viewport.1, 1.0)
        .await?;
    let raw = page.raw();

    let contexts: Arc<StdMutex<Vec<(String, String)>>> = Arc::default();
    let mut created = raw
        .event_listener::<EventExecutionContextCreated>()
        .await
        .map_err(cdp)?;
    let sink = contexts.clone();
    let _contexts_task = AbortOnDrop(tokio::spawn(async move {
        while let Some(event) = created.next().await {
            let aux = event.context.aux_data.as_ref();
            let is_default = aux
                .and_then(|v| v.get("isDefault"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let frame = aux
                .and_then(|v| v.get("frameId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let (true, Some(frame)) = (is_default, frame) {
                sink.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((frame, event.context.unique_id.clone()));
            }
        }
    }));
    raw.execute(RuntimeEnableParams::default())
        .await
        .map_err(cdp)?;
    page.clear_console().await;
    page.clear_exceptions();

    let navigation = page.goto(&request.url).await;
    // Document complete and the load event recorded, bounded; metrics report what exists.
    let _ = page
        .wait_eval("document.readyState === 'complete'", SETTLE_WAIT)
        .await;
    let _ = page
        .wait_eval(
            "(performance.getEntriesByType('navigation')[0] || {loadEventEnd: 1}).loadEventEnd > 0",
            SETTLE_WAIT,
        )
        .await;

    let frame = raw
        .execute(GetFrameTreeParams::default())
        .await
        .map_err(cdp)?
        .result
        .frame_tree
        .frame;
    let frame_id = frame.id.as_ref().to_string();
    let unique_context_id = contexts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .find(|(f, _)| *f == frame_id)
        .map(|(_, unique)| unique.clone());
    report.identity = Some(CaptureIdentity {
        binding: request.binding.clone(),
        request_digest: request_digest(request),
        browser_session_id: session.id().to_string(),
        browser_pid: session.process_id(),
        profile_dir: session.profile_dir().to_path_buf(),
        page_id: page.id().to_string(),
        target_id: page.target_id().to_string(),
        frame_id: frame_id.clone(),
        loader_id: frame.loader_id.as_ref().to_string(),
        unique_context_id,
        requested_url: request.url.clone(),
        final_url: frame.url.clone(),
        viewport: request.viewport,
    });
    if report
        .identity
        .as_ref()
        .is_some_and(|identity| identity.unique_context_id.is_none())
    {
        report
            .reasons
            .push("the main frame's execution context was not observed".into());
    }

    // An isolated world: the page's scripts cannot replace what observes it.
    let mut world = CreateIsolatedWorldParams::new(frame.id.clone());
    world.world_name = Some(format!("rightkit-capture-{}", session.id()));
    world.grant_univeral_access = Some(false);
    let context = raw
        .execute(world)
        .await
        .map_err(cdp)?
        .result
        .execution_context_id;

    let metrics = eval_isolated(&page, context, METRICS_SCRIPT.to_string()).await;
    let status = metrics
        .as_ref()
        .ok()
        .and_then(|v| v.pointer("/navigation/responseStatus"))
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok());
    let load = match &navigation {
        Ok(()) => LoadObservation {
            ok: status.is_some_and(|s| s < 400),
            status,
            error: None,
        },
        Err(error) => LoadObservation {
            ok: false,
            status,
            error: Some(error.to_string()),
        },
    };
    if !load.ok {
        report.reasons.push(match (&load.error, load.status) {
            (Some(error), _) => format!("navigation failed: {error}"),
            (None, Some(status)) => format!("navigation returned HTTP {status}"),
            (None, None) => "the browser reported no HTTP status for the page".into(),
        });
    }
    report.load = Some(load);

    match metrics {
        Ok(values) if !values.is_null() => {
            let bytes = serde_json::to_vec_pretty(&values)
                .map_err(|e| BrowserError::Invalid(e.to_string()))?;
            report.metrics = Some(MetricsObservation {
                artifact: write_artifact(out, "metrics.json", &bytes)?,
                values,
            });
        }
        Ok(_) => report.reasons.push("metrics were unavailable".into()),
        Err(error) => report
            .reasons
            .push(format!("metrics were unavailable: {error}")),
    }

    match page.screenshot().await {
        Ok(png) if png.len() > MAX_CAPTURE_SCREENSHOT_BYTES => report.reasons.push(format!(
            "screenshot exceeded {MAX_CAPTURE_SCREENSHOT_BYTES} bytes"
        )),
        Ok(png) => match png_dimensions(&png) {
            Some((width, height)) => {
                report.screenshot = Some(ScreenshotArtifact {
                    artifact: write_artifact(out, "screenshot.png", &png)?,
                    width,
                    height,
                });
            }
            None => report
                .reasons
                .push("the screenshot was not a valid PNG".into()),
        },
        Err(error) => report
            .reasons
            .push(format!("screenshot was unavailable: {error}")),
    }

    if let Some(axe) = &request.axe {
        run_axe(&page, context, axe, out, report).await?;
    }

    let lines = page.console().await;
    let truncated = lines.len() >= MAX_CAPTURE_CONSOLE_RECORDS;
    let records: Vec<ConsoleRecord> = lines
        .into_iter()
        .take(MAX_CAPTURE_CONSOLE_RECORDS)
        .map(|line| ConsoleRecord {
            level: line.level,
            text: line.text,
        })
        .collect();
    let exceptions: Vec<String> = page
        .exceptions()
        .into_iter()
        .map(|e| e.description.unwrap_or(e.text))
        .collect();
    let error_records = records
        .iter()
        .filter(|r| r.level.eq_ignore_ascii_case("error"))
        .count();
    let body = serde_json::json!({
        "records": records,
        "exceptions": exceptions,
        "truncated": truncated,
    });
    let bytes =
        serde_json::to_vec_pretty(&body).map_err(|e| BrowserError::Invalid(e.to_string()))?;
    report.console = Some(ConsoleObservation {
        artifact: write_artifact(out, "console.json", &bytes)?,
        records,
        error_records,
        exceptions,
        truncated,
    });
    if truncated {
        report
            .reasons
            .push("the console buffer was full; earlier lines may be missing".into());
    }
    Ok(())
}

#[cfg(feature = "axe")]
async fn run_axe(
    page: &BrowserPage,
    context: ExecutionContextId,
    axe: &AxeSpec,
    out: &Path,
    report: &mut CaptureReport,
) -> Result<()> {
    if let Err(error) = eval_isolated(page, context, AXE_SOURCE.to_string()).await {
        report
            .reasons
            .push(format!("axe-core could not be loaded: {error}"));
        return Ok(());
    }
    let version = eval_isolated(page, context, "axe.version".to_string()).await;
    if version.as_ref().ok().and_then(Value::as_str) != Some(AXE_VERSION) {
        report
            .reasons
            .push("axe-core reported a version other than the pinned one".into());
        return Ok(());
    }
    let tags =
        serde_json::to_string(&axe.tags).map_err(|e| BrowserError::Invalid(e.to_string()))?;
    let script = format!(
        "axe.run(document, {{ runOnly: {{ type: 'tag', values: {tags} }}, resultTypes: ['violations'] }})\
         .then((r) => JSON.stringify({{ url: r.url, testEngine: r.testEngine, \
         violations: r.violations.map((v) => ({{ id: v.id, impact: v.impact, help: v.help, \
         helpUrl: v.helpUrl, tags: v.tags, nodes: v.nodes.map((n) => ({{ target: n.target, \
         html: (n.html || '').slice(0, 300) }})) }})) }}))"
    );
    let result = match eval_isolated(page, context, script).await {
        Ok(value) => value,
        Err(error) => {
            report
                .reasons
                .push(format!("axe-core did not finish: {error}"));
            return Ok(());
        }
    };
    let Some(text) = result.as_str() else {
        report.reasons.push("axe-core returned no result".into());
        return Ok(());
    };
    let parsed: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            report
                .reasons
                .push(format!("axe-core result was not JSON: {error}"));
            return Ok(());
        }
    };
    let count = parsed
        .get("violations")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let (status, artifact) = if text.len() > MAX_CAPTURE_AXE_BYTES {
        report.reasons.push(format!(
            "the axe result exceeded {MAX_CAPTURE_AXE_BYTES} bytes and was not kept"
        ));
        (AxeStatus::Truncated, None)
    } else {
        let bytes =
            serde_json::to_vec_pretty(&parsed).map_err(|e| BrowserError::Invalid(e.to_string()))?;
        let status = if count == 0 {
            AxeStatus::Passed
        } else {
            AxeStatus::Violations
        };
        (status, Some(write_artifact(out, "axe.json", &bytes)?))
    };
    report.axe = Some(AxeObservation {
        version: AXE_VERSION.to_string(),
        script_sha256: AXE_SHA256.to_string(),
        tags: axe.tags.clone(),
        status,
        violation_count: count,
        artifact,
    });
    Ok(())
}

#[cfg(not(feature = "axe"))]
async fn run_axe(
    _page: &BrowserPage,
    _context: ExecutionContextId,
    _axe: &AxeSpec,
    _out: &Path,
    report: &mut CaptureReport,
) -> Result<()> {
    report
        .reasons
        .push("axe was requested but the `axe` feature is off".into());
    Ok(())
}
