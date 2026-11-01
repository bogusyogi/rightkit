//! The browser itself: Chromium through `rightkit-browser` (see `Cargo.toml`), on a Mac or a Linux
//! computer. One session at a time, started only after the owner approved a request
//! (the crate root) and gone when it ends.
//!
//! * The login store is the named profile `logins` under `<state>/browser/profiles`, created
//!   mode 0700. `rightkit-browser` locks it, so a second Chromium on it is refused.
//! * The network policy lets a document (a navigation or a frame) reach only the approved hosts
//!   and their subdomains; scripts, styles and images may come from anywhere public, because a
//!   sign-in page needs them. The crate's own SSRF rules (loopback, private, link-local,
//!   metadata) still apply to everything. The admission hook refuses `eval` and file upload and
//!   refuses a navigation to a host that is not approved. Typed text reaches the hook only as a
//!   length (the crate's rule), never here.
//! * Chromium is found with the crate's own search: Chrome for Testing in RightKit's managed
//!   folder, `RIGHTKIT_CHROME_FOR_TESTING`, Puppeteer or Playwright caches, then (this uses
//!   `system_chrome`) Chrome, Chromium or Edge on the path or `CHROME`. Nothing is installed here.
//! * Downloads are denied (the session's download behaviour is set to deny before any
//!   navigation), popups are blocked by Chromium's own flag and any other tab is closed by
//!   `reap_extra` (BD17, #577).
//! * Page content for the agent (`observe`) is read only when the owner's signed approval granted
//!   it (BD14); the screenshot goes back in the answer and is never written to disk (BD3).
//! * Frames are JPEG, quality 60 and at most 1280 wide, scaled down by steps until the answer
//!   fits the mesh line (256 KiB with the JSON around it).
//!
//! Every call into the crate runs on a runtime this module owns, created when a session starts and
//! dropped when it ends, so an idle computer has no browser thread. Callers are plain threads.

#![cfg_attr(
    not(all(feature = "browser", any(target_os = "linux", target_os = "macos"))),
    allow(dead_code, unused_imports)
)]

use std::path::Path;
#[cfg(all(feature = "browser", any(target_os = "linux", target_os = "macos")))]
use std::sync::Mutex;

use serde_json::{Value, json};

use super::Input;
use rightkit_approvals::PhoneError;

/// One live-view frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub seq: u64,
    pub jpeg_b64: String,
    pub width: u32,
    pub height: u32,
    pub url: String,
    pub title: String,
    /// The page has not changed since the viewer's `after_seq` (BD12): the picture is the one
    /// the viewer already holds.
    pub unchanged: bool,
}

/// The answer line may hold 256 KiB (`MESH_LINE_MAX`); the JSON around the picture is small, so
/// the base64 text stops here.
pub const FRAME_B64_MAX: usize = 200_000;
/// The page size of every session, in CSS pixels. A frame is never wider.
pub const VIEWPORT: (u32, u32) = (1280, 800);
/// JPEG quality of a frame at full size; later steps trade it and the size for fit.
const QUALITY_STEPS: [(i64, f64); 5] = [(60, 1.0), (50, 0.85), (40, 0.7), (35, 0.55), (30, 0.4)];
/// The visible text `observe` returns when the owner granted page content, in characters.
pub const OBSERVE_TEXT_MAX: usize = 20_000;

fn unavailable(message: impl Into<String>) -> PhoneError {
    PhoneError::new("browser_unavailable", message)
}

#[cfg(all(feature = "browser", any(target_os = "linux", target_os = "macos")))]
pub fn supported() -> bool {
    true
}

#[cfg(not(all(feature = "browser", any(target_os = "linux", target_os = "macos"))))]
pub fn supported() -> bool {
    false
}

// ---- Mac and Linux ----------------------------------------------------------------------------

#[cfg(all(feature = "browser", any(target_os = "linux", target_os = "macos")))]
mod live {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, Viewport};
    use chromiumoxide::page::ScreenshotParams;
    use rightkit_browser::{
        ActionKind, AdmissionRequest, BrowserError, BrowserPage, BrowserRequest, BrowserSession,
        Decision, DownloadBehavior, LaunchOptions, NetworkPolicy, PolicyDecision, Target,
    };

    use super::*;
    use crate::host_allowed;

    pub struct Live {
        rt: tokio::runtime::Runtime,
        session: Arc<BrowserSession>,
        page: Arc<BrowserPage>,
        /// The scale of the last frame, as thousandths (the owner's clicks are in frame pixels).
        scale_milli: AtomicU64,
        seq: AtomicU64,
        last: Mutex<Option<Cached>>,
        /// The id of the one page of the session; any other tab is closed (BD17).
        primary: String,
    }

    /// The last frame, with a fingerprint of the full-size capture it came from (BD12): while the
    /// page is unchanged a poll costs one capture, not up to five encodings.
    struct Cached {
        print: String,
        frame: Frame,
    }

    fn map_error(error: BrowserError) -> PhoneError {
        let text = |e: &BrowserError| {
            let mut t = e.to_string();
            t.truncate(240);
            t
        };
        match &error {
            BrowserError::ChromeNotFound | BrowserError::ChromeForTestingNotFound(_) => {
                unavailable(
                    "no Chrome for Testing, Chrome, Chromium or Edge was found on this computer",
                )
            }
            BrowserError::ChromeSandboxAccess { .. } => unavailable(text(&error)),
            BrowserError::ProfileInUse(_) => PhoneError::retry(
                "busy",
                "the login store is still in use by the last session; try again shortly",
            ),
            BrowserError::Blocked(_) | BrowserError::Denied(_) => PhoneError::new(
                "site_not_approved",
                "that address is not on a site the owner approved for this session",
            ),
            BrowserError::Timeout(_) => {
                PhoneError::retry("browser_timeout", "the page did not answer in time")
            }
            BrowserError::UnknownKey(_) | BrowserError::Invalid(_) => {
                PhoneError::new("invalid_request", text(&error))
            }
            _ => PhoneError::new("browser_failed", text(&error)),
        }
    }

    impl Live {
        /// Runs a browser future on this session's runtime and waits for it. The future is spawned,
        /// not blocked on, so any thread may call this.
        fn run<T: Send + 'static>(
            &self,
            seconds: u64,
            fut: impl std::future::Future<Output = T> + Send + 'static,
        ) -> Result<T, PhoneError> {
            let (tx, rx) = std::sync::mpsc::channel();
            self.rt.spawn(async move {
                let _ = tx.send(fut.await);
            });
            rx.recv_timeout(Duration::from_secs(seconds)).map_err(|_| {
                PhoneError::retry("browser_timeout", "the browser did not answer in time")
            })
        }
    }

    pub fn start(root: &Path, sites: &[String]) -> Result<Live, PhoneError> {
        // Chromium without its sandbox is refused: it would run web content as root.
        if unsafe { libc::geteuid() } == 0 {
            return Err(unavailable(
                "this computer runs the app as root; the agent browser needs an ordinary account",
            ));
        }
        let profiles = root.join("profiles");
        for dir in [
            root.to_path_buf(),
            profiles.clone(),
            profiles.join("logins"),
        ] {
            private_dir(&dir).map_err(|e| PhoneError::retry("state_unwritable", e.to_string()))?;
        }
        let approved: Vec<String> = sites.to_vec();
        let for_policy = approved.clone();
        let policy = NetworkPolicy::default().allow_with(move |request: &BrowserRequest| {
            // A document (a page, a frame) may go only where the owner said; everything the page
            // fetches for itself follows the crate's public-address rules.
            let document = matches!(request.resource_type.as_str(), "Navigation" | "Document");
            // Plain `http` carries a logged-in session in the clear: no document and no
            // subresource goes over it (BD2, #576).
            if request.scheme.eq_ignore_ascii_case("http") {
                PolicyDecision::Deny("insecure_scheme".to_string())
            } else if document && !host_allowed(&request.host, &for_policy) {
                PolicyDecision::Deny("site_not_approved".to_string())
            } else {
                PolicyDecision::Default
            }
        });
        let for_hook = approved.clone();
        let admission: rightkit_browser::AdmissionHook = Arc::new(move |request: AdmissionRequest| -> std::pin::Pin<Box<dyn std::future::Future<Output = Decision> + Send>> {
            let approved = for_hook.clone();
            Box::pin(async move {
                match request.action {
                    // The one script-class action allowed is the session's own download denial.
                    ActionKind::Eval
                        if request
                            .detail
                            .as_deref()
                            .is_some_and(|d| d.starts_with("download_behavior Deny")) =>
                    {
                        Decision::Allow
                    }
                    ActionKind::Eval | ActionKind::Upload => {
                        Decision::Deny("not allowed in an agent browser session".to_string())
                    }
                    ActionKind::Navigate => {
                        let host = request
                            .url
                            .as_deref()
                            .and_then(crate::url_host);
                        match host {
                            Some(host) if host_allowed(&host, &approved) => Decision::Allow,
                            _ => Decision::Deny("site_not_approved".to_string()),
                        }
                    }
                    _ => Decision::Allow,
                }
            })
        });
        let options = LaunchOptions::default()
            .system_chrome()
            .headless(true)
            .named_profile(profiles, "logins")
            .network(policy)
            .admission(admission);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rightkit-browser")
            .enable_all()
            .build()
            .map_err(|e| PhoneError::retry("browser_failed", e.to_string()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let launched = async {
                let session = BrowserSession::launch(options).await?;
                let page = match session.new_page("about:blank").await {
                    Ok(page) => page,
                    Err(error) => {
                        session.shutdown().await;
                        return Err(error);
                    }
                };
                if let Err(error) = page.set_viewport(VIEWPORT.0, VIEWPORT.1, 1.0).await {
                    session.shutdown().await;
                    return Err(error);
                }
                // The launch default lets pages save files into the profile; no download is
                // allowed in an agent session (BD17). A failure ends the start: fail closed.
                if let Err(error) = page.set_download_behavior(&DownloadBehavior::Deny).await {
                    session.shutdown().await;
                    return Err(error);
                }
                Ok::<_, BrowserError>((Arc::new(session), page))
            }
            .await;
            let _ = tx.send(launched);
        });
        let (session, page) = rx
            .recv_timeout(Duration::from_secs(90))
            .map_err(|_| PhoneError::retry("browser_timeout", "the browser did not start in time"))?
            .map_err(map_error)?;
        let primary = page.id().to_string();
        Ok(Live {
            rt,
            session,
            page,
            scale_milli: AtomicU64::new(1000),
            seq: AtomicU64::new(0),
            last: Mutex::new(None),
            primary,
        })
    }

    /// A folder only this account can use. An existing one is tightened.
    fn private_dir(dir: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.is_dir() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }

    impl Live {
        pub fn stop(self: Arc<Self>) {
            let session = self.session.clone();
            let _ = self.run(40, async move { session.shutdown().await });
            // `self` (and its runtime) drops on the caller's plain thread.
        }

        pub fn alive(&self) -> bool {
            match self.session.process_id() {
                Some(pid) => unsafe { libc::kill(pid as i32, 0) == 0 },
                None => false,
            }
        }

        pub fn goto(&self, url: &str) -> Result<(), PhoneError> {
            let page = self.page.clone();
            let url = url.to_string();
            self.run(60, async move { page.goto(&url).await })?
                .map_err(map_error)
        }

        pub fn describe(&self) -> (String, String) {
            let page = self.page.clone();
            self.run(10, async move { (page.url().await, page.title().await) })
                .unwrap_or_default()
        }

        /// Closes every tab but the session's own page and returns how many it closed (BD17).
        pub fn reap_extra(&self) -> usize {
            let session = self.session.clone();
            let primary = self.primary.clone();
            self.run(10, async move {
                let extra: Vec<String> = session
                    .pages()
                    .iter()
                    .map(|p| p.id().to_string())
                    .filter(|id| *id != primary)
                    .collect();
                let mut closed = 0;
                for id in extra {
                    if session.close_page(&id).await.is_ok() {
                        closed += 1;
                    }
                }
                closed
            })
            .unwrap_or(0)
        }

        /// The address and title always; the visible text and a screenshot only when the owner's
        /// approval granted page content (`share`, BD14). The text is redacted of obvious secrets.
        /// The screenshot is a JPEG inside the answer and is never written to disk (BD3).
        pub fn observe(&self, share: bool) -> Result<Value, PhoneError> {
            let (url, title) = self.describe();
            if !share {
                return Ok(json!({
                    "url": url, "title": title, "content": "withheld",
                    "message": "the owner did not allow page text or screenshots for this session; \
                                ask again and the approval card will offer it",
                }));
            }
            let page = self.page.clone();
            let text = self
                .run(30, async move { page.text().await })?
                .map_err(map_error)?;
            let mut chosen: Option<Vec<u8>> = None;
            for (quality, scale) in QUALITY_STEPS {
                let page = self.page.clone();
                let bytes = self
                    .run(20, async move { capture(&page, quality, scale).await })?
                    .map_err(|e| PhoneError::new("browser_failed", e))?;
                let fits = base64_len(bytes.len()) <= FRAME_B64_MAX;
                chosen = Some(bytes);
                if fits {
                    break;
                }
            }
            let bytes = chosen.ok_or_else(|| PhoneError::new("browser_failed", "no screenshot"))?;
            let shot = if base64_len(bytes.len()) <= FRAME_B64_MAX {
                Value::String(rightkit_crypto::base64_encode(&bytes))
            } else {
                Value::Null
            };
            let text: String = text.chars().take(OBSERVE_TEXT_MAX).collect();
            let (text, redacted) = rightkit_approvals::redact::redact(&text);
            Ok(json!({
                "url": url, "title": title, "text": text, "redacted": redacted,
                "screenshot_jpeg_base64": shot,
            }))
        }

        pub fn input(&self, events: &[Input], scale: f64) -> Result<(), PhoneError> {
            let scale = if scale.is_finite() && scale > 0.05 {
                scale
            } else {
                1.0
            };
            for event in events {
                let page = self.page.clone();
                let event = event.clone();
                self.run(30, async move {
                    match event {
                        Input::Click { x, y } => {
                            page.click(&Target::Point(x / scale, y / scale)).await
                        }
                        Input::Text(text) => page.type_text(&text).await,
                        Input::Key(key) => page.press(&key).await,
                        Input::Scroll { dy } => page.wheel(None, 0.0, dy / scale).await,
                    }
                })?
                .map_err(map_error)?;
            }
            Ok(())
        }

        pub fn frame_scale(&self) -> f64 {
            self.scale_milli.load(Ordering::Relaxed) as f64 / 1000.0
        }

        pub fn frame(&self, after_seq: u64) -> Result<Frame, PhoneError> {
            // One full-size capture first. When it is the one the last answer came from, the page
            // has not changed and that answer is returned as it stands (BD12).
            let (quality0, scale0) = QUALITY_STEPS[0];
            let page = self.page.clone();
            let first = self
                .run(20, async move { capture(&page, quality0, scale0).await })?
                .map_err(|e| PhoneError::new("browser_failed", e))?;
            let print = fingerprint(&first);
            let cached = {
                let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
                last.as_ref()
                    .filter(|c| c.print == print)
                    .map(|c| c.frame.clone())
            };
            if let Some(mut frame) = cached {
                let (url, title) = self.describe();
                frame.url = url;
                frame.title = title;
                frame.unchanged = frame.seq == after_seq;
                return Ok(frame);
            }
            let mut chosen: Option<(Vec<u8>, f64)> = None;
            let mut next = Some(first);
            for (quality, scale) in QUALITY_STEPS {
                let bytes = match next.take() {
                    Some(bytes) => bytes,
                    None => {
                        let page = self.page.clone();
                        self.run(20, async move { capture(&page, quality, scale).await })?
                            .map_err(|e| PhoneError::new("browser_failed", e))?
                    }
                };
                let fits = base64_len(bytes.len()) <= FRAME_B64_MAX;
                chosen = Some((bytes, scale));
                if fits {
                    break;
                }
            }
            let (bytes, scale) =
                chosen.ok_or_else(|| PhoneError::new("browser_failed", "no frame"))?;
            if base64_len(bytes.len()) > FRAME_B64_MAX {
                return Err(PhoneError::retry(
                    "frame_too_large",
                    "the page is too detailed to send as a picture right now",
                ));
            }
            let b64 = rightkit_crypto::base64_encode(&bytes);
            self.scale_milli
                .store((scale * 1000.0) as u64, Ordering::Relaxed);
            let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            let (url, title) = self.describe();
            let frame = Frame {
                seq,
                jpeg_b64: b64,
                width: (VIEWPORT.0 as f64 * scale) as u32,
                height: (VIEWPORT.1 as f64 * scale) as u32,
                url,
                title,
                unchanged: false,
            };
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(Cached {
                print,
                frame: frame.clone(),
            });
            Ok(frame)
        }
    }

    fn base64_len(raw: usize) -> usize {
        raw.div_ceil(3) * 4
    }

    /// A hash of a capture's bytes, to tell that the page changed.
    fn fingerprint(bytes: &[u8]) -> String {
        rightkit_crypto::hex_encode(&rightkit_crypto::sha256(bytes))
    }

    async fn capture(page: &BrowserPage, quality: i64, scale: f64) -> Result<Vec<u8>, String> {
        let mut params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Jpeg)
            .quality(quality);
        if scale < 0.999 {
            params = params.clip(Viewport {
                x: 0.0,
                y: 0.0,
                width: VIEWPORT.0 as f64,
                height: VIEWPORT.1 as f64,
                scale,
            });
        }
        page.raw()
            .screenshot(params.build())
            .await
            .map_err(|e| format!("screenshot: {e}"))
    }

    /// Whether a browser can be found, without starting one.
    pub fn availability() -> Result<(), PhoneError> {
        if unsafe { libc::geteuid() } == 0 {
            return Err(unavailable(
                "this computer runs the app as root; the agent browser needs an ordinary account",
            ));
        }
        rightkit_browser::chrome_for_testing_path(None)
            .map(|_| ())
            .or_else(|_| {
                rightkit_browser::find_chrome()
                    .map(|_| ())
                    .ok_or_else(|| {
                        unavailable(
                            "no Chrome for Testing, Chrome, Chromium or Edge was found on this computer",
                        )
                    })
            })
    }
}

// ---- the driver --------------------------------------------------------------------------------

#[cfg(all(feature = "browser", any(target_os = "linux", target_os = "macos")))]
pub struct Driver {
    live: Mutex<Option<std::sync::Arc<live::Live>>>,
}

#[cfg(all(feature = "browser", any(target_os = "linux", target_os = "macos")))]
impl Driver {
    pub fn new() -> Driver {
        Driver {
            live: Mutex::new(None),
        }
    }

    pub fn availability(&self) -> Result<(), PhoneError> {
        live::availability()
    }

    fn current(&self) -> Result<std::sync::Arc<live::Live>, PhoneError> {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| PhoneError::new("not_approved", "there is no open browser session"))
    }

    pub fn start(&self, root: &Path, sites: &[String]) -> Result<(), PhoneError> {
        let started = live::start(root, sites)?;
        *self.live.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(started));
        Ok(())
    }

    pub fn stop(&self) {
        let taken = self.live.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(live) = taken {
            live.stop();
        }
    }

    pub fn alive(&self) -> bool {
        self.current().map(|l| l.alive()).unwrap_or(false)
    }

    pub fn goto(&self, url: &str) -> Result<(), PhoneError> {
        self.current()?.goto(url)
    }

    pub fn describe(&self) -> (String, String) {
        self.current().map(|l| l.describe()).unwrap_or_default()
    }

    pub fn describe_json(&self) -> Value {
        let (url, title) = self.describe();
        json!({ "url": url, "title": title })
    }

    pub fn observe(&self, share: bool) -> Result<Value, PhoneError> {
        self.current()?.observe(share)
    }

    /// Closes any tab other than the session's page; how many were closed.
    pub fn reap_extra(&self) -> usize {
        self.current().map(|l| l.reap_extra()).unwrap_or(0)
    }

    pub fn input(&self, events: &[Input], scale: f64) -> Result<(), PhoneError> {
        self.current()?.input(events, scale)
    }

    pub fn frame(&self, after_seq: u64) -> Result<Frame, PhoneError> {
        self.current()?.frame(after_seq)
    }

    pub fn frame_scale(&self) -> f64 {
        self.current().map(|l| l.frame_scale()).unwrap_or(1.0)
    }
}

// ---- other platforms --------------------------------------------------------------------------

#[cfg(not(all(feature = "browser", any(target_os = "linux", target_os = "macos"))))]
pub struct Driver;

#[cfg(not(all(feature = "browser", any(target_os = "linux", target_os = "macos"))))]
impl Driver {
    pub fn new() -> Driver {
        Driver
    }

    fn no<T>(&self) -> Result<T, PhoneError> {
        Err(PhoneError::new(
            "unsupported",
            "agent browser logins run on a Mac or a Linux computer",
        ))
    }

    pub fn availability(&self) -> Result<(), PhoneError> {
        self.no()
    }

    pub fn start(&self, _root: &Path, _sites: &[String]) -> Result<(), PhoneError> {
        self.no()
    }

    pub fn stop(&self) {}

    pub fn alive(&self) -> bool {
        false
    }

    pub fn goto(&self, _url: &str) -> Result<(), PhoneError> {
        self.no()
    }

    pub fn describe(&self) -> (String, String) {
        (String::new(), String::new())
    }

    pub fn describe_json(&self) -> Value {
        json!({ "url": "", "title": "" })
    }

    pub fn observe(&self, _share: bool) -> Result<Value, PhoneError> {
        self.no()
    }

    pub fn reap_extra(&self) -> usize {
        0
    }

    pub fn input(&self, _events: &[Input], _scale: f64) -> Result<(), PhoneError> {
        self.no()
    }

    pub fn frame(&self, _after_seq: u64) -> Result<Frame, PhoneError> {
        self.no()
    }

    pub fn frame_scale(&self) -> f64 {
        1.0
    }
}
