//! DPI-independent window geometry persistence and restore.
//!
//! Base: CodeRight's `window_state` (the most developed donor: logical-pixel storage, v0 -> v1
//! migration, exact-dimension / intersecting / nearest-monitor selection, on-screen title bar
//! clamp, maximised-state memory). Changes: no tokio (debounce on a thread), the state file lives
//! under the shell's per-app directory, and every label is tracked, not only `main`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Monitor, Position, Runtime, Size};

const CURRENT_VERSION: u32 = 1;
const STATE_FILE: &str = "window-state.json";
const MIN_WIDTH: f64 = 600.0;
const MIN_HEIGHT: f64 = 400.0;
pub(crate) const CLAMP_MARGIN_X: f64 = 80.0;
pub(crate) const CLAMP_MARGIN_Y: f64 = 40.0;
const SAVE_DEBOUNCE_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

impl MonitorSnapshot {
    pub(crate) fn from_monitor(m: &Monitor) -> Self {
        let scale = m.scale_factor();
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self {
            x: f64::from(m.position().x) / scale,
            y: f64::from(m.position().y) / scale,
            width: f64::from(m.size().width) / scale,
            height: f64::from(m.size().height) / scale,
            scale,
        }
    }
    fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
    /// The window was minimised when last saved. Its `x`, `y`, `width` and `height` are the last
    /// normal geometry, never the off-screen rectangle a minimised window reports.
    #[serde(default)]
    pub minimized: bool,
    pub monitor: MonitorSnapshot,
    #[serde(default)]
    pub version: u32,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PersistedStore {
    #[serde(flatten)]
    states: HashMap<String, WindowState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

fn default_normal(monitor: MonitorSnapshot) -> WindowState {
    let width = 1200.0_f64.min(monitor.width);
    let height = 800.0_f64.min(monitor.height);
    WindowState {
        x: monitor.x + (monitor.width - width).max(0.0) / 2.0,
        y: monitor.y + (monitor.height - height).max(0.0) / 2.0,
        width,
        height,
        maximized: false,
        minimized: false,
        monitor,
        version: CURRENT_VERSION,
    }
}

fn migrate(mut store: PersistedStore) -> PersistedStore {
    for s in store.states.values_mut() {
        if s.version < CURRENT_VERSION {
            let scale = if s.monitor.scale.is_finite() && s.monitor.scale > 0.0 {
                s.monitor.scale
            } else {
                1.0
            };
            s.x /= scale;
            s.y /= scale;
            s.width /= scale;
            s.height /= scale;
            s.version = CURRENT_VERSION;
        }
    }
    store
}

fn load_store(dir: &Path) -> PersistedStore {
    match std::fs::read_to_string(dir.join(STATE_FILE)) {
        Ok(raw) if !raw.trim().is_empty() => {
            serde_json::from_str(&raw).map(migrate).unwrap_or_default()
        }
        _ => PersistedStore::default(),
    }
}

static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

fn save_store(dir: &Path, store: &PersistedStore) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let body = serde_json::to_string_pretty(store).map_err(|e| format!("serialize: {e}"))?;
    // Unique staging per publication: concurrent writers never share (or rename away) one
    // fixed temp file, and the rename is the only step that touches the live file.
    let seq = STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{STATE_FILE}.tmp.{}-{seq}", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(STATE_FILE))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .map_err(|e| format!("rename state: {e}"))
}

fn distance(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    (ax - bx).powi(2) + (ay - by).powi(2)
}
fn distance_to_center(m: &MonitorSnapshot, p: (f64, f64)) -> f64 {
    distance(m.center().0, m.center().1, p.0, p.1)
}

/// Pure geometry projection: where should a saved window land on the current monitors?
pub fn restoreable_state(state: &WindowState, monitors: &[MonitorSnapshot]) -> LogicalRect {
    let saved_monitor = state.monitor;
    let w = LogicalRect {
        x: state.x,
        y: state.y,
        width: state.width,
        height: state.height,
    };
    let exact = monitors
        .iter()
        .filter(|m| {
            (m.width - saved_monitor.width).abs() < 0.5
                && (m.height - saved_monitor.height).abs() < 0.5
        })
        .min_by(|a, b| {
            distance(a.x, a.y, saved_monitor.x, saved_monitor.y).total_cmp(&distance(
                b.x,
                b.y,
                saved_monitor.x,
                saved_monitor.y,
            ))
        });
    let center = (w.x + w.width / 2.0, w.y + w.height / 2.0);
    let intersecting = monitors
        .iter()
        .filter(|m| {
            w.x < m.x + m.width
                && w.x + w.width > m.x
                && w.y < m.y + m.height
                && w.y + w.height > m.y
        })
        .min_by(|a, b| distance_to_center(a, center).total_cmp(&distance_to_center(b, center)));
    let target = exact.or(intersecting).copied().unwrap_or_else(|| {
        *monitors
            .iter()
            .min_by(|a, b| {
                distance_to_center(a, saved_monitor.center())
                    .total_cmp(&distance_to_center(b, saved_monitor.center()))
            })
            .expect("non-empty monitors")
    });
    let mut x = w.x + target.x - saved_monitor.x;
    let mut y = w.y + target.y - saved_monitor.y;
    let width = w.width.min(target.width).max(MIN_WIDTH.min(target.width));
    let height = w
        .height
        .min(target.height)
        .max(MIN_HEIGHT.min(target.height));
    let max_x = target.x + target.width - CLAMP_MARGIN_X;
    let max_y = target.y + target.height - CLAMP_MARGIN_Y;
    let min_x = target.x - (width - CLAMP_MARGIN_X).max(0.0);
    x = if min_x <= max_x {
        x.clamp(min_x, max_x)
    } else {
        target.x
    };
    y = if target.y <= max_y {
        y.clamp(target.y, max_y)
    } else {
        target.y
    };
    LogicalRect {
        x,
        y,
        width,
        height,
    }
}

/// Result of [`WindowStateStore::import_legacy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyImport {
    /// The legacy file was read; `entries` were added (0 when every label already existed).
    Imported { entries: usize },
    /// An earlier call already imported this source.
    AlreadyImported,
    /// No legacy file exists (nothing marked; a later call may still import).
    NoLegacyFile,
}

/// Per-app store with debounced saves.
pub struct WindowStateStore {
    dir: PathBuf,
    generations: Mutex<HashMap<String, Arc<AtomicU64>>>,
    /// Serialises every load -> modify -> publish cycle. Without it two saves (a debounced
    /// thread and an exit save, or two labels) can each load, insert only their own entry and
    /// publish, silently dropping the other's; an older scheduled save can also win the rename.
    io: Mutex<()>,
}

impl WindowStateStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            generations: Mutex::new(HashMap::new()),
            io: Mutex::new(()),
        }
    }
    fn io_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    fn generation(&self, label: &str) -> Arc<AtomicU64> {
        Arc::clone(
            self.generations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(label.to_owned())
                .or_default(),
        )
    }

    /// The state saved for `label`, if any. Reads the file; applies nothing to any window.
    pub fn saved(&self, label: &str) -> Option<WindowState> {
        let _io = self.io_lock();
        load_store(&self.dir).states.remove(label)
    }

    /// Import a legacy per-app geometry store once (M1-G5; CodeRight `window_state/mod.rs:92,110-118`
    /// shape: a flat `label -> state` JSON object, `version` 0 meaning physical pixels).
    ///
    /// Each legacy entry runs through the same v0 -> v1 migration the store applies on load, then is
    /// added under its owner label (`label_map` is `(legacy, owner)` pairs; an empty map keeps every
    /// label). An owner entry that already exists is never overwritten. Idempotent: a marker file
    /// `window-state.imported-<source_id>` is written after the first attempt that found the legacy
    /// file (a missing file is not marked, so a later-appearing file still imports), and later
    /// calls return [`LegacyImport::AlreadyImported`]. The legacy file is left in place. A legacy
    /// file that does not parse is reported as an error and not marked.
    pub fn import_legacy(
        &self,
        source_id: &str,
        legacy_file: &Path,
        label_map: &[(&str, &str)],
    ) -> Result<LegacyImport, String> {
        if source_id.is_empty()
            || !source_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err("invalid import source id".into());
        }
        let marker = self.dir.join(format!("window-state.imported-{source_id}"));
        if marker.exists() {
            return Ok(LegacyImport::AlreadyImported);
        }
        let raw = match std::fs::read_to_string(legacy_file) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LegacyImport::NoLegacyFile)
            }
            Err(e) => return Err(format!("read {}: {e}", legacy_file.display())),
        };
        // Marker check, RMW and marker publication are one cycle: a concurrent save between
        // the load and the publish would otherwise be overwritten by the imported snapshot.
        let _io = self.io_lock();
        if marker.exists() {
            return Ok(LegacyImport::AlreadyImported);
        }
        let legacy: PersistedStore = if raw.trim().is_empty() {
            PersistedStore::default()
        } else {
            serde_json::from_str(&raw)
                .map(migrate)
                .map_err(|e| format!("parse {}: {e}", legacy_file.display()))?
        };
        let mut store = load_store(&self.dir);
        let mut imported = 0;
        for (label, state) in legacy.states {
            let target = if label_map.is_empty() {
                label
            } else {
                match label_map.iter().find(|(from, _)| *from == label) {
                    Some((_, to)) => (*to).to_owned(),
                    None => continue,
                }
            };
            if store.states.contains_key(&target) {
                continue;
            }
            store.states.insert(target, state);
            imported += 1;
        }
        if imported > 0 {
            save_store(&self.dir, &store)?;
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("create {}: {e}", self.dir.display()))?;
        std::fs::write(&marker, b"1").map_err(|e| format!("write {}: {e}", marker.display()))?;
        Ok(LegacyImport::Imported { entries: imported })
    }

    /// Record `state` for `label` without touching a window (a host that tracks its own geometry,
    /// or a test).
    pub fn save_state(&self, label: &str, state: WindowState) -> Result<(), String> {
        let _io = self.io_lock();
        let mut store = load_store(&self.dir);
        store.states.insert(label.to_owned(), state);
        save_store(&self.dir, &store)
    }

    /// Apply saved geometry to `label` (hidden or not). Returns whether a saved state existed.
    /// Positions only; equal to [`WindowStateStore::restore_with`] with the default options.
    pub fn restore<R: Runtime>(&self, app: &AppHandle<R>, label: &str) -> Result<bool, String> {
        self.restore_with(app, label, &RestoreOptions::default())
    }

    /// [`WindowStateStore::restore`] that can also bring the window up: `options` may unminimise,
    /// show and focus it, and re-minimise a window saved minimised. The visibility options apply
    /// whenever the window exists, saved geometry or not (a first run still shows). Returns whether
    /// saved geometry was applied. Nothing calls this for you outside the full `Shell` plugin.
    pub fn restore_with<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        label: &str,
        options: &RestoreOptions,
    ) -> Result<bool, String> {
        let Some(window) = app.get_webview_window(label) else {
            return Ok(false);
        };
        // The visibility guard has the last word on unminimise, show and focus.
        let constrained = options.constrained(crate::lifecycle::reveal_mode(
            app,
            crate::lifecycle::RevealSource::Restore,
        ));
        let options = &constrained;
        let monitors = window
            .available_monitors()
            .map_err(|e| format!("available_monitors: {e}"))?;
        let snapshots: Vec<_> = monitors.iter().map(MonitorSnapshot::from_monitor).collect();
        let state = self.saved(label).filter(|_| !snapshots.is_empty());
        let mut target = WindowTarget(&window);
        match state {
            Some(state) => {
                let rect = restoreable_state(&state, &snapshots);
                apply_restore(&mut target, rect, &state, options);
                Ok(true)
            }
            None => {
                apply_visibility(&mut target, options, false);
                Ok(false)
            }
        }
    }

    pub fn schedule_save<R: Runtime>(self: &Arc<Self>, app: AppHandle<R>, label: String) {
        let token = self.begin_save(&label);
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(SAVE_DEBOUNCE_MS));
            if this.generation(&label).load(Ordering::SeqCst) == token {
                let _ = this.save_generation(&app, &label, token);
            }
        });
    }

    // Token allocation and publication share one lock: a newer invocation invalidates all
    // older snapshots, including an in-flight debounced save and immediate exit saves.
    fn begin_save(&self, label: &str) -> u64 {
        let _io = self.io_lock();
        self.generation(label).fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn save_now<R: Runtime>(&self, app: &AppHandle<R>, label: &str) -> Result<(), String> {
        self.save_generation(app, label, self.begin_save(label))
    }

    fn save_generation<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        label: &str,
        token: u64,
    ) -> Result<(), String> {
        let Some(window) = app.get_webview_window(label) else {
            return Ok(());
        };
        if window.is_fullscreen().unwrap_or(false) {
            return Ok(());
        }
        let minimized = window.is_minimized().unwrap_or(false);
        let scale = window.scale_factor().map_err(|e| format!("scale: {e}"))?;
        let Some(monitor) = window
            .current_monitor()
            .map_err(|e| format!("current_monitor: {e}"))?
        else {
            return Ok(());
        };
        let monitor = MonitorSnapshot::from_monitor(&monitor);
        let maximized = window.is_maximized().unwrap_or(false);
        // A minimised window reports an off-screen rectangle on some platforms: never read it.
        let geometry = if maximized || minimized {
            None
        } else {
            let p = window
                .outer_position()
                .map_err(|e| format!("outer_position: {e}"))?;
            let s = window
                .inner_size()
                .map_err(|e| format!("inner_size: {e}"))?;
            Some((
                f64::from(p.x) / scale,
                f64::from(p.y) / scale,
                f64::from(s.width) / scale,
                f64::from(s.height) / scale,
            ))
        };
        self.publish_generation(label, token, monitor, maximized, minimized, geometry)
    }

    fn publish_generation(
        &self,
        label: &str,
        token: u64,
        monitor: MonitorSnapshot,
        maximized: bool,
        minimized: bool,
        geometry: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), String> {
        let _io = self.io_lock();
        if self.generation(label).load(Ordering::SeqCst) != token {
            return Ok(());
        }
        let mut store = load_store(&self.dir);
        let state = next_state(
            store.states.get(label),
            monitor,
            maximized,
            minimized,
            geometry,
        );
        store.states.insert(label.to_owned(), state);
        save_store(&self.dir, &store)
    }
}

/// The state to save next. `geometry` is the live `(x, y, width, height)` in logical pixels and is
/// only read for a window that is neither maximised nor minimised; otherwise the last normal
/// geometry is kept (or the default rectangle when none was saved).
pub fn next_state(
    prev: Option<&WindowState>,
    monitor: MonitorSnapshot,
    maximized: bool,
    minimized: bool,
    geometry: Option<(f64, f64, f64, f64)>,
) -> WindowState {
    match (maximized || minimized, geometry) {
        (false, Some((x, y, width, height))) => WindowState {
            x,
            y,
            width,
            height,
            maximized: false,
            minimized: false,
            monitor,
            version: CURRENT_VERSION,
        },
        _ => {
            let normal = prev.cloned().unwrap_or_else(|| default_normal(monitor));
            WindowState {
                // A minimised window may report either; keep what was saved if it was maximised.
                maximized: maximized || (minimized && normal.maximized),
                minimized,
                monitor,
                version: CURRENT_VERSION,
                ..normal
            }
        }
    }
}

/// What [`WindowStateStore::restore_with`] does besides positioning. The default does nothing else,
/// which is what `restore` and the full plugin have always done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestoreOptions {
    /// Unminimise the window.
    pub unminimize: bool,
    /// Show the window.
    pub show: bool,
    /// Give the window focus (raises it; leave off for a window that must not steal focus).
    pub focus: bool,
    /// Minimise again a window that was saved minimised. Applied last, after `show`.
    pub restore_minimized: bool,
}

impl RestoreOptions {
    /// These options as the visibility guard allows them: `Forbidden` strips unminimise, show,
    /// focus and re-minimise; `Background` strips unminimise and focus (QA never raises a window).
    pub fn constrained(self, mode: crate::lifecycle::RevealMode) -> Self {
        use crate::lifecycle::RevealMode;
        match mode {
            RevealMode::Foreground => self,
            RevealMode::Background => Self {
                unminimize: false,
                focus: false,
                ..self
            },
            RevealMode::Forbidden => Self {
                unminimize: false,
                show: false,
                focus: false,
                restore_minimized: false,
            },
        }
    }
}

/// The window calls a restore makes, so the order is testable without a window.
pub trait RestoreTarget {
    fn unmaximize(&mut self);
    fn set_size(&mut self, width: f64, height: f64);
    fn set_position(&mut self, x: f64, y: f64);
    fn maximize(&mut self);
    fn unminimize(&mut self);
    fn show(&mut self);
    fn focus(&mut self);
    fn minimize(&mut self);
}

struct WindowTarget<'a, R: Runtime>(&'a tauri::WebviewWindow<R>);

impl<R: Runtime> RestoreTarget for WindowTarget<'_, R> {
    fn unmaximize(&mut self) {
        let _ = self.0.unmaximize();
    }
    fn set_size(&mut self, width: f64, height: f64) {
        let _ = self
            .0
            .set_size(Size::Logical(LogicalSize { width, height }));
    }
    fn set_position(&mut self, x: f64, y: f64) {
        let _ = self
            .0
            .set_position(Position::Logical(LogicalPosition { x, y }));
    }
    fn maximize(&mut self) {
        let _ = self.0.maximize();
    }
    fn unminimize(&mut self) {
        let _ = self.0.unminimize();
    }
    fn show(&mut self) {
        let _ = self.0.show();
    }
    fn focus(&mut self) {
        let _ = self.0.set_focus();
    }
    fn minimize(&mut self) {
        let _ = self.0.minimize();
    }
}

/// Geometry first (size and position, then maximised), then the requested visibility.
pub fn apply_restore<T: RestoreTarget>(
    target: &mut T,
    rect: LogicalRect,
    state: &WindowState,
    options: &RestoreOptions,
) {
    target.unmaximize();
    target.set_size(rect.width, rect.height);
    target.set_position(rect.x, rect.y);
    if state.maximized {
        target.maximize();
    }
    apply_visibility(target, options, state.minimized);
}

fn apply_visibility<T: RestoreTarget>(
    target: &mut T,
    options: &RestoreOptions,
    was_minimized: bool,
) {
    if options.unminimize {
        target.unminimize();
    }
    if options.show {
        target.show();
    }
    if options.focus {
        target.focus();
    }
    if options.restore_minimized && was_minimized {
        target.minimize();
    }
}

#[cfg(test)]
mod publication_order_tests {
    use super::*;

    #[test]
    fn newer_immediate_save_invalidates_paused_same_label_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(WindowStateStore::new(dir.path().to_owned()));
        let (captured_tx, captured_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let old_store = Arc::clone(&store);
        let old = std::thread::spawn(move || {
            let token = old_store.begin_save("main");
            let monitor = MonitorSnapshot {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
                scale: 1.0,
            };
            captured_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            old_store
                .publish_generation(
                    "main",
                    token,
                    monitor,
                    false,
                    false,
                    Some((1.0, 2.0, 300.0, 400.0)),
                )
                .unwrap();
        });
        captured_rx.recv().unwrap();
        let token = store.begin_save("main");
        store
            .publish_generation(
                "main",
                token,
                MonitorSnapshot {
                    x: 0.0,
                    y: 0.0,
                    width: 1920.0,
                    height: 1080.0,
                    scale: 1.0,
                },
                false,
                false,
                Some((20.0, 30.0, 600.0, 700.0)),
            )
            .unwrap();
        resume_tx.send(()).unwrap();
        old.join().unwrap();
        let state = store.saved("main").unwrap();
        assert_eq!(
            (state.x, state.y, state.width, state.height),
            (20.0, 30.0, 600.0, 700.0)
        );
    }
}
