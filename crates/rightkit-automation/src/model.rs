//! Platform-neutral wire types shared by every backend.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.w && y <= self.y + self.h
    }
}

/// A running GUI application. `app_id` is the bundle id (macOS) or the lowercase
/// exe file name (Windows); it is the allowlist key.
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub pid: i32,
    pub app_id: String,
    pub name: String,
    pub path: Option<String>,
    /// Kernel process start time (identity across pid reuse).
    pub started: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub window_id: u64,
    pub pid: i32,
    pub owner: String,
    pub title: String,
    pub layer: i32,
    pub on_screen: bool,
    pub bounds: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrontApp {
    pub pid: i32,
    pub app_id: String,
    pub window_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    MacOs,
    Windows,
    Mock,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub platform: PlatformKind,
    /// macOS Accessibility; always true on Windows (UIA needs no grant).
    pub accessibility: bool,
    /// macOS Screen Recording; always true on Windows.
    pub screen_capture: bool,
    pub notes: Vec<String>,
}

/// Which target an observation or act addresses.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub pid: i32,
    /// `None` = the app's main/focused window.
    pub window_id: Option<u64>,
}

impl Target {
    pub fn app(pid: i32) -> Self {
        Target {
            pid,
            window_id: None,
        }
    }
    pub fn window(pid: i32, window_id: u64) -> Self {
        Target {
            pid,
            window_id: Some(window_id),
        }
    }
}

/// What the platform reads from one element. Backends never allocate ids.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawNode {
    /// Normalized cross-platform role (button, textfield, textarea, text, window, ...).
    pub role: String,
    /// The OS's own role string (AXButton, UIA ControlType name).
    pub native_role: String,
    pub subrole: Option<String>,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    pub enabled: Option<bool>,
    pub secure: bool,
    pub frame: Option<Rect>,
    /// Normalized capabilities: press, set_value, insert_text, scroll, toggle,
    /// expand, select, focus.
    pub caps: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub id: String,
    pub role: String,
    pub native_role: String,
    pub subrole: Option<String>,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    pub enabled: Option<bool>,
    pub secure: bool,
    pub frame: Option<Rect>,
    pub caps: Vec<String>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }
    pub fn find<'a>(&'a self, pred: &dyn Fn(&Node) -> bool) -> Option<&'a Node> {
        if pred(self) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(pred))
    }
    pub fn by_id(&self, id: &str) -> Option<&Node> {
        self.find(&|n| n.id == id)
    }
    pub fn render(&self, out: &mut String, depth: usize) {
        use std::fmt::Write;
        let _ = write!(out, "{}{} #{}", "  ".repeat(depth), self.role, self.id);
        if let Some(t) = &self.name {
            let _ = write!(out, " {t:?}");
        }
        if let Some(v) = &self.value {
            let v: String = v.chars().take(60).collect();
            let _ = write!(out, " = {v:?}");
        }
        if !self.caps.is_empty() {
            let _ = write!(out, " [{}]", self.caps.join(","));
        }
        out.push('\n');
        for c in &self.children {
            c.render(out, depth + 1);
        }
    }
}

/// An observation. Acts name `(snapshot id, element id)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub id: String,
    pub pid: i32,
    pub app_id: String,
    pub window_id: Option<u64>,
    pub taken_ms: u64,
    /// Hash of the tree's structure (ids, roles, enabled), not of its values.
    pub fingerprint: u64,
    pub truncated: bool,
    /// Id of the focused element, if it is inside this snapshot.
    pub focused: Option<String>,
    pub root: Node,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_depth: usize,
    pub max_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 12,
            max_nodes: 600,
        }
    }
}

/// Input ladder. Ordering is the order of preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// AX/UIA semantic action or value write: no input events at all.
    Semantic = 1,
    /// In-app control channel (rightkit-qa-driver loopback server).
    InApp = 2,
    /// Per-pid / per-window addressed events. Never moves the user's pointer.
    Targeted = 3,
    /// Global input queue with the app activated. Explicit opt-in only.
    Foreground = 4,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Semantic => "semantic",
            Tier::InApp => "in_app",
            Tier::Targeted => "targeted",
            Tier::Foreground => "foreground",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Press,
    SetValue(String),
    /// Insert text at the caret of the element (or the focused field).
    TypeText(String),
    /// Chord sequence, space separated: `"cmd+a delete"`.
    Keys(String),
    /// Click the element's centre, or an explicit screen point.
    Click {
        point: Option<(f64, f64)>,
        button: Button,
        count: u32,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    Focus,
    MenuPress(Vec<String>),
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Press => "press",
            Action::SetValue(_) => "set_value",
            Action::TypeText(_) => "type_text",
            Action::Keys(_) => "keys",
            Action::Click { .. } => "click",
            Action::Scroll { .. } => "scroll",
            Action::Focus => "focus",
            Action::MenuPress(_) => "menu_press",
        }
    }
    /// Text payload, for redaction.
    pub fn payload(&self) -> Option<&str> {
        match self {
            Action::SetValue(s) | Action::TypeText(s) | Action::Keys(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskClass {
    Delete,
    Send,
    Purchase,
    Close,
}

impl RiskClass {
    pub fn label(self) -> &'static str {
        match self {
            RiskClass::Delete => "delete",
            RiskClass::Send => "send",
            RiskClass::Purchase => "purchase",
            RiskClass::Close => "close",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    PermissionDenied(&'static str),
    NotFound(String),
    /// Platform API failure with a code and the operation that failed.
    Platform(i64, &'static str),
    Unsupported(&'static str),
    /// Safety policy refused (allowlist, kill switch, deny list, secure field).
    Blocked(String),
    /// Risky action needs `ActOptions::confirm`.
    NeedsConfirmation(RiskClass),
    /// Snapshot id unknown or evicted.
    UnknownSnapshot(String),
    /// The caller's max tier does not reach any tier that can do this.
    NoTier(String),
    FocusMoved {
        before: Option<i32>,
        after: Option<i32>,
    },
    Invalid(String),
    /// The caller's admission hook refused the effect before anything ran.
    Denied(String),
    /// Another controller holds the lease on this app.
    LeaseHeld {
        holder: String,
        epoch: u64,
    },
    /// The fence's epoch is not the current lease epoch (stale controller).
    StaleLease {
        presented: u64,
        current: Option<u64>,
    },
    /// The fence skipped ahead of `acked_seq + 1`.
    OutOfOrder {
        expected: u64,
        got: u64,
    },
    /// `seq <= acked_seq`: already processed, deliberately not executed again.
    Replayed {
        acked_seq: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::PermissionDenied(p) => write!(f, "permission denied: {p}"),
            Error::NotFound(s) => write!(f, "not found: {s}"),
            Error::Platform(c, op) => write!(f, "platform error {c} during {op}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::Blocked(s) => write!(f, "blocked by safety policy: {s}"),
            Error::NeedsConfirmation(r) => {
                write!(f, "confirmation required for {} action", r.label())
            }
            Error::UnknownSnapshot(s) => write!(f, "unknown snapshot {s}"),
            Error::NoTier(s) => write!(f, "no allowed tier can perform: {s}"),
            Error::FocusMoved { before, after } => {
                write!(f, "frontmost app moved {before:?} -> {after:?}")
            }
            Error::Invalid(s) => write!(f, "invalid: {s}"),
            Error::Denied(s) => write!(f, "denied by admission: {s}"),
            Error::LeaseHeld { holder, epoch } => {
                write!(f, "lease held by {holder} (epoch {epoch})")
            }
            Error::StaleLease { presented, current } => write!(
                f,
                "stale lease: presented epoch {presented}, current {current:?}"
            ),
            Error::OutOfOrder { expected, got } => {
                write!(f, "input out of order: expected seq {expected}, got {got}")
            }
            Error::Replayed { acked_seq } => write!(
                f,
                "input already processed (acked seq {acked_seq}); not executed again"
            ),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
