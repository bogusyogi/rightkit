//! Out-of-process menu bar helper (macOS) with an authenticated action channel.
//!
//! The status item lives in a nested helper app with its own bundle identifier rather than in
//! this process. macOS can permanently refuse to let Control Center adopt a given identifier's
//! NSStatusItem, and nothing in user space clears it. Keeping the item in a disposable identity
//! means recovering from that costs a helper rebuild instead of every user re-granting
//! Accessibility, Microphone and Keychain items.
//!
//! # Two directions, two trust levels
//!
//! * **State, app to helper** (tray icon state): a distributed notification on the configured
//!   state channel ([`publish_state`]). It carries no authority. Any process may post one; the
//!   worst outcome is a wrong icon until the next real state.
//! * **Actions, helper or Shortcuts extension to app** (start, stop, quit, open a window, any
//!   intent): a Unix-domain socket this process listens on. An action runs only after the
//!   operating system has identified the sender: the kernel's audit token of the connecting
//!   process is checked against its sender class's signing policy (the host's Team ID and the
//!   exact signing identifiers that class may carry, Developer ID Application). A distributed notification or a file in a user-writable
//!   folder can be forged by any process of the user, so neither carries an action.
//!
//! Unverified connections are closed before a byte is read and reported through the event sink
//! with a reason code and no content. Frames bind to a fresh connection nonce, advance a strict
//! sequence & reject duplicate ids within a bounded per-role history. State notifications carry
//! no command authority; host callbacks retain their own permission & application policy checks.
//!
//! The host supplies every product value ([`Config`], [`Sender`]) and every policy decision (the
//! action and intent callbacks). This module knows nothing about what an action means.

mod peer;
mod protocol;

pub use protocol::Sender;

use rightkit_daemon::{Accepted, Conn, Listener, PeerIdentity, PrivateSecret};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Product values of one menu bar helper.
#[derive(Clone)]
pub struct Config {
    /// Distributed-notification name this process posts state on (see [`publish_state`]).
    pub state_channel: String,
    /// Path of the helper `.app` relative to the host bundle root
    /// (`Contents/Library/LoginItems/<Name>.app`).
    pub helper_relative_path: String,
    /// Environment variable that carries this process's pid into the launched helper.
    pub parent_pid_env: String,
    /// File-name stem of the action socket: `<per-user temp dir>/<socket_name>.sock`. Senders
    /// compute the same path. 1..=48 bytes of ASCII letters, digits, `.`, `-`, `_`.
    pub socket_name: String,
    /// The Apple Developer Team ID every sender must be signed by (ten upper-case letters or
    /// digits).
    pub team_id: String,
    /// The supervised menu-bar helper: its allowed signing identifiers and the actions it may
    /// send. Any verified frame from it is proof of life for the supervisor.
    pub menu_sender: Sender,
    /// Seconds of silence after which the helper is presumed dead and relaunched.
    pub heartbeat_grace_secs: u64,
    /// Seconds between supervisor checks.
    pub supervisor_poll_secs: u64,
    /// Host event sink `(event, detail)`; the default drops events.
    pub event_sink: fn(&str, &str),
}

fn drop_event(_event: &str, _detail: &str) {}

impl Config {
    /// A config with the 9 s heartbeat grace, 4 s supervisor poll and no event sink.
    pub fn new(
        state_channel: impl Into<String>,
        helper_relative_path: impl Into<String>,
        parent_pid_env: impl Into<String>,
        socket_name: impl Into<String>,
        team_id: impl Into<String>,
        menu_sender: Sender,
    ) -> Self {
        Self {
            state_channel: state_channel.into(),
            helper_relative_path: helper_relative_path.into(),
            parent_pid_env: parent_pid_env.into(),
            socket_name: socket_name.into(),
            team_id: team_id.into(),
            menu_sender,
            heartbeat_grace_secs: 9,
            supervisor_poll_secs: 4,
            event_sink: drop_event,
        }
    }

    pub fn heartbeat(mut self, grace_secs: u64, poll_secs: u64) -> Self {
        self.heartbeat_grace_secs = grace_secs;
        self.supervisor_poll_secs = poll_secs;
        self
    }

    pub fn event_sink(mut self, sink: fn(&str, &str)) -> Self {
        self.event_sink = sink;
        self
    }
}

/// One verified App Intent command, as the Shortcuts extension sent it.
pub struct ShortcutIntent {
    pub id: String,
    pub action: String,
}

type ActionHandler = dyn Fn(&str) + Send + Sync;
type IntentHandler = dyn Fn(ShortcutIntent) + Send + Sync;

/// Most simultaneous authenticated connections served.
const MAX_CONNECTIONS: usize = 8;
/// Hard lifetime from the start of serving a verified connection, covering greeting, all reads
/// (including partial frames) & replies. Traffic never extends it. Persistent clients reconnect
/// after this ordinary EOF using the same action id if its acknowledgement was lost.
const CONNECTION_LIFETIME: Duration = Duration::from_secs(30);
/// Refusal events are logged at most once per this many seconds per process; the rest are counted.
const REFUSAL_LOG_INTERVAL_SECS: u64 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Menu,
    Intent,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Menu => "menu",
            Class::Intent => "intent",
        }
    }
}

struct Side {
    sender: Sender,
    policy: peer::RolePolicy,
}

struct IntentSide {
    side: Side,
    handler: Box<IntentHandler>,
}

struct Inner {
    config: Config,
    on_action: Box<ActionHandler>,
    menu: Side,
    intents: Option<IntentSide>,
    supervisor_started: AtomicBool,
    /// Unix seconds of the last verified frame from the helper (or its connection). The
    /// supervisor relaunches the helper if this goes stale, so a crashed or killed helper
    /// self-heals instead of leaving the menu bar empty until the whole app restarts. Only a
    /// verified sender can refresh it.
    last_helper_seen: AtomicU64,
    listening: AtomicBool,
    connections: AtomicUsize,
    /// Serializes intent callbacks so a rapid Siri/Shortcuts burst cannot race host state
    /// changes.
    intent_lock: Mutex<()>,
    menu_ids: Mutex<protocol::ReplayGuard>,
    intent_ids: Mutex<protocol::ReplayGuard>,
    refusal_last_logged: AtomicU64,
    refusal_suppressed: AtomicU64,
}

/// Release admission even when a connection worker unwinds, shutting down the socket first.
struct ConnectionSlot {
    inner: Arc<Inner>,
    shutdown: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        (self.shutdown)();
        self.inner.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Handle to one menu bar helper. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct MenubarHelper {
    inner: Arc<Inner>,
}

/// Builds a [`MenubarHelper`]. Intents are an opt-in with their own sender and callback, so a
/// host without a Shortcuts extension carries neither.
pub struct MenubarHelperBuilder {
    config: Config,
    on_action: Box<ActionHandler>,
    intents: Option<(Sender, Box<IntentHandler>)>,
}

impl MenubarHelperBuilder {
    /// Also accept intents from `sender` (a different signing identity than the menu helper,
    /// typically the Shortcuts extension). `on_intent` receives each verified intent, one at a
    /// time, off the main thread.
    pub fn accept_intents(
        mut self,
        sender: Sender,
        on_intent: impl Fn(ShortcutIntent) + Send + Sync + 'static,
    ) -> Self {
        self.intents = Some((sender, Box::new(on_intent)));
        self
    }

    /// Validate every host input and build. Fails on a malformed Team ID, signing identifier,
    /// action name or socket name, on a sender with no identifier or no action, and when the menu
    /// and intent senders share an identifier (a peer must belong to exactly one class).
    pub fn build(self) -> Result<MenubarHelper, String> {
        let Self {
            config,
            on_action,
            intents,
        } = self;
        if !protocol::valid_team_id(&config.team_id) {
            return Err("team_id must be ten upper-case letters or digits".to_string());
        }
        if !protocol::valid_socket_name(&config.socket_name) {
            return Err("socket_name is malformed".to_string());
        }
        config.menu_sender.validate("menu sender")?;
        let menu_policy =
            peer::RolePolicy::new(&config.team_id, &config.menu_sender.identifiers)
                .ok_or_else(|| "menu sender signing policy could not be built".to_string())?;
        let intents = match intents {
            None => None,
            Some((sender, handler)) => {
                sender.validate("intent sender")?;
                if sender
                    .identifiers
                    .iter()
                    .any(|id| config.menu_sender.identifiers.contains(id))
                {
                    return Err("menu and intent senders share a signing identifier".to_string());
                }
                let policy = peer::RolePolicy::new(&config.team_id, &sender.identifiers)
                    .ok_or_else(|| "intent sender signing policy could not be built".to_string())?;
                Some(IntentSide {
                    side: Side { sender, policy },
                    handler,
                })
            }
        };
        Ok(MenubarHelper {
            inner: Arc::new(Inner {
                menu: Side {
                    sender: config.menu_sender.clone(),
                    policy: menu_policy,
                },
                config,
                on_action,
                intents,
                supervisor_started: AtomicBool::new(false),
                last_helper_seen: AtomicU64::new(0),
                listening: AtomicBool::new(false),
                connections: AtomicUsize::new(0),
                intent_lock: Mutex::new(()),
                menu_ids: Mutex::new(protocol::ReplayGuard::default()),
                intent_ids: Mutex::new(protocol::ReplayGuard::default()),
                refusal_last_logged: AtomicU64::new(0),
                refusal_suppressed: AtomicU64::new(0),
            }),
        })
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MenubarHelper {
    /// Start building. `on_action(action)` receives each verified action of the menu helper
    /// (heartbeats included; proof of life is recorded before it runs), on a connection thread,
    /// and applies the host's policy.
    pub fn builder(
        config: Config,
        on_action: impl Fn(&str) + Send + Sync + 'static,
    ) -> MenubarHelperBuilder {
        MenubarHelperBuilder {
            config,
            on_action: Box::new(on_action),
            intents: None,
        }
    }

    fn log(&self, event: &str, detail: &str) {
        (self.inner.config.event_sink)(event, detail);
    }

    fn mark_helper_seen(&self) {
        self.inner
            .last_helper_seen
            .store(now_secs(), Ordering::SeqCst);
    }

    /// Absolute path to the nested helper inside the running app bundle. The resolver canonicalises
    /// and requires it to lie under the canonical bundle; a code-signature probe (detection only)
    /// reports through the event sink.
    fn helper_path(&self) -> Option<PathBuf> {
        match rightkit_child_process::resolve_bundled_app(&self.inner.config.helper_relative_path) {
            Ok(Some(path)) => {
                rightkit_child_process::probe_codesign(&path, self.inner.config.event_sink);
                Some(path)
            }
            Ok(None) => None,
            Err(reason) => {
                self.log(
                    "helper_path_rejected",
                    &format!(
                        "path={} reason={reason}",
                        self.inner.config.helper_relative_path
                    ),
                );
                None
            }
        }
    }

    /// Launch the helper through LaunchServices. Deliberately NOT spawned as a child process:
    /// Apple's guidance for this failure class is that a helper started as a bare child is not
    /// reliably mapped back to its bundle, which is the mapping Control Center needs to adopt the
    /// status item. (Authority therefore cannot ride on an inherited channel; it rides on the
    /// verified connection instead.)
    pub fn start(&self) -> Result<(), String> {
        let this = self.clone();
        std::thread::Builder::new()
            .name("menubar-helper-start".to_string())
            .spawn(move || {
                this.launch_once();
                if let Err(error) = this.start_supervisor() {
                    this.log("menubar_helper_supervisor_error", &error);
                }
            })
            .map(|_| ())
            .map_err(|error| format!("spawn menubar helper worker: {error}"))
    }

    /// `open -g` the helper. Safe to call repeatedly: the helper is a normal single-instance .app,
    /// so LaunchServices activates the existing process rather than spawning a duplicate icon when
    /// it is already alive.
    fn launch_once(&self) {
        let Some(path) = self.helper_path() else {
            self.log("menubar_helper", "not bundled — skipping");
            return;
        };
        let pid = std::process::id().to_string();
        // `background` (-g): never steal focus from the app being dictated into.
        match rightkit_child_process::launch_bundle(
            &path,
            true,
            &[(self.inner.config.parent_pid_env.as_str(), pid.as_str())],
        ) {
            Ok(()) => {
                self.mark_helper_seen();
                self.log("menubar_helper", "launched");
            }
            Err(err) => self.log("menubar_helper_error", &err.to_string()),
        }
    }

    /// Relaunch the helper if its heartbeat goes stale. The helper sends a heartbeat every few
    /// seconds over the verified channel; if none arrives within the grace window the process is
    /// presumed dead and re-opened.
    fn start_supervisor(&self) -> Result<(), String> {
        if self.inner.supervisor_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let this = self.clone();
        std::thread::Builder::new()
            .name("menubar-helper-supervisor".to_string())
            .spawn(move || {
                let grace_secs = this.inner.config.heartbeat_grace_secs;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(
                        this.inner.config.supervisor_poll_secs,
                    ));
                    let last = this.inner.last_helper_seen.load(Ordering::SeqCst);
                    if last == 0 {
                        continue; // never launched successfully yet; start() owns that
                    }
                    if now_secs().saturating_sub(last) > grace_secs {
                        this.log("menubar_helper_respawn", "heartbeat stale");
                        this.launch_once();
                    }
                }
            })
            .map(|_| ())
            .map_err(|error| {
                self.inner.supervisor_started.store(false, Ordering::SeqCst);
                format!("spawn menubar helper supervisor: {error}")
            })
    }

    /// Bind the action socket and serve verified senders for the life of the process. Call it
    /// before [`start`](Self::start) so the helper finds the socket, and after the host can
    /// handle callbacks. Returns an error when the socket cannot be bound (for example the
    /// per-user directory is not private, the path is too long, or a live listener already owns
    /// it); then no action can arrive, which fails closed.
    pub fn observe_actions(&self) -> Result<(), String> {
        if self.inner.listening.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let listener = match self.bind() {
            Ok(listener) => listener,
            Err(error) => {
                self.inner.listening.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let this = self.clone();
        std::thread::Builder::new()
            .name("menubar-action-listener".to_string())
            .spawn(move || this.accept_loop(listener))
            .map(|_| ())
            .map_err(|error| {
                self.inner.listening.store(false, Ordering::SeqCst);
                format!("spawn menubar action listener: {error}")
            })
    }

    fn bind(&self) -> Result<Listener, String> {
        let dir = peer::user_temp_dir()?;
        let path = dir.join(format!("{}.sock", self.inner.config.socket_name));
        let endpoint = path
            .to_str()
            .ok_or_else(|| "action socket path is not UTF-8".to_string())?;
        if endpoint.len() > rightkit_daemon::MAX_UNIX_SOCKET_PATH {
            return Err(format!(
                "action socket path is {} bytes, over the {} byte limit",
                endpoint.len(),
                rightkit_daemon::MAX_UNIX_SOCKET_PATH
            ));
        }
        // The listener replaces only a dead socket this user owns, creates it 0600 and accepts a
        // peer only after the kernel says it runs as this user. That is hygiene; the sender
        // identity check below is the authentication.
        Listener::bind(endpoint, rightkit_daemon::DEFAULT_MAX_PIPE_INSTANCES)
            .map_err(|error| format!("bind action socket: {error}"))
    }

    fn accept_loop(&self, listener: Listener) {
        let mut failures = 0u32;
        loop {
            match listener.accept() {
                Ok(Accepted::Conn(conn)) => {
                    failures = 0;
                    self.admit_connection(conn);
                }
                Ok(Accepted::Rejected(_)) | Ok(Accepted::Unidentified(_)) => {
                    failures = 0;
                    self.refuse("not_current_user", None);
                }
                Ok(Accepted::Vanished) => failures = 0,
                Err(error) => {
                    failures += 1;
                    self.log("menubar_action_listener_error", &format!("accept={error}"));
                    if failures >= 50 {
                        self.log("menubar_action_listener_stopped", "too many accept errors");
                        self.inner.listening.store(false, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }

    /// Identify the connecting process before reading a byte. A peer that is not provably one of
    /// the configured senders is closed unread.
    fn admit_connection(&self, conn: Conn) {
        let shutdown = conn.shutdown.clone();
        let Some(token) = conn.peer.audit_token else {
            self.refuse(peer::PeerCheck::NoToken.code(), None);
            (shutdown)();
            return;
        };
        let pid = Some(token[5]);
        let class = match self.classify(&conn.peer) {
            Ok(class) => class,
            Err(check) => {
                self.refuse(check.code(), pid);
                (shutdown)();
                return;
            }
        };
        if self.inner.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            self.inner.connections.fetch_sub(1, Ordering::SeqCst);
            self.refuse("too_many_connections", pid);
            (shutdown)();
            return;
        }
        let this = self.clone();
        let spawned = std::thread::Builder::new()
            .name("menubar-action-conn".to_string())
            .spawn(move || {
                let _slot = ConnectionSlot {
                    inner: this.inner.clone(),
                    shutdown: conn.shutdown.clone(),
                };
                this.serve(conn, class);
            });
        if spawned.is_err() {
            self.inner.connections.fetch_sub(1, Ordering::SeqCst);
            self.refuse("spawn_failed", pid);
            (shutdown)();
        }
    }

    /// Which sender class, if any, the process behind `peer` belongs to. The policies are disjoint
    /// (`build` refuses a shared identifier), so at most one can admit a peer.
    fn classify(&self, peer: &PeerIdentity) -> Result<Class, peer::PeerCheck> {
        let mut last = match self.inner.menu.policy.admits(peer) {
            Ok(()) => return Ok(Class::Menu),
            Err(check) => check,
        };
        if let Some(intents) = &self.inner.intents {
            match intents.side.policy.admits(peer) {
                Ok(()) => return Ok(Class::Intent),
                Err(check) => last = check,
            }
        }
        Err(last)
    }

    /// The sender definition of `class`. `None` for an intent class that is not configured, which
    /// cannot be classified; if it ever were, it has no actions (fail closed, never the menu set).
    fn sender_of(&self, class: Class) -> Option<&Sender> {
        match class {
            Class::Menu => Some(&self.inner.menu.sender),
            Class::Intent => self
                .inner
                .intents
                .as_ref()
                .map(|intents| &intents.side.sender),
        }
    }

    fn serve(&self, conn: Conn, class: Class) {
        // Conn exposes opaque Read/Write objects rather than a socket handle. Its shutdown
        // closure calls Shutdown::Both on the shared Unix socket, interrupting every blocking
        // clone. A separate deadline worker therefore bounds blocked reads AND writes without
        // weakening daemon custody or depending on incoming bytes to advance a timeout.
        let deadline = Instant::now() + CONNECTION_LIFETIME;
        let expired = Arc::new(AtomicBool::new(false));
        let deadline_expired = expired.clone();
        let deadline_shutdown = conn.shutdown.clone();
        let (_cancel_deadline, cancel) = mpsc::channel::<()>();
        if std::thread::Builder::new()
            .name("menubar-action-deadline".to_string())
            .spawn(move || {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if matches!(
                    cancel.recv_timeout(remaining),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    deadline_expired.store(true, Ordering::SeqCst);
                    (deadline_shutdown)();
                }
            })
            .is_err()
        {
            self.refuse("deadline_spawn_failed", None);
            return;
        }
        // Dropping _cancel_deadline on every return/unwind wakes & retires the deadline worker.
        // Reaching the lifetime is ordinary connection retirement, never an identity refusal.
        let Conn {
            reader,
            mut writer,
            shutdown,
            ..
        } = conn;
        let nonce = match PrivateSecret::generate() {
            Ok(secret) => secret.expose().to_string(),
            Err(_) => {
                self.refuse("nonce_unavailable", None);
                (shutdown)();
                return;
            }
        };
        let mut exchange = protocol::Exchange::new(nonce);
        if write_line(&mut writer, &exchange.greeting()).is_err() {
            (shutdown)();
            return;
        }
        if class == Class::Menu {
            self.mark_helper_seen();
        }
        let mut frames = protocol::FrameReader::new(reader);
        loop {
            if expired.load(Ordering::SeqCst) {
                break;
            }
            let line = match frames.next_frame() {
                Ok(Some(line)) => line,
                Ok(None) | Err(protocol::FrameError::Io) => break,
                Err(protocol::FrameError::TooLong) => {
                    self.log(
                        "menubar_frame_refused",
                        &format!("sender={} reason=frame_too_long", class.name()),
                    );
                    break;
                }
            };
            if expired.load(Ordering::SeqCst) {
                break;
            }
            match self.accept_frame(class, &mut exchange, &line) {
                Ok(request) => {
                    let seq = request.seq;
                    // Replay admission commits this request to delivery. No socket operation or
                    // disconnect may skip the callback after its id has been remembered. Retries
                    // retain the id & receive replay, so an acknowledgement lost after delivery
                    // cannot cause either a second callback or an accepted-but-undelivered action.
                    self.deliver(class, request);
                    if write_line(&mut writer, &protocol::reply_line(seq, Ok(()))).is_err() {
                        break;
                    }
                }
                Err((seq, refusal)) => {
                    self.log(
                        "menubar_frame_refused",
                        &format!("sender={} reason={}", class.name(), refusal.code()),
                    );
                    let _ = write_line(&mut writer, &protocol::reply_line(seq, Err(refusal)));
                    break;
                }
            }
        }
        (shutdown)();
    }

    /// One frame from a verified sender: well formed, bound to this connection (nonce, sequence),
    /// an action this class may send, and not a repeat.
    fn accept_frame(
        &self,
        class: Class,
        exchange: &mut protocol::Exchange,
        line: &[u8],
    ) -> Result<protocol::Request, (u64, protocol::Refusal)> {
        let request = protocol::parse_request(line).map_err(|refusal| (0, refusal))?;
        exchange
            .admit(&request)
            .map_err(|refusal| (request.seq, refusal))?;
        if !self
            .sender_of(class)
            .is_some_and(|sender| sender.allows(&request.action))
        {
            return Err((request.seq, protocol::Refusal::ActionNotAllowed));
        }
        let ids = match class {
            Class::Menu => &self.inner.menu_ids,
            Class::Intent => &self.inner.intent_ids,
        };
        let fresh = ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .admit(&request.id);
        if !fresh {
            return Err((request.seq, protocol::Refusal::Replay));
        }
        Ok(request)
    }

    fn deliver(&self, class: Class, request: protocol::Request) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match class {
            Class::Menu => {
                self.mark_helper_seen();
                (self.inner.on_action)(&request.action);
            }
            Class::Intent => {
                if let Some(intents) = &self.inner.intents {
                    let _serial = self
                        .inner
                        .intent_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    (intents.handler)(ShortcutIntent {
                        id: request.id,
                        action: request.action,
                    });
                }
            }
        }));
        if outcome.is_err() {
            self.log(
                "menubar_callback_panic",
                &format!("sender={}", class.name()),
            );
        }
    }

    /// Report an unverified connection: a reason code and, when known, the pid. Never content.
    /// Throttled so a process that connects in a loop cannot flood the sink.
    fn refuse(&self, reason: &str, pid: Option<u32>) {
        let now = now_secs();
        let last = self.inner.refusal_last_logged.load(Ordering::SeqCst);
        if now.saturating_sub(last) < REFUSAL_LOG_INTERVAL_SECS && last != 0 {
            self.inner.refusal_suppressed.fetch_add(1, Ordering::SeqCst);
            return;
        }
        self.inner.refusal_last_logged.store(now, Ordering::SeqCst);
        let suppressed = self.inner.refusal_suppressed.swap(0, Ordering::SeqCst);
        let pid = pid.map(|p| format!(" pid={p}")).unwrap_or_default();
        self.log(
            "menubar_sender_refused",
            &format!("reason={reason}{pid} suppressed={suppressed}"),
        );
    }

    /// Mirror host state onto the helper's icon on the configured state channel. Fire-and-forget:
    /// a dropped notification only means a momentarily stale icon. Carries no authority.
    pub fn publish_state(&self, state: &str) {
        publish_state(&self.inner.config.state_channel, state);
    }
}

fn write_line(writer: &mut dyn Write, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Post `state` on `state_channel` (needs no running helper handle). The payload rides in
/// `object`, which for distributed notifications is a plain string. State only: nothing that
/// listens on this channel may treat it as a command.
pub fn publish_state(state_channel: &str, state: &str) {
    use objc2_foundation::{NSDistributedNotificationCenter, NSString};
    unsafe {
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str(state_channel);
        let value = NSString::from_str(state);
        center.postNotificationName_object(&name, Some(&value));
    }
}
