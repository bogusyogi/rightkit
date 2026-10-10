//! The per-OS backend contract. The engine (ids, freshness guard, tier ladder,
//! safety, evidence, delivery) is generic over this trait and tested against
//! `mock::MockPlatform` on every host; `macos` and `windows` implement it with
//! the real accessibility, event, capture, and clipboard APIs.

use crate::keys::Chord;
use crate::model::*;
use crate::proof::FieldText;
use std::path::Path;

/// A low-level input event for tiers 3 and 4.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// Unicode text, delivered as key events (never the clipboard).
    Text(String),
    Key(Chord),
    Click {
        x: f64,
        y: f64,
        button: Button,
        count: u32,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
}

/// Clipboard state held across a paste transaction. `lossless` is false when
/// the clipboard carries formats this snapshot cannot restore, in which case a
/// destructive paste must be refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipSnapshot {
    pub text: Option<String>,
    pub lossless: bool,
    pub change: u64,
    pub formats: usize,
}

pub trait Platform: Send + Sync + 'static {
    /// A retained accessibility element (AXUIElement / IUIAutomationElement).
    type El: Clone + Send + Sync;

    fn kind(&self) -> PlatformKind;
    fn status(&self) -> Status;

    // ---- observe
    fn apps(&self) -> Vec<AppInfo>;
    fn app(&self, pid: i32) -> Option<AppInfo> {
        self.apps().into_iter().find(|a| a.pid == pid)
    }
    fn windows(&self, on_screen_only: bool) -> Vec<WindowInfo>;
    fn frontmost(&self) -> Option<FrontApp>;
    /// Wake web/Electron accessibility trees (macOS AXManualAccessibility).
    fn wake(&self, _pid: i32) {}
    fn root(&self, target: &Target) -> Result<Self::El>;
    /// `None` when the element no longer exists.
    fn read(&self, el: &Self::El) -> Option<RawNode>;
    fn children(&self, el: &Self::El) -> Vec<Self::El>;
    /// Deepest focused element of the app, resolved to an editable editor when
    /// the focus sits on a container.
    fn focused(&self, pid: i32) -> Result<Option<Self::El>>;
    fn same(&self, a: &Self::El, b: &Self::El) -> bool;
    fn window_of(&self, el: &Self::El) -> Option<u64>;
    fn capture(&self, window_id: u64, path: &Path) -> Result<()>;

    // ---- tier 1: semantic
    /// Perform `Press`, `SetValue`, `Scroll`, or `Focus` semantically. Returns a
    /// short detail naming the API used. `Unsupported` means "go up a tier".
    fn semantic(&self, el: &Self::El, action: &Action) -> Result<String>;
    fn menu_press(&self, pid: i32, path: &[String]) -> Result<()>;

    // ---- text proof and native write (delivery)
    fn field_text(&self, el: &Self::El) -> Option<FieldText>;
    /// Selection-exact native write is possible for this element: a real
    /// native text control, not web/rich-editor content, value readable.
    fn native_write_capable(&self, el: &Self::El) -> bool;
    fn native_write(&self, el: &Self::El, text: &str) -> Result<()>;

    // ---- tier 3: targeted events (never the global queue)
    fn post(&self, pid: i32, window: Option<u64>, ev: &InputEvent) -> Result<()>;

    // ---- tier 4: foreground
    fn activate(&self, pid: i32) -> bool;
    fn foreground(&self, pid: i32, ev: &InputEvent) -> Result<()>;

    // ---- clipboard transaction (tier 4 paste)
    fn clip_snapshot(&self) -> Option<ClipSnapshot>;
    /// Stage text; returns the change token after the write.
    fn clip_write(&self, text: &str) -> Option<u64>;
    /// Restore only if nobody wrote since `expected_change`.
    fn clip_restore(&self, snapshot: &ClipSnapshot, expected_change: u64) -> bool;
    fn clip_change(&self) -> u64;

    // ---- lifecycle
    /// Launch without activation. `spec` is a bundle id / app path (macOS) or exe path (Windows).
    fn launch(&self, spec: &str) -> Result<i32>;
    fn terminate(&self, pid: i32) -> Result<()>;
    /// The app id `spec` will run as, decided WITHOUT running anything, so the
    /// allowlist and admission precede the first effect. Errors (fail closed) when
    /// the spec cannot be resolved; the default treats the spec as the id.
    fn resolve_launch(&self, spec: &str) -> Result<String> {
        Ok(spec.to_string())
    }
    /// Hard stop for a process this platform launched and owns (job kill on Windows).
    /// Unsupported where the platform has no owned-process handle.
    fn kill_owned(&self, _pid: i32) -> Result<()> {
        Err(Error::Unsupported("no owned-process kill on this platform"))
    }
}
