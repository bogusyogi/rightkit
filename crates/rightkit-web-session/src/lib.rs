//! Agent browser logins.
//!
//! One computer owns a login store: one named browser profile, `<state>/browser/profiles/logins`
//! (mode 0700). An agent on that computer asks for named sites (`pulse browser request`); the
//! owner approves each use with a signed approval from a paired phone (Face ID) or a paired
//! computer (the owner check); only then does Chromium start, through `rightkit-browser`, with a
//! network policy that lets documents reach the approved hosts only. A session ends at the
//! owner-set limit (`agentBrowserSessionMinutes`), after 15 minutes idle or when the agent ends
//! it; Chromium exits then and the profile stays.
//!
//! This module is the bookkeeping and the rules: requests, the single live session, who may
//! look at it, and the timers. The browser itself is `driver`. Text typed in the owner's live
//! view and page content never reach a log, the journal or an error message; only ids, hosts,
//! the chat label and the device do. The wire methods (`browser.pending` ... `browser.done`) are
//! served by `devicelink::phone::Phone::browser`; the local agent commands by the engine.
//!
//! Page content for the agent (`observe`'s text and screenshot) leaves this computer only when the
//! owner's signed approval granted it for that one session (`share_content`, BD14); screenshots
//! are never written to disk (BD3). Downloads, popups and other tabs are denied (BD17).
//!
//! Written, not compiled, not run (added 2026-10-23, #460).

mod driver;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use rightkit_owner_approvals::PhoneError;
use rightkit_owner_approvals::admin_sig::{self, Nonces};
use rightkit_remote_gateway::settings::Settings;

pub use driver::{Frame, supported};
use std::collections::HashMap;

/// A request waits this long for the owner.
pub const REQUEST_TTL_MS: u64 = 10 * 60 * 1000;
/// A session with no agent or owner action for this long ends.
pub const IDLE_MS: u64 = 15 * 60 * 1000;
/// The agent's `login` waits this long for the owner to tap Done.
pub const LOGIN_WAIT_MS: u64 = 15 * 60 * 1000;
/// Most approved sites in one request.
pub const MAX_SITES: usize = 8;
/// Most requests waiting at once.
const MAX_PENDING: usize = 8;
const REASON_MAX: usize = 500;
const CHAT_MAX: usize = 120;
/// Most events in one `browser.input`.
const INPUT_EVENTS_MAX: usize = 32;
/// Longest text one `text` input or agent `type` carries, in characters.
const TEXT_MAX: usize = 1000;
/// How long a decided request stays answerable for a waiting `pulse browser request`.
const OUTCOME_KEEP_MS: u64 = 15 * 60 * 1000;
/// How often the session watcher looks.
/// The sentence the approval card shows next to the page-content choice (BD14): what is sent and
/// to whom.
pub const SHARE_NOTE: &str = "If you allow page content, the visible text (up to 20,000 characters, obvious secrets masked) and a screenshot of the approved sites' pages go to this agent's model provider (the company behind the chat that asked: Anthropic for Claude, OpenAI for Codex). A screenshot cannot be masked. Without it the agent gets only the address and title.";
/// The device id the owner check on this same computer approves as (BD5): a Mac that holds the
/// login store has no pairing with itself.
pub const LOCAL_APPROVER: &str = "local";
const WATCH_EVERY: Duration = Duration::from_secs(2);
/// The system file that switches the feature on a headless Linux computer (no hub or owner app there
/// to hold the owner-only settings): root-owned, written with one sudo, like the admin keys.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn linux_settings_file() -> &'static str {
    LINUX_SETTINGS_FILE
        .get_or_init(|| "/etc/rightkit/browser.json".to_string())
        .as_str()
}

#[cfg(all(unix, not(target_os = "macos")))]
static LINUX_SETTINGS_FILE: OnceLock<String> = OnceLock::new();

/// Sets the file that switches the feature on a headless Linux computer. Only the first call takes
/// effect. The default is `/etc/rightkit/browser.json`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn set_linux_settings_file(path: &str) -> bool {
    LINUX_SETTINGS_FILE.set(path.to_string()).is_ok()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_id(prefix: &str) -> String {
    let mut raw = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw).ok();
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{hex}")
}

/// One event for the journal (`browser.request`, `browser.approve`, `browser.deny`,
/// `browser.end`): ids, hosts, the chat label and the device; never page content or typed text.
pub type Recorder = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// What a waiting `pulse browser request` is told.
#[derive(Clone, Debug)]
enum Outcome {
    Approved { session: String },
    Denied,
    Expired,
    Failed { code: String, message: String },
}

#[derive(Clone, Debug)]
struct Req {
    id: String,
    sites: Vec<String>,
    reason: String,
    chat: String,
    device: String,
    created_ms: u64,
    expires_ms: u64,
    /// The session length this request was shown with, in minutes (signed in a v2 approval).
    minutes: u64,
}

impl Req {
    fn record(&self) -> Value {
        json!({
            "id": self.id, "sites": self.sites, "reason": self.reason, "chat": self.chat,
            "device": self.device, "created_ms": self.created_ms, "expires_ms": self.expires_ms,
            "minutes": self.minutes, "share_note": SHARE_NOTE,
        })
    }

    fn bind(&self, share_content: bool) -> admin_sig::BrowserBind<'_> {
        admin_sig::BrowserBind {
            reason: &self.reason,
            device: &self.device,
            minutes: self.minutes,
            share_content,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SState {
    Starting,
    Active,
    NeedsOwner,
}

impl SState {
    fn word(self) -> &'static str {
        match self {
            SState::Starting => "starting",
            SState::Active => "active",
            SState::NeedsOwner => "needs-owner",
        }
    }
}

struct Sess {
    id: String,
    sites: Vec<String>,
    chat: String,
    /// The paired device whose key approved it: the only one that may see the live view.
    approver: String,
    /// The owner allowed page text and screenshots to reach the agent for this session (BD14).
    share_content: bool,
    state: SState,
    deadline_ms: u64,
    last_ms: u64,
    owner_since_ms: u64,
    /// `login` ended: the owner tapped Done (`true`) or the wait ran out (`false`).
    login_result: Option<bool>,
}

#[derive(Default)]
struct State {
    requests: Vec<Req>,
    session: Option<Sess>,
    outcomes: HashMap<String, (u64, Outcome)>,
    /// Requests an approver is starting a session for (between the signature check and Chromium).
    approving: Vec<String>,
    /// Extra tabs closed in the live session so far; journaled once per session (BD17).
    reaped: bool,
}

/// Where the feature stands on this computer.
#[derive(Clone, Copy, Debug)]
pub struct Effective {
    pub on: bool,
    pub minutes: u64,
}

pub struct Browser {
    state: Mutex<State>,
    cv: Condvar,
    root: Mutex<Option<PathBuf>>,
    recorder: Mutex<Option<Recorder>>,
    /// The settings as last read, kept five seconds (a live view asks four times a second).
    cached: Mutex<Option<(std::time::Instant, Effective)>>,
    /// Challenges for an approval by the owner check on this same computer (BD5).
    local_nonces: OnceLock<Nonces>,
    driver: driver::Driver,
}

/// The one browser manager of this process.
pub fn global() -> &'static Browser {
    static BROWSER: OnceLock<Browser> = OnceLock::new();
    BROWSER.get_or_init(|| Browser {
        state: Mutex::new(State::default()),
        cv: Condvar::new(),
        root: Mutex::new(None),
        recorder: Mutex::new(None),
        cached: Mutex::new(None),
        local_nonces: OnceLock::new(),
        driver: driver::Driver::new(),
    })
}

fn refuse(code: &str, message: &str) -> PhoneError {
    PhoneError::new(code, message)
}

/// The host app's product name, from the app host (`RightKit` when none is installed).
pub(crate) fn product_name() -> String {
    rightkit_app_host::host().product_name().to_string()
}

/// The name of a thread this crate starts: `<product, lower case>-<what>`.
pub(crate) fn thread_name(what: &str) -> String {
    format!(
        "{}-{what}",
        product_name()
            .to_lowercase()
            .replace(char::is_whitespace, "-")
    )
}

/// A host name an agent may name: lower case letters, digits, hyphens and dots, at least one dot,
/// a last label that is not a number (no IP addresses), at most 253 characters.
pub fn clean_site(raw: &str) -> Option<String> {
    let site = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if site.len() < 3 || site.len() > 253 || !site.contains('.') {
        return None;
    }
    let labels: Vec<&str> = site.split('.').collect();
    let ok = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    let last_has_letter = labels
        .last()
        .is_some_and(|l| l.bytes().any(|b| b.is_ascii_alphabetic()));
    (ok && last_has_letter).then_some(site)
}

/// One line of text from an agent: control characters become spaces, at most `max` characters.
fn plain_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().chars().take(max).collect()
}

/// True when `host` is one of the approved sites or a subdomain of one.
pub fn host_allowed(host: &str, sites: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !host.is_empty()
        && sites
            .iter()
            .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// The host of an `http` or `https` URL, lower case; `None` for anything else.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    // A port is dropped; an IPv6 literal is not a site.
    if authority.starts_with('[') {
        return None;
    }
    let host = authority.split(':').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

impl Browser {
    // ---- configuration --------------------------------------------------------------------

    /// Where the login store, the shots and the nonces live: `<state folder>/browser`.
    fn root(&self) -> Result<PathBuf, PhoneError> {
        if let Some(root) = lock(&self.root).clone() {
            return Ok(root);
        }
        let root = rightkit_app_host::state_directory()
            .map_err(|e| PhoneError::retry("state_unavailable", e.to_string()))?
            .join("browser");
        Ok(root)
    }

    /// Names the folder (tests and hosts that keep state elsewhere).
    pub fn set_root(&self, root: PathBuf) {
        *lock(&self.root) = Some(root);
    }

    /// Where journal events go (the engine's task journal). Without one nothing is recorded.
    pub fn set_recorder(&self, recorder: Recorder) {
        *lock(&self.recorder) = Some(recorder);
    }

    fn record(&self, kind: &str, payload: Value) {
        let recorder = lock(&self.recorder).clone();
        if let Some(recorder) = recorder {
            recorder(kind, payload);
        }
    }

    /// The two settings. Settings come from `Settings::load` (the engine's own protected copy on
    /// a Mac or Windows). A headless Linux computer has no hub or owner app to change owner-only
    /// settings, so a root-owned `/etc/pulse/browser.json`
    /// (`{"agentBrowser": true, "agentBrowserSessionMinutes": 60}`) decides when it exists.
    pub fn effective(&self) -> Effective {
        if let Some((at, effective)) = *lock(&self.cached)
            && at.elapsed() < Duration::from_secs(5)
        {
            return effective;
        }
        let effective = self.read_effective();
        *lock(&self.cached) = Some((std::time::Instant::now(), effective));
        effective
    }

    fn read_effective(&self) -> Effective {
        let settings = Settings::load();
        // Mutated only where the root-owned /etc/pulse/browser.json applies (headless Linux).
        #[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(unused_mut))]
        let mut now = Effective {
            on: settings.agent_browser,
            minutes: settings.agent_browser_minutes,
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(file) = linux_file() {
            now.on = file["agentBrowser"].as_bool() == Some(true);
            if let Some(minutes) = file["agentBrowserSessionMinutes"].as_u64() {
                now.minutes = minutes.clamp(
                    1,
                    rightkit_remote_gateway::settings::HARD_AGENT_BROWSER_MINUTES,
                );
            }
        }
        now
    }

    pub fn enabled(&self) -> bool {
        supported() && self.effective().on
    }

    /// The switch and the platform, as a refusal: `feature_off` or `unsupported`.
    pub fn gate(&self) -> Result<Effective, PhoneError> {
        if !supported() {
            return Err(refuse(
                "unsupported",
                "agent browser logins run on a Mac or a Linux computer",
            ));
        }
        let effective = self.effective();
        if !effective.on {
            return Err(refuse(
                "feature_off",
                &format!(
                    "agent browser logins are off on this computer ({} Settings, agentBrowser)",
                    product_name()
                ),
            ));
        }
        Ok(effective)
    }

    /// `browser.status` for the local CLI and the owner's surfaces: what is set and whether a
    /// browser can start. Never lists a login or a page.
    pub fn status(&self) -> Value {
        let effective = self.effective();
        let live = {
            let mut st = lock(&self.state);
            self.sweep(&mut st, now_ms());
            st.session.as_ref().map(|s| {
                // The session id is the only credential for open, observe, act, login and end, and the
                // caller's label is not authenticated: never list it here (it goes to the requesting
                // chat alone, as the answer to `browser.request`). BD1, #576.
                json!({
                    "sites": s.sites, "chat": s.chat, "state": s.state.word(),
                    "pageContent": s.share_content,
                })
            })
        };
        json!({
            "supported": supported(),
            "enabled": supported() && effective.on,
            "sessionMinutes": effective.minutes,
            "browser": self.driver.availability().err().map(|e| e.message),
            "session": live,
        })
    }

    // ---- the agent's side -----------------------------------------------------------------

    /// `pulse browser request`: queue one request for the owner. `device` is this computer's name.
    pub fn submit(
        &self,
        sites: &[String],
        reason: &str,
        chat: &str,
        device: &str,
    ) -> Result<Value, PhoneError> {
        let effective = self.gate()?;
        self.driver.availability()?;
        let mut clean: Vec<String> = Vec::new();
        for site in sites {
            let Some(site) = clean_site(site) else {
                return Err(refuse(
                    "invalid_request",
                    "each --site is a host name such as ads.google.com (no address, port or path)",
                ));
            };
            if !clean.contains(&site) {
                clean.push(site);
            }
        }
        if clean.is_empty() || clean.len() > MAX_SITES {
            return Err(refuse(
                "invalid_request",
                "name between one and eight sites with --site",
            ));
        }
        let reason = plain_line(reason, REASON_MAX);
        if reason.is_empty() {
            return Err(refuse("invalid_request", "say why with --reason"));
        }
        let chat = plain_line(chat, CHAT_MAX);
        let now = now_ms();
        let mut st = lock(&self.state);
        self.sweep(&mut st, now);
        if st.session.is_some() || !st.approving.is_empty() {
            return Err(refuse(
                "busy",
                "a browser session is already open on this computer; end it first",
            ));
        }
        if st.requests.len() >= MAX_PENDING {
            return Err(refuse(
                "busy",
                "too many browser requests are waiting for the owner",
            ));
        }
        let req = Req {
            id: random_id("br_"),
            sites: clean,
            reason,
            chat,
            device: plain_line(device, CHAT_MAX),
            created_ms: now,
            expires_ms: now + REQUEST_TTL_MS,
            minutes: effective.minutes,
        };
        let record = req.record();
        self.record(
            "browser.request",
            json!({ "request": req.id, "sites": req.sites, "chat": req.chat, "device": req.device }),
        );
        st.requests.push(req);
        Ok(record)
    }

    /// `pulse browser request`, waiting: `{"state":"waiting"}` until the owner decides or
    /// `wait` has passed; `{"state":"approved","session"}`; a refusal otherwise.
    pub fn wait_request(&self, id: &str, wait: Duration) -> Result<Value, PhoneError> {
        let until = std::time::Instant::now() + wait;
        let mut st = lock(&self.state);
        loop {
            let now = now_ms();
            self.sweep(&mut st, now);
            if let Some((_, outcome)) = st.outcomes.get(id) {
                return match outcome.clone() {
                    Outcome::Approved { session } => {
                        Ok(json!({ "state": "approved", "session": session }))
                    }
                    Outcome::Denied => Err(refuse("denied", "the owner declined this request")),
                    Outcome::Expired => Err(refuse(
                        "expired",
                        "nobody approved this request within ten minutes",
                    )),
                    Outcome::Failed { code, message } => Err(PhoneError::new(&code, message)),
                };
            }
            if !st.requests.iter().any(|r| r.id == id) && !st.approving.iter().any(|a| a == id) {
                return Err(refuse("expired", "this request is gone"));
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(json!({ "state": "waiting" }));
            }
            let (guard, _) = self
                .cv
                .wait_timeout(st, left.min(Duration::from_secs(1)))
                .unwrap_or_else(|e| e.into_inner());
            st = guard;
        }
    }

    /// The agent gives up on a request that is still waiting.
    pub fn withdraw(&self, id: &str) -> Value {
        let mut st = lock(&self.state);
        let before = st.requests.len();
        st.requests.retain(|r| r.id != id);
        self.cv.notify_all();
        json!({ "withdrawn": st.requests.len() != before })
    }

    /// Runs `f` on the live session `id` for the agent: it must be the live one, not ended, and
    /// not held by the owner. Touches its idle clock. Returns the approved sites.
    fn agent_session(&self, id: &str, owner_ok: bool) -> Result<Vec<String>, PhoneError> {
        let now = now_ms();
        let mut st = lock(&self.state);
        self.sweep(&mut st, now);
        let Some(session) = st.session.as_mut().filter(|s| s.id == id) else {
            return Err(refuse(
                "not_approved",
                "there is no approved browser session with that id (it may have ended)",
            ));
        };
        match session.state {
            SState::Starting => return Err(refuse("busy", "the browser is still starting")),
            SState::NeedsOwner if !owner_ok => {
                return Err(refuse(
                    "busy",
                    "the owner has this session open for signing in; wait for Done",
                ));
            }
            _ => {}
        }
        session.last_ms = now;
        Ok(session.sites.clone())
    }

    /// `pulse browser open`.
    pub fn open(&self, id: &str, url: &str) -> Result<Value, PhoneError> {
        let sites = self.agent_session(id, false)?;
        check_url(url, &sites)?;
        self.driver.goto(url)?;
        Ok(self.driver.describe_json())
    }

    /// `pulse browser observe`: the address and title; with the owner's page-content grant for
    /// this session also the visible text (bounded, secrets masked) and a screenshot inside the
    /// answer. Nothing is written to disk, and the text is never logged here.
    pub fn observe(&self, id: &str) -> Result<Value, PhoneError> {
        self.agent_session(id, false)?;
        let share = lock(&self.state)
            .session
            .as_ref()
            .is_some_and(|s| s.id == id && s.share_content);
        self.driver.observe(share)
    }

    /// `pulse browser act`: `{"type":"click","x","y"}`, `{"type":"type","text"}`,
    /// `{"type":"key","key"}` or `{"type":"scroll","dy"}`, in page pixels.
    pub fn act(&self, id: &str, action: &Value) -> Result<Value, PhoneError> {
        self.agent_session(id, false)?;
        let input = parse_input(action)?;
        self.driver.input(&[input], 1.0)?;
        Ok(json!({}))
    }

    /// `pulse browser login`, start: optionally navigate to `url`, then hand the session to the
    /// owner (state `needs-owner`).
    pub fn login_start(&self, id: &str, url: Option<&str>) -> Result<Value, PhoneError> {
        let sites = self.agent_session(id, false)?;
        if let Some(url) = url {
            check_url(url, &sites)?;
            self.driver.goto(url)?;
        }
        let now = now_ms();
        let mut st = lock(&self.state);
        let Some(session) = st.session.as_mut().filter(|s| s.id == id) else {
            return Err(refuse("not_approved", "that session ended"));
        };
        session.state = SState::NeedsOwner;
        session.owner_since_ms = now;
        session.last_ms = now;
        session.login_result = None;
        self.cv.notify_all();
        Ok(json!({ "state": "needs-owner" }))
    }

    /// `pulse browser login`, waiting: `{"state":"waiting"}`, `{"state":"active"}` once the owner
    /// tapped Done, `owner_timeout` after 15 minutes.
    pub fn login_wait(&self, id: &str, wait: Duration) -> Result<Value, PhoneError> {
        let until = std::time::Instant::now() + wait;
        let mut st = lock(&self.state);
        loop {
            let now = now_ms();
            self.sweep(&mut st, now);
            let Some(session) = st.session.as_mut().filter(|s| s.id == id) else {
                return Err(refuse("not_approved", "that session ended"));
            };
            match session.login_result.take() {
                Some(true) => {
                    session.last_ms = now;
                    return Ok(json!({ "state": "active" }));
                }
                Some(false) => {
                    return Err(refuse(
                        "owner_timeout",
                        "the owner did not finish signing in within 15 minutes",
                    ));
                }
                None => {}
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(json!({ "state": "waiting" }));
            }
            let (guard, _) = self
                .cv
                .wait_timeout(st, left.min(Duration::from_secs(1)))
                .unwrap_or_else(|e| e.into_inner());
            st = guard;
        }
    }

    /// `pulse browser end`.
    pub fn end(&self, id: &str) -> Result<Value, PhoneError> {
        {
            let st = lock(&self.state);
            if !st.session.as_ref().is_some_and(|s| s.id == id) {
                return Err(refuse("not_approved", "there is no such session"));
            }
        }
        self.finish(id, "agent");
        Ok(json!({}))
    }

    // ---- the approver's side (the wire methods) -------------------------------------------

    /// `browser.pending`.
    pub fn pending(&self) -> Result<Value, PhoneError> {
        self.gate()?;
        let mut st = lock(&self.state);
        self.sweep(&mut st, now_ms());
        Ok(Value::Array(st.requests.iter().map(Req::record).collect()))
    }

    fn find_pending(&self, id: &str) -> Result<Req, PhoneError> {
        let mut st = lock(&self.state);
        self.sweep(&mut st, now_ms());
        st.requests
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| refuse("expired", "that request is gone or ran out"))
    }

    /// `browser.challenge {id}`: a single-use nonce for one pending request, valid 60 s.
    pub fn challenge(
        &self,
        nonces: &Nonces,
        device_id: &str,
        id: &str,
    ) -> Result<Value, PhoneError> {
        self.gate()?;
        self.find_pending(id)?;
        let (nonce, _) = nonces.issue(id, device_id, now_ms())?;
        Ok(json!({ "nonce": nonce }))
    }

    /// `browser.approve {id, nonce, key_id, sig, v?, share_content?}`: verify the owner's
    /// signature, then start the browser. `phone` says which signed text the signature covers
    /// (`admin_sig::check_browser`). With `"v": 2` the signature also binds the reason, the
    /// computer's name, the session length and the page-content grant (BD13, BD14).
    #[allow(clippy::too_many_arguments)]
    pub fn approve(
        &self,
        admin_root: &Path,
        nonces: &Nonces,
        device_id: &str,
        phone: bool,
        params: &Value,
    ) -> Result<Value, PhoneError> {
        self.approve_with(device_id, params, |req, id| {
            admin_sig::check_browser(
                admin_root,
                nonces,
                device_id,
                phone,
                id,
                &req.sites,
                &req.chat,
                &req.bind(false),
                params,
                now_ms(),
            )
        })
    }

    /// `browser.challenge` for the owner check on this same computer (BD5): a Mac that holds the
    /// login store has no pairing with itself, so its own owner app signs here. Returns the nonce,
    /// the request and what the owner check signs (always the v2 text, with `share_content` as
    /// the owner chose it).
    pub fn challenge_local(&self, id: &str, share_content: bool) -> Result<Value, PhoneError> {
        self.gate()?;
        let req = self.find_pending(id)?;
        // The signed text names the device of the enrolled key that will sign (`verify_with` uses
        // the key's own device), so the challenge says that one, and no key refuses before a
        // nonce is spent.
        let device = rightkit_owner_approvals::approval_sig::own_owner_check_key()
            .map(|(device, _, _)| device)
            .ok_or_else(|| {
                refuse(
                    "approver_unknown",
                    "this computer has no owner-check key enrolled",
                )
            })?;
        let (nonce, _) = self.local_nonces()?.issue(id, LOCAL_APPROVER, now_ms())?;
        let action = admin_sig::browser_action_v2(
            id,
            &req.sites,
            &req.chat,
            &req.bind(share_content),
            &nonce,
        );
        Ok(json!({
            "nonce": nonce,
            "request": req.record(),
            "sign": {
                "kind": "browser", "request": id, "action": action, "client": "",
                "device": device, "nonce": nonce,
            },
        }))
    }

    /// `browser.approve` by the owner check on this computer (BD5). The signature is checked
    /// against this computer's own enrolled owner-check key (Touch ID on the Mac); a computer with
    /// no owner-check key refuses. The session's approver is [`LOCAL_APPROVER`].
    pub fn approve_local(&self, params: &Value) -> Result<Value, PhoneError> {
        self.approve_with(LOCAL_APPROVER, params, |req, id| {
            let share = params.get("share_content").and_then(Value::as_bool) == Some(true);
            let nonce = params
                .get("nonce")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty() && n.len() <= 64)
                .ok_or_else(|| refuse("signature_required", "missing param `nonce`"))?
                .to_ascii_lowercase();
            let sig = params
                .get("sig")
                .filter(|s| s.is_object())
                .ok_or_else(|| refuse("signature_required", "missing param `sig`"))?;
            let bad = |r: rightkit_owner_approvals::Refusal| PhoneError::new(r.code, r.message);
            let signed =
                rightkit_owner_approvals::approval_sig::Signed::from_value(sig).map_err(bad)?;
            if !signed.nonce.eq_ignore_ascii_case(&nonce) {
                return Err(refuse(
                    "sig_invalid",
                    "the signature names another challenge",
                ));
            }
            // Spent before anything else happens: a challenge answers once.
            self.local_nonces()?
                .take(id, LOCAL_APPROVER, &nonce, now_ms())?;
            let at = now_ms() / 1000;
            if signed.expires <= at
                || signed.issued > at + 60
                || signed.expires < signed.issued
                || signed.expires - signed.issued > 120
            {
                return Err(refuse(
                    "approval_expired",
                    "the signed approval is expired or not yet valid; approve again",
                ));
            }
            let action =
                admin_sig::browser_action_v2(id, &req.sites, &req.chat, &req.bind(share), &nonce);
            // The key that signed must be this computer's own owner-check key, the device the
            // challenge named; any other enrolled key is refused.
            let own = rightkit_owner_approvals::approval_sig::own_owner_check_key()
                .map(|(device, _, _)| device);
            let signer =
                rightkit_owner_approvals::approval_sig::owner_check_key_device(&signed.key_id)
                    .map_err(bad)?;
            if own.is_none() || signer != own {
                return Err(refuse(
                    "approver_unknown",
                    "that key is not this computer's owner-check key",
                ));
            }
            rightkit_owner_approvals::approval_sig::verify_owner_check_kind(
                "browser", id, &action, "", &signed,
            )
            .map_err(bad)?;
            Ok((signed.key_id.clone(), share))
        })
    }

    fn local_nonces(&self) -> Result<&Nonces, PhoneError> {
        if let Some(nonces) = self.local_nonces.get() {
            return Ok(nonces);
        }
        let root = self.root()?;
        std::fs::create_dir_all(&root)
            .map_err(|e| PhoneError::retry("state_unwritable", e.to_string()))?;
        Ok(self
            .local_nonces
            .get_or_init(|| Nonces::new(root.join("local-nonces.json"))))
    }

    /// The shared approval path: find the request, hold the one approval slot, run `verify`
    /// (which returns the signing key's id and whether page content was granted), then start
    /// Chromium and open the session.
    fn approve_with(
        &self,
        approver: &str,
        params: &Value,
        verify: impl FnOnce(&Req, &str) -> Result<(String, bool), PhoneError>,
    ) -> Result<Value, PhoneError> {
        let effective = self.gate()?;
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .ok_or_else(|| refuse("invalid_request", "missing param `id`"))?;
        let req = self.find_pending(id)?;
        self.driver.availability()?;
        let root = self.root()?;
        // One live session per store, and one approval at a time.
        {
            let mut st = lock(&self.state);
            if st.session.is_some() || !st.approving.is_empty() {
                return Err(refuse(
                    "busy",
                    "a browser session is already open on this computer",
                ));
            }
            st.approving.push(id.to_string());
        }
        let release = |this: &Browser| {
            let mut st = lock(&this.state);
            st.approving.retain(|a| a != id);
            this.cv.notify_all();
        };
        let (key, share_content) = match verify(&req, id) {
            Ok(checked) => checked,
            Err(error) => {
                release(self);
                return Err(error);
            }
        };
        let session_id = random_id("bs_");
        let now = now_ms();
        // The length the owner was shown, never more than the limit now in force.
        let minutes = req.minutes.min(effective.minutes).max(1);
        {
            let mut st = lock(&self.state);
            st.requests.retain(|r| r.id != id);
            st.reaped = false;
            st.session = Some(Sess {
                id: session_id.clone(),
                sites: req.sites.clone(),
                chat: req.chat.clone(),
                approver: approver.to_string(),
                share_content,
                state: SState::Starting,
                deadline_ms: now + minutes * 60_000,
                last_ms: now,
                owner_since_ms: 0,
                login_result: None,
            });
            st.approving.retain(|a| a != id);
        }
        // Nothing from an older build's screenshots stays behind (BD3).
        purge_shots(&root);
        if let Err(error) = self.driver.start(&root, &req.sites) {
            let mut st = lock(&self.state);
            st.session = None;
            st.outcomes.insert(
                id.to_string(),
                (
                    now_ms(),
                    Outcome::Failed {
                        code: error.code.clone(),
                        message: error.message.clone(),
                    },
                ),
            );
            self.cv.notify_all();
            drop(st);
            // BD15: a refused or broken start leaves its trace. Journaled as a denial with the
            // reason `start_failed`; the code only, never the message text.
            self.record(
                "browser.deny",
                json!({
                    "request": id, "sites": req.sites, "chat": req.chat,
                    "device": approver, "reason": "start_failed", "code": error.code,
                }),
            );
            return Err(error);
        }
        {
            let mut st = lock(&self.state);
            if let Some(session) = st.session.as_mut().filter(|s| s.id == session_id) {
                session.state = SState::Active;
                session.last_ms = now_ms();
            }
            st.outcomes.insert(
                id.to_string(),
                (
                    now_ms(),
                    Outcome::Approved {
                        session: session_id.clone(),
                    },
                ),
            );
            self.cv.notify_all();
        }
        self.record(
            "browser.approve",
            json!({
                "request": id, "session": session_id, "sites": req.sites, "chat": req.chat,
                "device": approver, "key": key, "pageContent": share_content,
                "minutes": minutes,
            }),
        );
        global().watch(session_id.clone());
        Ok(json!({ "session": session_id }))
    }

    /// `browser.deny {id}`.
    pub fn deny(&self, device_id: &str, id: &str) -> Result<Value, PhoneError> {
        self.gate()?;
        let req = self.find_pending(id)?;
        {
            let mut st = lock(&self.state);
            st.requests.retain(|r| r.id != id);
            st.outcomes
                .insert(id.to_string(), (now_ms(), Outcome::Denied));
            self.cv.notify_all();
        }
        self.record(
            "browser.deny",
            json!({ "request": id, "sites": req.sites, "chat": req.chat, "device": device_id }),
        );
        Ok(json!({}))
    }

    /// `browser.sessions`: the live session, to the device whose key approved it.
    pub fn sessions(&self, device_id: &str) -> Result<Value, PhoneError> {
        self.gate()?;
        let found = {
            let mut st = lock(&self.state);
            self.sweep(&mut st, now_ms());
            st.session
                .as_ref()
                .filter(|s| s.approver == device_id)
                .map(|s| (s.id.clone(), s.sites.clone(), s.chat.clone(), s.state))
        };
        let mut out = Vec::new();
        if let Some((id, sites, chat, state)) = found {
            let (url, title) = if state == SState::Starting {
                (String::new(), String::new())
            } else {
                self.driver.describe()
            };
            out.push(json!({
                "session": id, "sites": sites, "chat": chat, "state": state.word(),
                "url": url, "title": title,
            }));
        }
        Ok(Value::Array(out))
    }

    /// A live-view call: the session must be the live one, `needs-owner`, and approved by this
    /// device. Counts as activity. Returns nothing else about the session.
    fn owner_view(&self, device_id: &str, session: &str) -> Result<(), PhoneError> {
        let now = now_ms();
        let mut st = lock(&self.state);
        self.sweep(&mut st, now);
        let Some(s) = st.session.as_mut().filter(|s| s.id == session) else {
            return Err(refuse("not_approved", "there is no such session"));
        };
        if s.approver != device_id {
            return Err(refuse(
                "forbidden",
                "only the device that approved this session may see it",
            ));
        }
        if s.state != SState::NeedsOwner {
            return Err(refuse(
                "not_needs_owner",
                "the live view is open only while the agent asks you to sign in",
            ));
        }
        s.last_ms = now;
        Ok(())
    }

    /// `browser.frame {session, after_seq}`.
    pub fn frame(
        &self,
        device_id: &str,
        session: &str,
        after_seq: u64,
    ) -> Result<Value, PhoneError> {
        self.gate()?;
        self.owner_view(device_id, session)?;
        let frame = self.driver.frame(after_seq)?;
        Ok(json!({
            "seq": frame.seq, "jpeg_b64": frame.jpeg_b64, "width": frame.width,
            "height": frame.height, "url": frame.url, "title": frame.title,
            "unchanged": frame.unchanged,
        }))
    }

    /// `browser.input {session, events}`: x and y are frame pixels. Typed text is applied and
    /// forgotten; it is not kept, logged or journaled.
    pub fn input(
        &self,
        device_id: &str,
        session: &str,
        events: &Value,
    ) -> Result<Value, PhoneError> {
        self.gate()?;
        self.owner_view(device_id, session)?;
        let list = events
            .as_array()
            .filter(|l| l.len() <= INPUT_EVENTS_MAX)
            .ok_or_else(|| refuse("invalid_request", "`events` is a list of at most 32 events"))?;
        let parsed: Result<Vec<Input>, PhoneError> = list.iter().map(parse_input).collect();
        self.driver.input(&parsed?, self.driver.frame_scale())?;
        Ok(json!({}))
    }

    /// `browser.done {session}`: back to the agent.
    pub fn done(&self, device_id: &str, session: &str) -> Result<Value, PhoneError> {
        self.gate()?;
        self.owner_view(device_id, session)?;
        let now = now_ms();
        let mut st = lock(&self.state);
        if let Some(s) = st.session.as_mut().filter(|s| s.id == session) {
            s.state = SState::Active;
            s.last_ms = now;
            s.login_result = Some(true);
        }
        self.cv.notify_all();
        Ok(json!({}))
    }

    // ---- ending ---------------------------------------------------------------------------

    /// Drops requests that ran out and decided outcomes nobody asked for.
    fn sweep(&self, st: &mut State, now: u64) {
        let (gone, kept): (Vec<Req>, Vec<Req>) = std::mem::take(&mut st.requests)
            .into_iter()
            .partition(|r| r.expires_ms <= now);
        st.requests = kept;
        for req in gone {
            st.outcomes.insert(req.id.clone(), (now, Outcome::Expired));
            self.record(
                "browser.deny",
                json!({
                    "request": req.id, "sites": req.sites, "chat": req.chat,
                    "device": req.device, "reason": "expired",
                }),
            );
            self.cv.notify_all();
        }
        st.outcomes
            .retain(|_, (at, _)| now.saturating_sub(*at) < OUTCOME_KEEP_MS);
    }

    /// Ends the live session `id` (if it is still the live one): stops Chromium, clears the
    /// slot, journals `browser.end` with the reason word.
    fn finish(&self, id: &str, reason: &str) {
        {
            let st = lock(&self.state);
            if !st.session.as_ref().is_some_and(|s| s.id == id) {
                return;
            }
        }
        // Chromium goes first, so a new approval cannot meet the old one's profile lock.
        self.driver.stop();
        if let Ok(root) = self.root() {
            purge_shots(&root);
        }
        let taken = {
            let mut st = lock(&self.state);
            if st.session.as_ref().is_some_and(|s| s.id == id) {
                st.session.take()
            } else {
                None
            }
        };
        let Some(session) = taken else {
            return;
        };
        self.record(
            "browser.end",
            json!({
                "session": session.id, "sites": session.sites, "chat": session.chat,
                "device": session.approver, "reason": reason,
            }),
        );
        self.cv.notify_all();
    }

    /// One thread per session: ends it at its limit, when idle, when Chromium is gone, and hands
    /// a signing-in session back to the agent after 15 minutes. Exits when the session does.
    fn watch(&'static self, id: String) {
        let watched = id.clone();
        let spawned = std::thread::Builder::new()
            .name(thread_name("browser-watch"))
            .spawn(move || {
                loop {
                    {
                        let st = lock(&self.state);
                        let _ = self
                            .cv
                            .wait_timeout(st, WATCH_EVERY)
                            .unwrap_or_else(|e| e.into_inner());
                    }
                    let now = now_ms();
                    let verdict = {
                        let mut st = lock(&self.state);
                        let Some(s) = st.session.as_mut().filter(|s| s.id == id) else {
                            return;
                        };
                        if now >= s.deadline_ms {
                            Some("limit")
                        } else if s.state == SState::NeedsOwner
                            && now.saturating_sub(s.owner_since_ms) >= LOGIN_WAIT_MS
                        {
                            s.state = SState::Active;
                            s.login_result = Some(false);
                            s.last_ms = now;
                            self.cv.notify_all();
                            None
                        } else if s.state != SState::NeedsOwner
                            && s.state != SState::Starting
                            && now.saturating_sub(s.last_ms) >= IDLE_MS
                        {
                            Some("idle")
                        } else {
                            None
                        }
                    };
                    if let Some(reason) = verdict {
                        self.finish(&id, reason);
                        return;
                    }
                    if !self.driver.alive() {
                        self.finish(&id, "browser_closed");
                        return;
                    }
                    // BD17: a popup or new tab is closed; the first one in a session is journaled.
                    if self.driver.reap_extra() > 0 {
                        let first = {
                            let mut st = lock(&self.state);
                            let first = !st.reaped;
                            st.reaped = true;
                            first
                        };
                        if first {
                            let info = {
                                let st = lock(&self.state);
                                st.session
                                    .as_ref()
                                    .filter(|s| s.id == id)
                                    .map(|s| (s.sites.clone(), s.chat.clone(), s.approver.clone()))
                            };
                            if let Some((sites, chat, device)) = info {
                                self.record(
                                    "browser.deny",
                                    json!({
                                        "session": id, "sites": sites, "chat": chat,
                                        "device": device, "reason": "new_tab_closed",
                                    }),
                                );
                            }
                        }
                    }
                }
            });
        if spawned.is_err() {
            // Without its watcher a session could outlive its limit: end it now.
            self.finish(&watched, "no_watcher");
        }
    }
}

/// Removes the screenshot folder an older build wrote (BD3). Screenshots are never written now;
/// this keeps a computer that ran the earlier code clean when a session starts and ends.
fn purge_shots(root: &Path) {
    let _ = std::fs::remove_dir_all(root.join("shots"));
}

// ---- input ------------------------------------------------------------------------------------

/// One pointer or key action, from the agent (page pixels) or the owner's live view (frame pixels).
#[derive(Clone, Debug)]
pub enum Input {
    Click { x: f64, y: f64 },
    Text(String),
    Key(String),
    Scroll { dy: f64 },
}

fn parse_input(event: &Value) -> Result<Input, PhoneError> {
    let bad = |what: &str| refuse("invalid_request", what);
    let number = |key: &str, max: f64| -> Result<f64, PhoneError> {
        event
            .get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && n.abs() <= max)
            .ok_or_else(|| bad("x, y and dy are plain numbers"))
    };
    match event.get("type").and_then(Value::as_str) {
        Some("click") => Ok(Input::Click {
            x: number("x", 20_000.0)?,
            y: number("y", 20_000.0)?,
        }),
        Some("text") => {
            let text = event
                .get("text")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty() && t.chars().count() <= TEXT_MAX)
                .ok_or_else(|| bad("text is 1 to 1000 characters"))?;
            Ok(Input::Text(text.to_string()))
        }
        Some("key") => {
            let key = event
                .get("key")
                .and_then(Value::as_str)
                .filter(|k| !k.is_empty() && k.len() <= 40 && !k.chars().any(char::is_control))
                .ok_or_else(|| bad("key is a key name such as Enter or Tab"))?;
            Ok(Input::Key(key.to_string()))
        }
        Some("scroll") => Ok(Input::Scroll {
            dy: number("dy", 10_000.0)?,
        }),
        _ => Err(bad("an event is click, text, key or scroll")),
    }
}

/// `open` and `login --url`: an http(s) address on an approved host.
fn check_url(url: &str, sites: &[String]) -> Result<(), PhoneError> {
    match url_host(url) {
        Some(host) if host_allowed(&host, sites) => Ok(()),
        _ => Err(refuse(
            "site_not_approved",
            "that address is not on a site the owner approved for this session",
        )),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_file() -> Option<Value> {
    let path = Path::new(linux_settings_file());
    if !rightkit_owner_approvals::admin_sig::admin_root_only(path) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}
