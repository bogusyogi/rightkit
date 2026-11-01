//! Motion, owned by the resident engine: render a HyperFrames
//! HTML composition to MP4 from the engine. `motion.status`, `motion.render` and `motion.cancel`
//! are engine commands; the app shell keeps `motion_*` Tauri commands that only forward here.
//!
//! The render path: a loopback project server (`rightkit_motion::project_server`) hosts the composition and the
//! deterministic runtime; managed headless Chrome (`rightkit-browser`, the RightKit
//! browser-automation owner) loads it; for every frame the runtime seeks every timeline, clip and
//! animation to `n / fps` (a pure function of the frame index, never of wall-clock time) and the
//! engine takes a CDP screenshot; frames are piped to the owned LGPL ffmpeg through the encode
//! `rightkit-motion` driver ([`encode`] holds the ffmpeg resolution and the shared
//! `h264_encoder_unavailable` shape). No OS webview.
//!
//! Progress is the `motion://progress` topic frame. GSAP is not bundled: register the build to
//! serve with [`set_gsap`] (remote GSAP URLs are left as they are without one).

pub mod encode;

use rightkit_motion::composition as doc;
use rightkit_motion::project_server as server;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_browser::{chrome_for_testing_path, BrowserError, BrowserSession, LaunchOptions};
use rightkit_ffmpeg::{CancellationToken, FfmpegBinary};
use rightkit_local_gen::roots::{self, EnvValues};
use rightkit_media_gen::GenError as EngineError;
use rightkit_motion::{
    capture_still, render_with_fallback, FrameError, FrameSource, Quality, RenderJob, RenderPhase,
    RenderProgress,
};
use serde::Deserialize;
use serde_json::{json, Value};

static GSAP: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();

/// Register the GSAP build the project server serves in place of remote GSAP URLs. First call wins.
pub fn set_gsap(js: Arc<str>) -> Result<(), Arc<str>> {
    GSAP.set(js)
}

fn gsap() -> Option<Arc<str>> {
    GSAP.get().cloned()
}

pub const PREFIX: &str = "motion.";
const PAGE_TIMEOUT: Duration = Duration::from_secs(45);
/// Flags that keep headless Chrome's output a function of the page alone: sRGB output, no
/// scrollbars, no hinting or LCD text variation, and no throttling of a page nobody looks at.
const CHROME_FLAGS: &[&str] = &[
    "force-color-profile=srgb",
    "hide-scrollbars",
    "font-render-hinting=none",
    "disable-lcd-text",
    "disable-renderer-backgrounding",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
];

pub fn is_command(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// The commands `Host::dispatch` serves (without [`PREFIX`]). The engine wire registers every method
/// by exact name, so keep this list beside the `match` in `dispatch`.
pub const COMMANDS: &[&str] = &["status", "cancel", "render"];

pub type Events = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderRequest {
    /// The composition document (preferred). Without it, the legacy HTML fields below describe a
    /// one-layer `html` document.
    #[serde(default)]
    document: Option<doc::CompositionDoc>,
    #[serde(default)]
    project_dir: String,
    #[serde(default = "default_entry")]
    entry_file: String,
    #[serde(default = "default_fps")]
    fps: u32,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default = "default_quality")]
    quality: String,
    #[serde(default)]
    project_slug: Option<String>,
    #[serde(default)]
    operator_approved: bool,
    /// Output folder; defaults to `<content root>/<content subdir>/motion`.
    #[serde(default)]
    out_dir: Option<String>,
    /// Accept MPEG-4 when no H.264 encoder can run. Without it, Motion fails with
    /// `h264_encoder_unavailable`; with it, the result reports `fallback: true`.
    #[serde(default)]
    allow_mpeg4_fallback: bool,
    /// Transparent render (ProRes 4444 `.mov` or VP9 `.webm`, never H.264). When absent, the
    /// project's `meta.json` `alpha` decides; opaque otherwise.
    #[serde(default)]
    alpha: Option<bool>,
    /// `prores` (default, `.mov`) or `vp9` (`.webm`); used only for an alpha render.
    #[serde(default)]
    alpha_format: Option<String>,
    /// Still export: capture the one frame nearest this time (seconds, clamped to the composition)
    /// as a PNG instead of a video. Same approval gate, browser and capture path.
    #[serde(default)]
    still_at_s: Option<f64>,
}

/// Whether the render is transparent: the request wins, else `<project>/meta.json` `alpha`.
fn wants_alpha(req_alpha: Option<bool>, project_dir: &std::path::Path) -> bool {
    req_alpha.unwrap_or_else(|| {
        std::fs::read(project_dir.join("meta.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|m| m["alpha"].as_bool())
            .unwrap_or(false)
    })
}

fn default_entry() -> String {
    "index.html".into()
}
fn default_fps() -> u32 {
    30
}
fn default_quality() -> String {
    "draft".into()
}

/// A request with the document folded in: the one place project, entry and size are decided.
#[derive(Debug, PartialEq)]
struct Plan {
    project_dir: String,
    entry: String,
    fps: u32,
    width: u32,
    height: u32,
}

fn plan(req: &RenderRequest) -> Result<Plan, String> {
    let (project_dir, entry, fps, width, height) = match &req.document {
        Some(document) => {
            document.validate()?;
            let (dir, entry) = document.html_source()?;
            (dir, entry, document.fps, document.width, document.height)
        }
        None => (
            req.project_dir.clone(),
            req.entry_file.clone(),
            req.fps,
            req.width,
            req.height,
        ),
    };
    if !(1..=120).contains(&fps) {
        return Err("invalid_input: fps must be between 1 and 120".into());
    }
    if !["draft", "standard", "high"].contains(&req.quality.as_str()) {
        return Err("invalid_input: quality must be draft, standard, or high".into());
    }
    if width < 16 || height < 16 || width > 7680 || height > 7680 {
        return Err("invalid_input: width and height must be between 16 and 7680".into());
    }
    Ok(Plan {
        project_dir,
        entry: entry.replace('\\', "/"),
        fps,
        width,
        height,
    })
}

#[derive(Debug, serde::Serialize)]
struct RenderResult {
    status: &'static str,
    output_path: String,
    poster_path: String,
    manifest_path: String,
    frames: u64,
    width: u32,
    height: u32,
    fps: u32,
    duration_s: f64,
    encoder: String,
    video_codec: &'static str,
    /// True only when MPEG-4 was produced because `allow_mpeg4_fallback` was set.
    fallback: bool,
    /// True for a transparent render; `pixel_format` then names the alpha pixel format.
    alpha: bool,
    pixel_format: &'static str,
    elapsed_ms: u128,
}

/// Result of a still export: one PNG of the frame nearest `still_at_s`.
#[derive(Debug, serde::Serialize)]
struct StillResult {
    status: &'static str,
    output_path: String,
    /// Lowercase hex SHA-256 of the written PNG.
    sha256: String,
    bytes: u64,
    /// Captured frame index; `time_s` is `frame / fps`.
    frame: u64,
    time_s: f64,
    width: u32,
    height: u32,
    fps: u32,
    /// True when the PNG keeps transparency (an alpha project or request).
    alpha: bool,
    elapsed_ms: u128,
}

/// The project folder and its entry file must exist, and the entry must stay inside the folder.
fn project_dir_checked(plan: &Plan) -> Result<PathBuf, String> {
    let dir = PathBuf::from(&plan.project_dir);
    if !dir.is_dir() {
        return Err(format!(
            "project_missing: {} is not a directory",
            dir.display()
        ));
    }
    let entry_path = server::resolve_inside(&dir, &plan.entry)
        .ok_or("invalid_entry: entry_file must stay inside the project")?;
    if !entry_path.is_file() || !plan.entry.to_ascii_lowercase().ends_with(".html") {
        return Err(format!("entry_missing: {}", entry_path.display()));
    }
    Ok(dir)
}

pub struct Host {
    events: Events,
    running: AtomicBool,
    /// Shared with the ffmpeg runner's `CancellationToken`, so one flag stops capture and encode.
    cancel: Arc<AtomicBool>,
}

/// Clears the running flag when a render ends, however it ends.
struct RunGuard<'a>(&'a Host);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// A page that is open, loaded and has reported its metadata: the browser session (shut down by the
/// caller on every path), the page, the root composition's length and its audio list.
struct Opened {
    session: BrowserSession,
    page: Arc<rightkit_browser::BrowserPage>,
    duration: f64,
    meta: Value,
}

/// The Chrome page as a `rightkit-motion` frame source: `seek` runs the runtime's seek script and
/// waits for the paint, `capture` takes the CDP screenshot. It blocks on the render's runtime and
/// owns the per-frame timeout, as the owner's driver adds none.
struct ChromeSource {
    rt: Arc<tokio::runtime::Runtime>,
    page: Arc<rightkit_browser::BrowserPage>,
    alpha: bool,
    frame: u64,
}

impl FrameSource for ChromeSource {
    fn seek(&mut self, time_s: f64) -> Result<(), FrameError> {
        let page = &self.page;
        let frame = self.frame;
        self.rt.block_on(async {
            let seek = tokio::time::timeout(PAGE_TIMEOUT, page.eval(&seek_script(time_s)))
                .await
                .map_err(|_| {
                    FrameError(format!("page_timeout: frame {frame} did not become ready"))
                })?;
            seek.map(|_| ()).map_err(|e| FrameError(browser_error(e)))
        })
    }

    fn capture(&mut self) -> Result<Vec<u8>, FrameError> {
        let (page, alpha) = (&self.page, self.alpha);
        let png = self
            .rt
            .block_on(capture_png(page, alpha))
            .map_err(FrameError)?;
        self.frame += 1;
        Ok(png)
    }
}

/// Everything one capture needs.
struct Job<'a> {
    plan: &'a Plan,
    url: String,
    port: u16,
    chrome: PathBuf,
    alpha: bool,
}

impl Host {
    pub fn new(events: Events) -> Self {
        Self {
            events,
            running: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn dispatch(&self, name: &str, input: &Value) -> Result<Value, EngineError> {
        match name.strip_prefix(PREFIX).unwrap_or(name) {
            "status" => Ok(self.status()),
            "cancel" => Ok(json!(self.cancel())),
            "render" => {
                let request: RenderRequest =
                    serde_json::from_value(input.clone()).map_err(|e| {
                        EngineError::new("invalid_input", format!("invalid_input: {e}"))
                    })?;
                let result = self.render(request).map_err(error_from_message)?;
                serde_json::to_value(result)
                    .map_err(|e| EngineError::new("encode", format!("encode: {e}")))
            }
            other => Err(EngineError::new(
                "unknown_command",
                format!("unknown motion command: {other}"),
            )),
        }
    }

    fn status(&self) -> Value {
        let ffmpeg = encode::ffmpeg_path().unwrap_or_default();
        let picked = encode::pick_encoder(false);
        let browser = chrome_for_testing_path(None);
        let ready = picked.is_ok();
        let error = picked.as_ref().err().cloned();
        let encoder = picked.ok().map(|e| encode::EncoderView::from(&e));
        json!({
            "capture_supported": browser.is_ok(),
            "browser_path": browser.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            "browser_error": browser.as_ref().err().map(|e| format!("browser_unavailable: {e}")),
            "ffmpeg_path": ffmpeg.to_string_lossy(),
            "ffmpeg_trust": encode::ffmpeg_trust(),
            "ffmpeg_ready": ready,
            "error": error,
            "encoder": encoder,
            "running": self.running.load(Ordering::SeqCst),
            "out_dir": default_out_dir().to_string_lossy(),
        })
    }

    /// Ask the running render (if any) to stop; true when one was running.
    fn cancel(&self) -> bool {
        let running = self.running.load(Ordering::SeqCst);
        if running {
            self.cancel.store(true, Ordering::SeqCst);
        }
        running
    }

    fn progress(&self, phase: &str, frame: u64, total: u64) {
        (self.events)(
            "motion://progress",
            json!({ "phase": phase, "frame": frame, "total": total }),
        );
    }

    fn render(&self, request: RenderRequest) -> Result<Value, String> {
        if !request.operator_approved {
            return Err("approval_required: operator approval is required before rendering".into());
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err("busy: a Motion render is already running".into());
        }
        let _guard = RunGuard(self);
        self.cancel.store(false, Ordering::SeqCst);
        let value = match request.still_at_s {
            Some(at) => serde_json::to_value(self.still_attempt(&request, at)?),
            None => serde_json::to_value(self.render_attempt(&request)?),
        };
        value.map_err(|e| format!("encode: {e}"))
    }

    /// Where a render's files go: `<out_dir>/<slug>-<stamp>`.
    fn output_base(req: &RenderRequest, dir: &std::path::Path) -> Result<PathBuf, String> {
        let out_dir = req
            .out_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(default_out_dir);
        std::fs::create_dir_all(&out_dir).map_err(|e| format!("io_error: {e}"))?;
        let slug = slugify(
            req.project_slug
                .as_deref()
                .unwrap_or_else(|| dir.file_name().and_then(|n| n.to_str()).unwrap_or("")),
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(out_dir.join(format!("{slug}-{stamp}")))
    }

    /// One video render. The H.264-refused-to-start retry (MPEG-4, only when the request allowed it) is
    /// the owner's `render_with_fallback`.
    fn render_attempt(&self, req: &RenderRequest) -> Result<RenderResult, String> {
        let started = Instant::now();
        let plan = plan(req)?;
        let dir = project_dir_checked(&plan)?;
        let chrome =
            chrome_for_testing_path(None).map_err(|e| format!("browser_unavailable: {e}"))?;
        let alpha_kind = if wants_alpha(req.alpha, &dir) {
            let name = req.alpha_format.as_deref().unwrap_or("prores");
            Some(encode::parse_alpha(name).ok_or_else(|| {
                format!("invalid_input: alpha_format must be prores or vp9, got {name}")
            })?)
        } else {
            None
        };
        let encoder = match alpha_kind {
            Some(kind) => encode::pick_alpha_encoder(kind)?,
            None => encode::pick_encoder(req.allow_mpeg4_fallback)?,
        };
        let ffmpeg = encode::ffmpeg_bin()?;
        let quality = Quality::parse(&req.quality)
            .ok_or("invalid_input: quality must be draft, standard, or high")?;

        let base = Self::output_base(req, &dir)?;
        let output = base.with_extension(encoder.kind.extension());
        let poster = base.with_extension("png");
        let manifest = base.with_extension("run.json");

        self.progress("loading", 0, 0);
        let mut server = server::ProjectServer::start(dir.clone(), plan.entry.clone(), gsap())
            .map_err(|e| format!("server_failed: {e}"))?;
        let job = Job {
            plan: &plan,
            url: server.url(&plan.entry),
            port: server.port(),
            chrome,
            alpha: alpha_kind.is_some(),
        };
        let outcome = self.with_page(&job, &server, |rt, opened| {
            let mut source = ChromeSource {
                rt: rt.clone(),
                page: opened.page.clone(),
                alpha: job.alpha,
                frame: 0,
            };
            let render_job = RenderJob {
                width: plan.width,
                height: plan.height,
                fps: plan.fps,
                duration_s: opened.duration,
                quality,
                encoder: encoder.clone(),
                audio: audio_inputs(&opened.meta, &dir),
                output: output.clone(),
                poster: Some(poster.clone()),
            };
            let events = self.events.clone();
            let mut on_progress = move |p: RenderProgress| {
                let phase = match p.phase {
                    RenderPhase::Capturing => "capturing",
                    RenderPhase::Encoding => "encoding",
                    RenderPhase::Done => return,
                };
                (events)(
                    "motion://progress",
                    json!({ "phase": phase, "frame": p.frame, "total": p.total }),
                );
            };
            render_with_fallback(
                &ffmpeg,
                &mut source,
                &render_job,
                req.allow_mpeg4_fallback,
                &CancellationToken::from_flag(self.cancel.clone()),
                &mut on_progress,
            )
            .map_err(|e| encode::render_error(e, Some(ffmpeg.ffmpeg_path())))
        });
        server.shutdown();

        match outcome {
            Ok(done) => {
                let used = encode::EncoderView::from(&done.encoder);
                let result = RenderResult {
                    status: "completed",
                    output_path: done.output.to_string_lossy().into_owned(),
                    poster_path: poster.to_string_lossy().into_owned(),
                    manifest_path: manifest.to_string_lossy().into_owned(),
                    frames: done.frames,
                    width: plan.width,
                    height: plan.height,
                    fps: plan.fps,
                    duration_s: done.duration_s,
                    encoder: done.encoder.name.to_string(),
                    video_codec: done.encoder.name,
                    fallback: done.encoder.fallback,
                    alpha: alpha_kind.is_some(),
                    pixel_format: encode::pixel_format(alpha_kind),
                    elapsed_ms: started.elapsed().as_millis(),
                };
                let record = json!({
                    "schema_version": "1.0.0",
                    "route": "managed-chrome-capture",
                    "execution_mode": "operator_approved_motion_render",
                    "request": { "project_dir": plan.project_dir, "entry_file": plan.entry, "fps": plan.fps, "quality": req.quality, "alpha": alpha_kind.is_some() },
                    "result": &result,
                    "encoder": used,
                });
                let _ = rightkit_fs::atomic_write(
                    &manifest,
                    &serde_json::to_vec_pretty(&record).unwrap_or_default(),
                );
                self.progress("done", done.frames, done.frames);
                Ok(result)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&output);
                self.progress("failed", 0, 0);
                Err(e)
            }
        }
    }

    /// One PNG: the frame nearest `at` seconds, through the same browser, server and seek path as a
    /// video. No encoder runs, so the H.264 rule does not apply.
    fn still_attempt(&self, req: &RenderRequest, at: f64) -> Result<StillResult, String> {
        let started = Instant::now();
        if !at.is_finite() || at < 0.0 {
            return Err("invalid_input: still_at_s must be a finite time of 0 or more".into());
        }
        let plan = plan(req)?;
        let dir = project_dir_checked(&plan)?;
        let chrome =
            chrome_for_testing_path(None).map_err(|e| format!("browser_unavailable: {e}"))?;
        // A transparent project keeps PNG alpha; `alpha_format` does not apply to a still.
        let alpha = wants_alpha(req.alpha, &dir);

        let base = Self::output_base(req, &dir)?;
        let output = PathBuf::from(format!("{}-still.png", base.display()));

        self.progress("loading", 0, 0);
        let mut server = server::ProjectServer::start(dir.clone(), plan.entry.clone(), gsap())
            .map_err(|e| format!("server_failed: {e}"))?;
        let job = Job {
            plan: &plan,
            url: server.url(&plan.entry),
            port: server.port(),
            chrome,
            alpha,
        };
        let outcome = self.with_page(&job, &server, |rt, opened| {
            // The frame nearest `at`, as for a video frame, clamped to the last one.
            let total = (opened.duration * plan.fps as f64).ceil() as u64;
            if total == 0 || total > rightkit_motion::MAX_FRAMES {
                return Err(format!("invalid_duration: {total} frames"));
            }
            let frame = ((at * plan.fps as f64).round() as u64).min(total - 1);
            let mut source = ChromeSource {
                rt: rt.clone(),
                page: opened.page.clone(),
                alpha,
                frame,
            };
            let png = capture_still(
                &mut source,
                frame as f64 / plan.fps as f64,
                Some((plan.width, plan.height)),
            )
            .map_err(|e| encode::render_error(e, None))?;
            Ok((png, frame))
        });
        server.shutdown();

        match outcome {
            Ok((png, frame)) => {
                rightkit_fs::atomic_write(&output, &png).map_err(|e| format!("io_error: {e}"))?;
                self.progress("done", 1, 1);
                Ok(StillResult {
                    status: "completed",
                    output_path: output.to_string_lossy().into_owned(),
                    sha256: rightkit_media_gen::library::sha256_hex(&png),
                    bytes: png.len() as u64,
                    frame,
                    time_s: frame as f64 / plan.fps as f64,
                    width: plan.width,
                    height: plan.height,
                    fps: plan.fps,
                    alpha,
                    elapsed_ms: started.elapsed().as_millis(),
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&output);
                self.progress("failed", 0, 0);
                Err(e)
            }
        }
    }

    /// Launch Chrome, load the composition and wait for its metadata on a runtime that keeps the
    /// browser connection alive, then run `work` (which blocks on that runtime per frame) and shut the
    /// session down on every path.
    fn with_page<T>(
        &self,
        job: &Job<'_>,
        server: &server::ProjectServer,
        work: impl FnOnce(&Arc<tokio::runtime::Runtime>, &Opened) -> Result<T, String>,
    ) -> Result<T, String> {
        // Two workers: the browser connection's I/O keeps running while a frame source blocks on it.
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| format!("runtime_failed: {e}"))?,
        );
        let opened = rt.block_on(self.open(job, server))?;
        let result = work(&rt, &opened);
        rt.block_on(opened.session.shutdown());
        result
    }

    async fn open(&self, job: &Job<'_>, server: &server::ProjectServer) -> Result<Opened, String> {
        let port = job.port;
        let options = LaunchOptions {
            chrome_path: Some(job.chrome.clone()),
            viewport: (job.plan.width, job.plan.height),
            request_timeout: PAGE_TIMEOUT,
            extra_args: CHROME_FLAGS.iter().map(|f| f.to_string()).collect(),
            // Loopback stays blocked except for this render's own project server; public hosts
            // (web fonts, remote images) keep working as they did in the webview route.
            network: rightkit_browser::NetworkPolicy::default().allow_with(
                move |r: &rightkit_browser::BrowserRequest| {
                    r.host == "127.0.0.1" && r.port == Some(port)
                },
            ),
            ..LaunchOptions::default()
        };
        let session = BrowserSession::launch(options)
            .await
            .map_err(browser_error)?;
        match self.load(job, server, &session).await {
            Ok((page, duration, meta)) => Ok(Opened {
                session,
                page,
                duration,
                meta,
            }),
            Err(e) => {
                session.shutdown().await;
                Err(e)
            }
        }
    }

    async fn load(
        &self,
        job: &Job<'_>,
        server: &server::ProjectServer,
        session: &BrowserSession,
    ) -> Result<(Arc<rightkit_browser::BrowserPage>, f64, Value), String> {
        let page = session.new_page(&job.url).await.map_err(browser_error)?;
        page.set_viewport(job.plan.width, job.plan.height, 1.0)
            .await
            .map_err(browser_error)?;
        if job.alpha {
            // Fully transparent default background, so uncovered page area captures as alpha 0.
            page.set_transparent_background(true)
                .await
                .map_err(browser_error)?;
        }

        // Wait for the runtime's metadata (it loads sub-compositions and seeks to frame 0 first).
        let deadline = Instant::now() + PAGE_TIMEOUT;
        let meta = loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            match page.eval("window.__hf_meta || null").await {
                Ok(v) if !v.is_null() => break v,
                Ok(_) => {}
                Err(e) => return Err(browser_error(e)),
            }
            if Instant::now() >= deadline {
                let tail = server.logs().join(" | ");
                let console = page
                    .console()
                    .await
                    .iter()
                    .rev()
                    .take(5)
                    .map(|l| format!("{l:?}"))
                    .collect::<Vec<_>>()
                    .join(" | ");
                return Err(format!(
                    "page_timeout: the composition did not respond ({tail}{console})"
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let duration = meta["duration"]
            .as_f64()
            .filter(|d| *d > 0.0)
            .ok_or("no_duration: the root composition needs data-duration")?;
        let total = (duration * job.plan.fps as f64).ceil() as u64;
        if total == 0 || total > doc::MAX_FRAMES {
            return Err(format!("invalid_duration: {total} frames"));
        }
        Ok((page, duration, meta))
    }
}

/// One PNG capture; `alpha` keeps the transparent background (omit-background screenshot).
async fn capture_png(page: &rightkit_browser::BrowserPage, alpha: bool) -> Result<Vec<u8>, String> {
    if alpha {
        page.screenshot_with(rightkit_browser::ScreenshotOptions {
            omit_background: true,
        })
        .await
        .map_err(browser_error)
    } else {
        page.screenshot().await.map_err(browser_error)
    }
}

/// The page expression for frame time `t`: seek everything, then let two animation frames (or
/// 250 ms, if the page gets none) pass so the paint matches the seek before the screenshot.
fn seek_script(t: f64) -> String {
    format!(
        "window.__hf_seek({t:.6}).then(function(){{return Promise.race([new Promise(function(r){{requestAnimationFrame(function(){{requestAnimationFrame(function(){{r(true)}})}})}}),new Promise(function(r){{setTimeout(function(){{r(true)}},250)}})])}})"
    )
}

fn audio_inputs(meta: &Value, dir: &std::path::Path) -> Vec<rightkit_motion::AudioInput> {
    meta["audio"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let path = server::resolve_inside(dir, a["src"].as_str()?)?;
                    path.is_file().then(|| rightkit_motion::AudioInput {
                        path,
                        start: a["start"].as_f64().unwrap_or(0.0),
                        duration: a["duration"].as_f64(),
                        media_start: a["media_start"].as_f64().unwrap_or(0.0),
                        volume: a["volume"].as_f64().unwrap_or(1.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn browser_error(error: BrowserError) -> String {
    match error {
        BrowserError::ChromeForTestingNotFound(_)
        | BrowserError::ChromeNotFound
        | BrowserError::ChromeSandboxAccess { .. } => {
            format!("browser_unavailable: {error}")
        }
        other => format!("browser_failed: {other}"),
    }
}

fn default_out_dir() -> PathBuf {
    PathBuf::from(roots::content_root(&EnvValues::from_process()))
        .join(&rightkit_local_gen::host::config().content_subdir)
        .join("motion")
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c)
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-')
        }
    }
    let t = out.trim_end_matches('-');
    if t.is_empty() {
        "hyperframes-video".into()
    } else {
        t.chars().take(60).collect()
    }
}

/// `h264_encoder_unavailable: x` becomes code `h264_encoder_unavailable`; text with no
/// `snake_case:` prefix becomes `motion_error`. The message always stays the whole original text,
/// so the shell can hand it to the webview unchanged.
fn error_from_message(message: String) -> EngineError {
    let code = message
        .split_once(':')
        .map(|(head, _)| head)
        .filter(|head| {
            !head.is_empty()
                && head
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("motion_error")
        .to_string();
    EngineError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::new(Arc::new(|_, _| {}))
    }

    fn request(extra: Value) -> RenderRequest {
        let mut base =
            json!({ "project_dir": "p", "width": 1280, "height": 720, "operator_approved": true });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            base[k] = v;
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn only_motion_names_are_motion_commands() {
        assert!(is_command("motion.render") && is_command("motion.status"));
        assert!(!is_command("local_image.runtime"));
    }

    #[test]
    fn a_legacy_request_plans_with_defaults_and_validates_its_ranges() {
        let p = plan(&request(json!({}))).unwrap();
        assert_eq!(
            p,
            Plan {
                project_dir: "p".into(),
                entry: "index.html".into(),
                fps: 30,
                width: 1280,
                height: 720
            }
        );
        assert!(plan(&request(json!({ "fps": 0 })))
            .unwrap_err()
            .starts_with("invalid_input"));
        assert!(plan(&request(json!({ "fps": 121 }))).is_err());
        assert!(plan(&request(json!({ "quality": "ultra" }))).is_err());
        assert!(plan(&request(json!({ "width": 8 }))).is_err());
        assert_eq!(
            plan(&request(json!({ "entry_file": "a\\b.html" })))
                .unwrap()
                .entry,
            "a/b.html"
        );
    }

    #[test]
    fn a_document_decides_project_entry_and_size() {
        let document = doc::CompositionDoc::from_html("D:/proj", "main.html", 24, 640, 360, 48);
        let mut req = request(json!({}));
        req.document = Some(document);
        let p = plan(&req).unwrap();
        assert_eq!(
            p,
            Plan {
                project_dir: "D:/proj".into(),
                entry: "main.html".into(),
                fps: 24,
                width: 640,
                height: 360
            }
        );
        let mut bad = request(json!({}));
        let mut d = doc::CompositionDoc::from_html("D:/proj", "main.html", 24, 640, 360, 48);
        d.layers[0].template = "mystery".into();
        bad.document = Some(d);
        assert!(plan(&bad).unwrap_err().starts_with("unknown_template"));
    }

    #[test]
    fn render_needs_the_operators_approval_before_anything_else() {
        let e = host()
            .dispatch(
                "motion.render",
                &json!({ "project_dir": "p", "width": 640, "height": 360 }),
            )
            .unwrap_err();
        assert_eq!(e.code, "approval_required");
    }

    #[test]
    fn a_second_render_is_refused_while_one_runs_and_cancel_reports_it() {
        let h = host();
        assert_eq!(
            h.dispatch("motion.cancel", &json!({})).unwrap(),
            json!(false)
        );
        h.running.store(true, Ordering::SeqCst);
        let e = h.dispatch("motion.render", &json!({ "project_dir": "p", "width": 640, "height": 360, "operator_approved": true })).unwrap_err();
        assert_eq!(e.code, "busy");
        assert_eq!(
            h.dispatch("motion.cancel", &json!({})).unwrap(),
            json!(true)
        );
        assert!(h.cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn a_missing_project_is_a_typed_error_and_frees_the_running_flag() {
        let h = host();
        let e = h
            .dispatch("motion.render", &json!({ "project_dir": "Z:/definitely/not/here", "width": 640, "height": 360, "operator_approved": true }))
            .unwrap_err();
        assert_eq!(e.code, "project_missing");
        assert!(!h.running.load(Ordering::SeqCst));
    }

    #[test]
    fn unknown_names_and_bad_input_are_typed() {
        assert_eq!(
            host()
                .dispatch("motion.bogus", &json!({}))
                .unwrap_err()
                .code,
            "unknown_command"
        );
        assert_eq!(
            host()
                .dispatch("motion.render", &json!({ "fps": "fast" }))
                .unwrap_err()
                .code,
            "invalid_input"
        );
    }

    #[test]
    fn errors_keep_their_text_and_take_a_code_from_the_prefix() {
        let e = error_from_message("h264_encoder_unavailable: none; details={}".to_string());
        assert_eq!(e.code, "h264_encoder_unavailable");
        assert!(e.message.starts_with("h264_encoder_unavailable: none"));
        assert_eq!(
            error_from_message("cancelled".to_string()).code,
            "motion_error"
        );
        assert_eq!(
            error_from_message("Disk full: x".to_string()).code,
            "motion_error"
        );
    }

    #[test]
    fn the_seek_script_is_a_pure_function_of_the_frame_time() {
        assert_eq!(seek_script(1.0 / 3.0), seek_script(1.0 / 3.0));
        assert!(seek_script(0.5).starts_with("window.__hf_seek(0.500000)"));
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(slugify("My Great Video!!"), "my-great-video");
        assert_eq!(slugify("***"), "hyperframes-video");
    }

    #[test]
    fn audio_inputs_stay_inside_the_project() {
        let dir = std::env::temp_dir().join(format!("gr-motion-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.wav"), b"x").unwrap();
        let meta = json!({ "audio": [
            { "src": "a.wav", "start": 1.0, "duration": 2.0, "volume": 0.5 },
            { "src": "../escape.wav" },
            { "src": "missing.wav" }
        ]});
        let inputs = audio_inputs(&meta, &dir);
        assert_eq!(inputs.len(), 1);
        assert_eq!((inputs[0].start, inputs[0].volume), (1.0, 0.5));
        let _ = std::fs::remove_dir_all(dir);
    }
}
