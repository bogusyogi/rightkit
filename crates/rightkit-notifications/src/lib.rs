//! Neutral native notifications owner (rightkit#958): usable without Tauri.
//!
//! `rightkit-tauri-shell`'s `notify.rs` could post a notification only inside the Tauri crate, had no
//! click or named actions, no replace or withdraw by key and no permission query, and reported Linux as
//! unsupported. This crate is the owner; the shell delegates to it.
//!
//! ```no_run
//! use rightkit_notifications::{Client, Notification};
//!
//! let client = Client::new("com.orthiclabs.pulse");
//! client.on_activation(|activation| println!("{:?}", activation));
//! let note = Notification::new("Build finished").body("3 warnings").key("build").action("open", "Open");
//! let handle = client.send(&note)?;
//! handle.update(&Notification::new("Build finished").body("0 warnings").key("build"))?;
//! handle.close()?;
//! client.set_badge(2)?;
//! # Ok::<(), rightkit_notifications::Error>(())
//! ```
//!
//! # Platforms
//!
//! | | macOS | Windows | Linux |
//! |---|---|---|---|
//! | backend | UserNotifications | WinRT toast | freedesktop D-Bus (`notify-rust`) |
//! | replace by key | yes (request identifier) | yes (toast tag) | yes (server id) |
//! | close by key | yes | yes | yes |
//! | named actions and click callback | yes (categories, delegate) | no | no |
//! | permission query and prompt | yes | no (always granted) | no (always granted) |
//! | read back what is shown ([`Client::delivered`]) | yes | no (empty) | no (empty) |
//! | badge | dock badge (macOS 13+) | Start and taskbar badge number (needs an AUMID) | none |
//!
//! Known gaps, reported by [`Capabilities`] rather than hidden: on Windows a click does not reach
//! [`Client::on_activation`] (toast activation after the app exited needs COM activation registered
//! at install time; a click while running is not wired), and no taskbar overlay icon is drawn, only
//! the numeric badge the shell allows an AUMID app. On Linux there are no action buttons and no badge.
//! macOS needs an app bundle (`.app` with a bundle identifier); a bare binary gets
//! [`Error::Unsupported`], never a crash.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod platform;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// This platform or process cannot do it (no app bundle, no notification service).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The person has not allowed notifications for this app.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The OS did not answer in time.
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("{0}")]
    Backend(String),
}

/// A button on a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub title: String,
}

/// The alert sound.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Sound {
    /// No sound.
    #[default]
    Silent,
    /// The system default.
    Default,
    /// A system sound by name (macOS `Glass`; Windows `IM`, `Mail`, `Reminder`, `SMS`).
    Named(String),
}

/// What to show. Build with [`Notification::new`] and the chained setters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// Replace key. A notification sent with the key of one still shown replaces it, and
    /// [`Client::close`] withdraws it by key. Default: a fresh key per send.
    pub key: Option<String>,
    /// Groups notifications of one conversation or task (macOS thread identifier; ignored elsewhere).
    pub thread: Option<String>,
    pub sound: Sound,
    pub actions: Vec<Action>,
}

impl Notification {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: String::new(),
            key: None,
            thread: None,
            sound: Sound::Silent,
            actions: Vec::new(),
        }
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn thread(mut self, thread: impl Into<String>) -> Self {
        self.thread = Some(thread.into());
        self
    }

    pub fn sound(mut self, sound: Sound) -> Self {
        self.sound = sound;
        self
    }

    /// Adds a button. Where [`Capabilities::actions`] is false it is ignored.
    pub fn action(mut self, id: impl Into<String>, title: impl Into<String>) -> Self {
        self.actions.push(Action {
            id: id.into(),
            title: title.into(),
        });
        self
    }
}

/// Whether the person allowed this app to notify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
    /// Not asked yet; [`Client::request_permission`] shows the system prompt.
    NotDetermined,
    /// Delivered quietly to the notification list (macOS).
    Provisional,
    /// The platform has no per-app permission or this build cannot read it.
    Unsupported,
}

/// What this platform can do. Check before building UI around a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Named action buttons, and their clicks reach [`Client::on_activation`].
    pub actions: bool,
    /// A click on the notification body reaches [`Client::on_activation`].
    pub activation: bool,
    /// Sending with a key replaces the shown notification.
    pub replace: bool,
    /// [`Handle::close`] and [`Client::close`] withdraw a shown notification.
    pub close: bool,
    /// [`Client::permission`] reflects a real per-app permission.
    pub permission: bool,
    /// [`Client::set_badge`] sets the dock (macOS) or Start and taskbar (Windows) badge.
    pub badge: bool,
    /// [`Client::delivered`] reads back what the OS still shows for this app (macOS). Where false it
    /// answers an empty list.
    pub delivered: bool,
}

/// One notification the OS reports as still delivered to this app (macOS read-back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    /// The notification's key (the request identifier).
    pub key: String,
    pub title: String,
    pub body: String,
    /// The thread identifier; the key when the sender set none.
    pub thread: String,
}

/// What the person did with a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivationKind {
    /// The body was clicked.
    Default,
    /// The notification was dismissed.
    Dismiss,
    /// A button was clicked; the [`Action::id`].
    Action(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// The notification's key.
    pub key: String,
    pub kind: ActivationKind,
}

pub(crate) type ActivationHandler = Arc<dyn Fn(Activation) + Send + Sync>;

pub(crate) static HANDLER: Mutex<Option<ActivationHandler>> = Mutex::new(None);

/// Deliver an activation to the registered handler (called by the platform backends).
pub(crate) fn dispatch(activation: Activation) {
    let handler = HANDLER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(handler) = handler {
        handler(activation);
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn fresh_key() -> String {
    format!(
        "rk-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// The notification front door. One per app; cheap to clone.
#[derive(Debug, Clone)]
pub struct Client {
    app_id: Arc<str>,
}

impl Client {
    /// `app_id` attributes notifications to the app: on Windows the AppUserModelID (an installed app
    /// owns one through its Start-menu shortcut; Tauri's installers use the bundle identifier), on
    /// Linux the application name; macOS takes the bundle identifier from the app bundle.
    pub fn new(app_id: &str) -> Self {
        Self {
            app_id: Arc::from(app_id.trim()),
        }
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn capabilities(&self) -> Capabilities {
        platform::capabilities()
    }

    /// The current permission. Never prompts.
    pub fn permission(&self) -> Result<Permission, Error> {
        platform::permission()
    }

    /// Asks the person when the permission is not determined, and returns the answer. Blocks until they
    /// answer (up to 60 s on macOS): call it from a worker thread, never the UI thread.
    pub fn request_permission(&self) -> Result<Permission, Error> {
        platform::request_permission()
    }

    /// Registers the one callback that hears clicks, dismissals and button presses. It runs on a system
    /// thread. Replaces an earlier callback. Where [`Capabilities::activation`] is false it never runs.
    pub fn on_activation(&self, handler: impl Fn(Activation) + Send + Sync + 'static) {
        *HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(handler));
        platform::activation_registered();
    }

    /// Shows a notification (or replaces the one with the same key) and returns its handle.
    pub fn send(&self, notification: &Notification) -> Result<Handle, Error> {
        let key = notification.key.clone().unwrap_or_else(fresh_key);
        platform::post(&self.app_id, &key, notification)?;
        Ok(Handle {
            app_id: self.app_id.clone(),
            key,
        })
    }

    /// Withdraws the shown notification with this key. Nothing shown is not an error.
    pub fn close(&self, key: &str) -> Result<(), Error> {
        platform::close(&self.app_id, key)
    }

    /// The notifications the OS still shows for this app, read back from the system (macOS). An empty
    /// list where [`Capabilities::delivered`] is false. Blocks until the OS answers (up to 10 s).
    pub fn delivered(&self) -> Result<Vec<Delivered>, Error> {
        platform::delivered(&self.app_id)
    }

    /// Withdraws every notification this app has shown or has pending (macOS, Windows history, Linux
    /// those shown by this process). Nothing shown is not an error.
    pub fn close_all(&self) -> Result<(), Error> {
        platform::close_all(&self.app_id)
    }

    /// Sets the badge count (0 clears it): the dock badge on macOS, the Start and taskbar number badge on
    /// Windows. [`Error::Unsupported`] elsewhere.
    pub fn set_badge(&self, count: u32) -> Result<(), Error> {
        platform::set_badge(&self.app_id, count)
    }
}

/// A sent notification: update it in place or withdraw it.
#[derive(Debug, Clone)]
pub struct Handle {
    app_id: Arc<str>,
    key: String,
}

impl Handle {
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Replaces the shown notification with `notification`'s content, keeping this handle's key.
    pub fn update(&self, notification: &Notification) -> Result<(), Error> {
        platform::post(&self.app_id, &self.key, notification)
    }

    /// Withdraws it.
    pub fn close(&self) -> Result<(), Error> {
        platform::close(&self.app_id, &self.key)
    }
}
