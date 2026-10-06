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
const CLAMP_MARGIN_X: f64 = 80.0;
const CLAMP_MARGIN_Y: f64 = 40.0;
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
    fn from_monitor(m: &Monitor) -> Self {
        let scale = m.scale_factor();
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
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
        monitor,
        version: CURRENT_VERSION,
    }
}

fn migrate(mut store: PersistedStore) -> PersistedStore {
    for s in store.states.values_mut() {
        if s.version < CURRENT_VERSION {
            let scale = if s.monitor.scale.is_finite() && s.monitor.scale > 0.0 { s.monitor.scale } else { 1.0 };
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
        Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).map(migrate).unwrap_or_default(),
        _ => PersistedStore::default(),
    }
}

fn save_store(dir: &Path, store: &PersistedStore) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let body = serde_json::to_string_pretty(store).map_err(|e| format!("serialize: {e}"))?;
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
    std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(STATE_FILE)).map_err(|e| format!("rename state: {e}"))
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
    let w = LogicalRect { x: state.x, y: state.y, width: state.width, height: state.height };
    let exact = monitors
        .iter()
        .filter(|m| (m.width - saved_monitor.width).abs() < 0.5 && (m.height - saved_monitor.height).abs() < 0.5)
        .min_by(|a, b| distance(a.x, a.y, saved_monitor.x, saved_monitor.y).total_cmp(&distance(b.x, b.y, saved_monitor.x, saved_monitor.y)));
    let center = (w.x + w.width / 2.0, w.y + w.height / 2.0);
    let intersecting = monitors
        .iter()
        .filter(|m| w.x < m.x + m.width && w.x + w.width > m.x && w.y < m.y + m.height && w.y + w.height > m.y)
        .min_by(|a, b| distance_to_center(a, center).total_cmp(&distance_to_center(b, center)));
    let target = exact
        .or(intersecting)
        .copied()
        .unwrap_or_else(|| {
            *monitors
                .iter()
                .min_by(|a, b| distance_to_center(a, saved_monitor.center()).total_cmp(&distance_to_center(b, saved_monitor.center())))
                .expect("non-empty monitors")
        });
    let mut x = w.x + target.x - saved_monitor.x;
    let mut y = w.y + target.y - saved_monitor.y;
    let width = w.width.min(target.width).max(MIN_WIDTH.min(target.width));
    let height = w.height.min(target.height).max(MIN_HEIGHT.min(target.height));
    let max_x = target.x + target.width - CLAMP_MARGIN_X;
    let max_y = target.y + target.height - CLAMP_MARGIN_Y;
    let min_x = target.x - (width - CLAMP_MARGIN_X).max(0.0);
    x = if min_x <= max_x { x.clamp(min_x, max_x) } else { target.x };
    y = if target.y <= max_y { y.clamp(target.y, max_y) } else { target.y };
    LogicalRect { x, y, width, height }
}

/// Per-app store with debounced saves.
pub struct WindowStateStore {
    dir: PathBuf,
    generations: Mutex<HashMap<String, Arc<AtomicU64>>>,
}

impl WindowStateStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, generations: Mutex::new(HashMap::new()) }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    fn generation(&self, label: &str) -> Arc<AtomicU64> {
        Arc::clone(self.generations.lock().unwrap_or_else(|e| e.into_inner()).entry(label.to_owned()).or_default())
    }

    /// Apply saved geometry to `label` (hidden or not). Returns whether a saved state existed.
    pub fn restore<R: Runtime>(&self, app: &AppHandle<R>, label: &str) -> Result<bool, String> {
        let Some(window) = app.get_webview_window(label) else { return Ok(false) };
        let store = load_store(&self.dir);
        let monitors = window.available_monitors().map_err(|e| format!("available_monitors: {e}"))?;
        let snapshots: Vec<_> = monitors.iter().map(MonitorSnapshot::from_monitor).collect();
        let Some(state) = store.states.get(label).filter(|_| !snapshots.is_empty()) else { return Ok(false) };
        let target = restoreable_state(state, &snapshots);
        let _ = window.unmaximize();
        let _ = window.set_size(Size::Logical(LogicalSize { width: target.width, height: target.height }));
        let _ = window.set_position(Position::Logical(LogicalPosition { x: target.x, y: target.y }));
        if state.maximized {
            let _ = window.maximize();
        }
        Ok(true)
    }

    pub fn schedule_save<R: Runtime>(self: &Arc<Self>, app: AppHandle<R>, label: String) {
        let generation = self.generation(&label);
        let token = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(SAVE_DEBOUNCE_MS));
            if this.generation(&label).load(Ordering::SeqCst) == token {
                let _ = this.save_now(&app, &label);
            }
        });
    }

    pub fn save_now<R: Runtime>(&self, app: &AppHandle<R>, label: &str) -> Result<(), String> {
        let Some(window) = app.get_webview_window(label) else { return Ok(()) };
        if window.is_fullscreen().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
            return Ok(());
        }
        let scale = window.scale_factor().map_err(|e| format!("scale: {e}"))?;
        let Some(monitor) = window.current_monitor().map_err(|e| format!("current_monitor: {e}"))? else { return Ok(()) };
        let monitor = MonitorSnapshot::from_monitor(&monitor);
        let maximized = window.is_maximized().unwrap_or(false);
        let mut store = load_store(&self.dir);
        let state = if maximized {
            let normal = store.states.get(label).cloned().unwrap_or_else(|| default_normal(monitor));
            WindowState { maximized: true, monitor, version: CURRENT_VERSION, ..normal }
        } else {
            let p = window.outer_position().map_err(|e| format!("outer_position: {e}"))?;
            let s = window.inner_size().map_err(|e| format!("inner_size: {e}"))?;
            WindowState {
                x: f64::from(p.x) / scale,
                y: f64::from(p.y) / scale,
                width: f64::from(s.width) / scale,
                height: f64::from(s.height) / scale,
                maximized: false,
                monitor,
                version: CURRENT_VERSION,
            }
        };
        store.states.insert(label.to_owned(), state);
        save_store(&self.dir, &store)
    }
}
