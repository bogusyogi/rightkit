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
use rightkit_owner_approvals::PhoneError;

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
/// Most controls one observation lists, in page order; `truncated` says more were there.
const NODES_MAX: usize = 150;
/// Longest control name an observation carries, in characters.
const NAME_MAX: usize = 100;
/// Most entries in one list of an observation's `changes`.
const CHANGES_SHOWN: usize = 25;
/// How long a page gets to react after an action before it is read again.
const SETTLE_MS: u64 = 250;

/// What the last reading of the page says one ref is.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    pub role: String,
    /// Masked secrets, cut; `••••••••` for a password-like field.
    pub name: String,
    /// A password-like field: its name is masked and the agent never types into it.
    pub sensitive: bool,
    /// The page's address when it was read.
    pub url: String,
}

/// The host rule of a session, applied to every request the page makes (AG-10): no plain `http`;
/// a document (a navigation, a frame) only on an approved host; and the requests a page's script
/// makes for itself (`XHR`, `Fetch`, a socket, an event stream, a ping, a CORS preflight, a
/// connection no page check saw) only to an approved host too, so a script that read the page
/// cannot carry it to an address of its own choosing. Scripts, styles, images and fonts may still
/// come from any public address, because a sign-in page needs them; that is the one request kind
/// a page can use to send a short value out (an image address), and the owner's approval of the
/// sites is what bounds it. `None` lets the crate's own rules decide.
pub(crate) fn request_denied(
    resource_type: &str,
    scheme: &str,
    host: &str,
    sites: &[String],
) -> Option<&'static str> {
    if scheme.eq_ignore_ascii_case("http") {
        return Some("insecure_scheme");
    }
    let is = |name: &str| resource_type.eq_ignore_ascii_case(name);
    let document = is("Navigation") || is("Document");
    let sends = [
        "XHR",
        "Fetch",
        "WebSocket",
        "EventSource",
        "Ping",
        "CSPViolationReport",
        "Preflight",
        "Connect",
    ]
    .iter()
    .any(|name| is(name));
    // A request with no host (`data:`, `blob:`) leaves nothing to a network.
    let reaches = document || (sends && !host.is_empty());
    (reaches && !super::host_allowed(host, sites)).then_some("site_not_approved")
}

/// Whether `script` is one of the two the browser crate runs itself to find the element behind a
/// ref and its centre (`BrowserPage::click` and its siblings go through them, and so does a
/// `<select>` choice). The session never evaluates a script a caller wrote: there is no such
/// call, so these are the only scripts the admission hook lets through.
pub(crate) fn internal_script(script: &str) -> bool {
    script.starts_with("(()=>{const l=document.querySelectorAll(")
        || script.starts_with("(()=>{const e=document.querySelector(")
}

/// Why a secret was not typed into the agent browser's page (`Driver::fill_secret`). Nothing here
/// carries the value.
pub enum SecretFillError {
    /// The page's own origin is not the origin the caller verified.
    OriginMismatch,
    /// The element is not a password or card field, the only kind the crate types a secret into.
    NotCredentialField,
    /// Refused before anything was typed: a stale or missing ref, an ambiguous or covered element,
    /// or an argument the crate rejected. Nothing reached the page.
    Untyped(PhoneError),
    /// Anything else (no session, a timeout, a browser failure): the value may or may not have been
    /// typed.
    Other(PhoneError),
}

/// Longest text or value of one action, in characters.
const ACT_TEXT_MAX: usize = 1000;

/// How a node's `ref` looks: `r<epoch>.<number>`.
fn valid_ref(node: &str) -> bool {
    node.len() <= 24
        && node
            .strip_prefix('r')
            .and_then(|rest| rest.split_once('.'))
            .is_some_and(|(epoch, n)| {
                !epoch.is_empty()
                    && !n.is_empty()
                    && epoch.bytes().all(|b| b.is_ascii_digit())
                    && n.bytes().all(|b| b.is_ascii_digit())
            })
}

/// What an observation carries beside the controls.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    /// Characters of the page's visible text to add (secrets masked); 0 adds none.
    pub text_chars: usize,
    /// Whether and how a screenshot comes with it.
    pub shot: Shot,
}

/// A screenshot in an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shot {
    /// None.
    No,
    /// As `screenshot_jpeg_base64`, the command line's field.
    Field,
    /// As `screenshot: {base64, mime}`, the shape the agent's broker turns into image content.
    Image,
}

/// One action on the page the agent asks for (AG-10). A node is a `ref` from the last observation.
#[derive(Clone, Debug)]
pub enum BrowserAct {
    Click {
        node: String,
    },
    Hover {
        node: String,
    },
    Fill {
        node: String,
        text: String,
        clear: bool,
    },
    Select {
        node: String,
        value: String,
    },
    Press {
        key: String,
    },
    Type {
        text: String,
    },
    Scroll {
        node: Option<String>,
        dy: f64,
    },
}

impl BrowserAct {
    /// The action's word, as the agent wrote it.
    pub fn kind(&self) -> &'static str {
        match self {
            BrowserAct::Click { .. } => "click",
            BrowserAct::Hover { .. } => "hover",
            BrowserAct::Fill { .. } => "fill",
            BrowserAct::Select { .. } => "select",
            BrowserAct::Press { .. } => "press",
            BrowserAct::Type { .. } => "type",
            BrowserAct::Scroll { .. } => "scroll",
        }
    }

    /// The ref the action addresses, when it has one.
    pub fn node(&self) -> Option<&str> {
        match self {
            BrowserAct::Click { node }
            | BrowserAct::Hover { node }
            | BrowserAct::Fill { node, .. }
            | BrowserAct::Select { node, .. } => Some(node),
            BrowserAct::Scroll { node, .. } => node.as_deref(),
            BrowserAct::Press { .. } | BrowserAct::Type { .. } => None,
        }
    }
}

/// `{"type": "click"|"hover"|"fill"|"select"|"press"|"type"|"scroll", "ref", "text", "clear",
/// "value", "key", "dy"}` as one [`BrowserAct`]. A ref is required where the action addresses a
/// control; `scroll` takes one optionally.
pub fn parse_act(action: &Value) -> Result<BrowserAct, PhoneError> {
    let bad = |what: &str| PhoneError::new("invalid_request", what);
    let node = |required: bool| -> Result<Option<String>, PhoneError> {
        match action.get("ref") {
            None | Some(Value::Null) if !required => Ok(None),
            Some(Value::String(r)) if valid_ref(r) => Ok(Some(r.clone())),
            _ => Err(bad(
                "ref is a control's ref from the last observation, such as r3.12",
            )),
        }
    };
    let text = |key: &str, empty_ok: bool| -> Result<String, PhoneError> {
        action
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| (empty_ok || !t.is_empty()) && t.chars().count() <= ACT_TEXT_MAX)
            .map(str::to_string)
            .ok_or_else(|| bad("text and value are plain text of at most 1000 characters"))
    };
    match action.get("type").and_then(Value::as_str) {
        Some("click") => Ok(BrowserAct::Click {
            node: node(true)?.unwrap_or_default(),
        }),
        Some("hover") => Ok(BrowserAct::Hover {
            node: node(true)?.unwrap_or_default(),
        }),
        Some("fill") => Ok(BrowserAct::Fill {
            node: node(true)?.unwrap_or_default(),
            text: text("text", true)?,
            clear: action.get("clear").and_then(Value::as_bool).unwrap_or(true),
        }),
        Some("select") => Ok(BrowserAct::Select {
            node: node(true)?.unwrap_or_default(),
            value: text("value", false)?,
        }),
        Some("press") => {
            let key = action
                .get("key")
                .and_then(Value::as_str)
                .filter(|k| !k.is_empty() && k.len() <= 40 && !k.chars().any(char::is_control))
                .ok_or_else(|| bad("key is a key name such as Enter or Tab"))?;
            Ok(BrowserAct::Press {
                key: key.to_string(),
            })
        }
        Some("type") => Ok(BrowserAct::Type {
            text: text("text", false)?,
        }),
        Some("scroll") => {
            let dy = action
                .get("dy")
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && n.abs() <= 10_000.0)
                .ok_or_else(|| bad("dy is a plain number of pixels"))?;
            Ok(BrowserAct::Scroll {
                node: node(false)?,
                dy,
            })
        }
        _ => Err(bad(
            "an action is click, hover, fill, select, press, type or scroll",
        )),
    }
}

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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use rightkit_browser::{
        ActionKind, AdmissionRequest, BrowserError, BrowserPage, BrowserRequest, BrowserSession,
        Decision, DownloadBehavior, LaunchOptions, NetworkPolicy, PolicyDecision, ScreenshotClip,
        ScreenshotFormat, ScreenshotOptions, Target,
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
        /// The last reading of the page, for the refs and the diff (AG-10).
        snap: Mutex<Snap>,
    }

    /// One control as an observation lists it.
    #[derive(Clone, PartialEq)]
    struct Seen {
        role: String,
        name: String,
        disabled: bool,
        sensitive: bool,
    }

    /// The last reading of the page: its epoch, its controls by ref and by the crate's stable id,
    /// and its address.
    #[derive(Default)]
    struct Snap {
        epoch: u64,
        url: String,
        by_ref: HashMap<String, Seen>,
        by_id: HashMap<u64, Seen>,
    }

    /// A reading as the agent receives it.
    struct Reading {
        epoch: u64,
        url: String,
        title: String,
        /// The controls listed, at most `NODES_MAX`.
        nodes: Vec<Value>,
        total: usize,
        /// What changed since the reading before it; `Null` for the first of a page.
        changes: Value,
        /// Whether anything showed: the address, the epoch or a control changed.
        changed: bool,
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
            // Fixed words: the crate's own text names a selector, which is page content.
            BrowserError::StaleRef(_) => PhoneError::new(
                "stale_ref",
                "the page changed under that ref; nothing was done, call browser_look again",
            ),
            BrowserError::NoElement(_) => PhoneError::new(
                "no_element",
                "no control matches that ref any more; nothing was done, call browser_look again",
            ),
            BrowserError::Ambiguous(..) => PhoneError::new(
                "ambiguous_target",
                "more than one control matches that ref; nothing was done, call browser_look again",
            ),
            BrowserError::NotInteractable(_) => PhoneError::new(
                "not_interactable",
                "that control is hidden, off the page or covered by another element; nothing was done",
            ),
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
            return Err(unavailable(format!(
                "this computer runs {} as root; the agent browser needs an ordinary account",
                crate::product_name()
            )));
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
            // The host rule (`request_denied`): no plain `http` (BD2, #576), a document only where
            // the owner said, and the requests a page's script makes only there too; what is left
            // follows the crate's public-address rules.
            match request_denied(
                &request.resource_type,
                &request.scheme,
                &request.host,
                &for_policy,
            ) {
                Some(reason) => PolicyDecision::Deny(reason.to_string()),
                None => PolicyDecision::Default,
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
                    // The two scripts the crate runs to find the element behind a ref.
                    ActionKind::Eval
                        if request.detail.as_deref().is_some_and(internal_script) =>
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
            .thread_name(crate::thread_name("browser"))
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
            snap: Mutex::new(Snap::default()),
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

        /// The address and title always. With the owner's page-content grant (`share`, BD14) also
        /// the page's controls as refs with the epoch of the reading and what changed since the
        /// last one (AG-10), and what `look` asks beside them: the visible text (obvious secrets
        /// masked) and a screenshot. Control names and text are the page's own words, so the answer
        /// is marked untrusted. The screenshot is a JPEG inside the answer and is never written to
        /// disk (BD3).
        pub fn observe(&self, share: bool, look: Look) -> Result<Value, PhoneError> {
            if !share {
                let (url, title) = self.describe();
                return Ok(json!({
                    "url": url, "title": title, "content": "withheld",
                    "message": "the owner did not allow page text or screenshots for this session; \
                                ask again and the approval card will offer it",
                }));
            }
            let reading = self.read_page()?;
            let truncated = reading.total > reading.nodes.len();
            let mut answer = json!({
                "untrusted": true, "source": "web page",
                "url": reading.url, "title": reading.title, "epoch": reading.epoch,
                "nodes": reading.nodes, "total": reading.total, "truncated": truncated,
                "changes": reading.changes,
            });
            if look.text_chars > 0 {
                let page = self.page.clone();
                let text = self
                    .run(30, async move { page.text().await })?
                    .map_err(map_error)?;
                let text: String = text.chars().take(look.text_chars).collect();
                let (text, redacted) = rightkit_remote_gateway::redact::redact(&text);
                answer["text"] = Value::String(text);
                answer["redacted"] = json!(redacted);
            }
            let shot = match look.shot {
                Shot::No => None,
                Shot::Field | Shot::Image => self.screenshot()?,
            };
            match look.shot {
                Shot::No => {}
                Shot::Field => {
                    answer["screenshot_jpeg_base64"] = shot.map_or(Value::Null, Value::String);
                }
                Shot::Image => {
                    if let Some(base64) = shot {
                        answer["screenshot"] = json!({"base64": base64, "mime": "image/jpeg"});
                    }
                }
            }
            Ok(answer)
        }

        /// A JPEG of the page as base64 that fits one answer line, or `None` when none does.
        fn screenshot(&self) -> Result<Option<String>, PhoneError> {
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
            Ok((base64_len(bytes.len()) <= FRAME_B64_MAX)
                .then(|| rightkit_crypto::base64_encode(&bytes)))
        }

        /// Reads the page: its visible controls as refs, the epoch of the reading, and what
        /// changed since the reading before it. The epoch advances only when the page's controls
        /// do (the crate's own fingerprint), so a ref stays good while the page is as it was.
        fn read_page(&self) -> Result<Reading, PhoneError> {
            let page = self.page.clone();
            let seen = self
                .run(30, async move { page.observe().await })?
                .map_err(map_error)?;
            let mut snap = self.snap.lock().unwrap_or_else(|e| e.into_inner());
            let first = snap.epoch == 0;
            let new_page = !first && snap.url != seen.url;
            let mut by_ref: HashMap<String, Seen> = HashMap::new();
            let mut by_id: HashMap<u64, Seen> = HashMap::new();
            let mut nodes: Vec<Value> = Vec::new();
            let mut total = 0usize;
            let (mut added, mut changed_list) = (Vec::new(), Vec::new());
            let (mut n_added, mut n_changed) = (0usize, 0usize);
            for node in seen
                .nodes
                .iter()
                .filter(|n| n.visible && !n.selector.is_empty())
            {
                total += 1;
                let entry = Seen {
                    role: node.role.clone(),
                    name: clean_name(&node.name, node.sensitive),
                    disabled: node.disabled,
                    sensitive: node.sensitive,
                };
                let mut item =
                    json!({"ref": node.node_ref, "role": entry.role, "name": entry.name});
                if entry.disabled {
                    item["disabled"] = json!(true);
                }
                if entry.sensitive {
                    item["sensitive"] = json!(true);
                }
                if !first && !new_page && node.stable_id != 0 {
                    match snap.by_id.get(&node.stable_id) {
                        None => {
                            n_added += 1;
                            if added.len() < CHANGES_SHOWN {
                                added.push(item.clone());
                            }
                        }
                        Some(before) if *before != entry => {
                            n_changed += 1;
                            if changed_list.len() < CHANGES_SHOWN {
                                changed_list.push(item.clone());
                            }
                        }
                        Some(_) => {}
                    }
                }
                if nodes.len() < NODES_MAX {
                    nodes.push(item);
                }
                if node.stable_id != 0 {
                    by_id.insert(node.stable_id, entry.clone());
                }
                by_ref.insert(node.node_ref.clone(), entry);
            }
            let (changes, changed) = if first {
                (Value::Null, true)
            } else if new_page {
                (json!({"newPage": true}), true)
            } else {
                let mut gone: Vec<&Seen> = snap
                    .by_id
                    .iter()
                    .filter(|(id, _)| !by_id.contains_key(*id))
                    .map(|(_, before)| before)
                    .collect();
                gone.sort_by(|a, b| (&a.role, &a.name).cmp(&(&b.role, &b.name)));
                let n_removed = gone.len();
                let removed: Vec<Value> = gone
                    .iter()
                    .take(CHANGES_SHOWN)
                    .map(|s| json!({"role": s.role, "name": s.name}))
                    .collect();
                let any = n_added + n_removed + n_changed > 0 || seen.epoch != snap.epoch;
                (
                    json!({
                        "added": added, "removed": removed, "changed": changed_list,
                        "addedCount": n_added, "removedCount": n_removed, "changedCount": n_changed,
                    }),
                    any,
                )
            };
            snap.epoch = seen.epoch;
            snap.url = seen.url.clone();
            snap.by_ref = by_ref;
            snap.by_id = by_id;
            Ok(Reading {
                epoch: seen.epoch,
                url: seen.url,
                title: seen.title,
                nodes,
                total,
                changes,
                changed,
            })
        }

        /// The control a ref names in a fresh reading of the page: `stale_ref` when the ref is not
        /// of this reading's epoch or the page no longer lists it. Returns the control and the
        /// reading's epoch and address.
        fn check_ref(&self, node: &str) -> Result<(Seen, u64, String), PhoneError> {
            let reading = self.read_page()?;
            let ref_epoch = node
                .trim_start_matches('r')
                .split('.')
                .next()
                .and_then(|e| e.parse::<u64>().ok());
            let snap = self.snap.lock().unwrap_or_else(|e| e.into_inner());
            match snap.by_ref.get(node) {
                Some(seen) if ref_epoch == Some(reading.epoch) => {
                    Ok((seen.clone(), reading.epoch, reading.url))
                }
                _ => Err(PhoneError::new(
                    "stale_ref",
                    "the page changed since that ref was listed, or never listed it; nothing was done, call browser_look again",
                )),
            }
        }

        /// What the page's current reading says `node` is.
        pub fn node(&self, node: &str) -> Result<NodeInfo, PhoneError> {
            let (seen, _, url) = self.check_ref(node)?;
            Ok(NodeInfo {
                role: seen.role,
                name: seen.name,
                sensitive: seen.sensitive,
                url,
            })
        }

        /// One action on the page by ref, then a fresh reading: the answer is the change. The ref
        /// is checked against the page as it is now first (`check_ref`), so an old epoch is
        /// refused before anything happens. A click on a link or a button that shows no change is
        /// `no_effect`, never done; other actions change state the reading does not show, so they
        /// are `unmeasured` when nothing showed.
        pub fn act(&self, act: &BrowserAct) -> Result<Value, PhoneError> {
            let (role, epoch_before) = match act.node() {
                Some(node) => {
                    let (seen, epoch, _) = self.check_ref(node)?;
                    (Some(seen.role), epoch)
                }
                None => (None, self.read_page()?.epoch),
            };
            let page = self.page.clone();
            let work = act.clone();
            self.run(60, async move {
                match work {
                    BrowserAct::Click { node } => page.click(&Target::Ref(node)).await,
                    BrowserAct::Hover { node } => page.hover(&Target::Ref(node)).await,
                    BrowserAct::Fill { node, text, clear } => {
                        page.fill(&Target::Ref(node), &text, clear).await
                    }
                    BrowserAct::Select { node, value } => {
                        page.select_option(&Target::Ref(node), &value).await
                    }
                    BrowserAct::Press { key } => page.press(&key).await,
                    BrowserAct::Type { text } => page.type_text(&text).await,
                    BrowserAct::Scroll { node, dy } => {
                        let at = node.map(Target::Ref);
                        page.wheel(at.as_ref(), 0.0, dy).await
                    }
                }
            })?
            .map_err(map_error)?;
            std::thread::sleep(Duration::from_millis(SETTLE_MS));
            let after = self.read_page()?;
            let measurable = matches!(act, BrowserAct::Click { .. })
                && matches!(role.as_deref(), Some("link" | "button" | "menuitem"));
            let verdict = if after.changed {
                "effect"
            } else if measurable {
                "no_effect"
            } else {
                "unmeasured"
            };
            let no_effect = verdict == "no_effect";
            let stale = after.epoch != epoch_before;
            let mut answer = json!({
                "untrusted": true, "source": "web page",
                "done": !no_effect,
                "result": if no_effect { "no_effect" } else { "done" },
                "verdict": verdict,
                "verified": verdict == "effect",
                "epoch": after.epoch, "url": after.url, "title": after.title,
                "staleRefs": stale,
                "changes": after.changes,
            });
            if no_effect {
                answer["note"] = json!(
                    "the page showed no change after the click; look again before repeating it"
                );
            } else if stale {
                answer["note"] = json!(
                    "the page changed: refs listed before this call are stale; use the refs in changes, or call browser_look"
                );
            }
            Ok(answer)
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

        /// Types `value` into the element `target_ref` (a ref from `observe`) through the crate's
        /// `fill_secret`, which types only when the page's own origin equals `origin` and the
        /// element is a password or card field. Nothing here reads the value back.
        pub fn fill_secret(
            &self,
            target_ref: &str,
            value: &str,
            origin: &str,
            clear: bool,
        ) -> Result<(), SecretFillError> {
            let page = self.page.clone();
            let target = Target::Ref(target_ref.to_string());
            let secret = rightkit_crypto::Secret::new(value.as_bytes().to_vec());
            let origin = origin.to_string();
            let outcome = self
                .run(30, async move {
                    page.fill_secret(&target, &secret, &origin, clear).await
                })
                .map_err(SecretFillError::Other)?;
            outcome.map_err(|error| match error {
                // The crate says which refusal in its text only: the origin check names "origin",
                // the field check "field".
                BrowserError::Denied(why) if why.contains("origin") => {
                    SecretFillError::OriginMismatch
                }
                BrowserError::Denied(_) => SecretFillError::NotCredentialField,
                refused @ (BrowserError::StaleRef(_)
                | BrowserError::NoElement(_)
                | BrowserError::Ambiguous(..)
                | BrowserError::NotInteractable(_)
                | BrowserError::Invalid(_)) => SecretFillError::Untyped(map_error(refused)),
                other => SecretFillError::Other(map_error(other)),
            })
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

    /// A control's name as the agent reads it: one line, obvious secrets masked, cut. A
    /// password-like field's name is already masked by the crate and is left as it is.
    fn clean_name(name: &str, sensitive: bool) -> String {
        if sensitive {
            return name.to_string();
        }
        let flat: String = name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let (masked, _) = rightkit_remote_gateway::redact::redact(flat.trim());
        masked.chars().take(NAME_MAX).collect()
    }

    /// A hash of a capture's bytes, to tell that the page changed.
    fn fingerprint(bytes: &[u8]) -> String {
        rightkit_crypto::hex_encode(&rightkit_crypto::sha256(bytes))
    }

    async fn capture(page: &BrowserPage, quality: i64, scale: f64) -> Result<Vec<u8>, String> {
        let scaled = scale < 0.999;
        page.screenshot_with(ScreenshotOptions {
            format: ScreenshotFormat::Jpeg {
                quality: quality as u8,
            },
            clip: scaled.then_some(ScreenshotClip {
                x: 0.0,
                y: 0.0,
                width: VIEWPORT.0 as f64,
                height: VIEWPORT.1 as f64,
            }),
            scale: scaled.then_some(scale),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("screenshot: {e}"))
    }

    /// Whether a browser can be found, without starting one.
    pub fn availability() -> Result<(), PhoneError> {
        if unsafe { libc::geteuid() } == 0 {
            return Err(unavailable(format!(
                "this computer runs {} as root; the agent browser needs an ordinary account",
                crate::product_name()
            )));
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
impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn observe(&self, share: bool, look: Look) -> Result<Value, PhoneError> {
        self.current()?.observe(share, look)
    }

    pub fn act(&self, act: &BrowserAct) -> Result<Value, PhoneError> {
        self.current()?.act(act)
    }

    pub fn node(&self, node: &str) -> Result<NodeInfo, PhoneError> {
        self.current()?.node(node)
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

    pub fn fill_secret(
        &self,
        target_ref: &str,
        value: &str,
        origin: &str,
        clear: bool,
    ) -> Result<(), SecretFillError> {
        self.current()
            .map_err(SecretFillError::Other)?
            .fill_secret(target_ref, value, origin, clear)
    }

    pub fn frame_scale(&self) -> f64 {
        self.current().map(|l| l.frame_scale()).unwrap_or(1.0)
    }
}

// ---- other platforms --------------------------------------------------------------------------

#[cfg(not(all(feature = "browser", any(target_os = "linux", target_os = "macos"))))]
pub struct Driver;

#[cfg(not(all(feature = "browser", any(target_os = "linux", target_os = "macos"))))]
impl Default for Driver {
    fn default() -> Driver {
        Driver
    }
}

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

    pub fn observe(&self, _share: bool, _look: Look) -> Result<Value, PhoneError> {
        self.no()
    }

    pub fn act(&self, _act: &BrowserAct) -> Result<Value, PhoneError> {
        self.no()
    }

    pub fn node(&self, _node: &str) -> Result<NodeInfo, PhoneError> {
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

    pub fn fill_secret(
        &self,
        _target_ref: &str,
        _value: &str,
        _origin: &str,
        _clear: bool,
    ) -> Result<(), SecretFillError> {
        self.no().map_err(SecretFillError::Other)
    }

    pub fn frame_scale(&self) -> f64 {
        1.0
    }
}
