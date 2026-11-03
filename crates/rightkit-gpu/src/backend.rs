//! Which wgpu backends an app lets wgpu use, for its compute device and for its window, and the
//! crash sentinel around device creation (issue #136). Adapted from Ember `crates/gpu`.
//!
//! wgpu loads the system driver of *every* backend in the instance's set while it enumerates
//! adapters, even when it then picks another one: with Vulkan in the set, a broken Vulkan driver
//! can crash the process (an access violation inside the driver DLL, which nothing can catch)
//! although DX12 would have been used. So Windows defaults to DX12 alone. macOS uses Metal, Linux
//! Vulkan (plus GL for a window when the build has it).
//!
//! Overrides, read from the environment each call: the app's own variable (its name is passed as
//! `app_var`, for example `EMBER_GPU_BACKEND`) = `dx12` | `vulkan` | `metal` | `gl` (comma lists
//! allowed), `auto` (the platform default), or `off` (no GPU compute; the window still needs one
//! backend and keeps the platform default); else `WGPU_BACKEND` (wgpu's own variable, same names)
//! for both the window and compute.
//!
//! Backends this build doesn't contain (for example `gl`) are ignored with a warning and the
//! platform default is used.

use std::path::PathBuf;
use std::sync::Mutex;

pub use wgpu::Backends;

/// What the environment asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    /// The platform default.
    Auto,
    /// No GPU compute.
    Off,
    /// These backends.
    Use(Backends),
}

/// Parse a backend list (`"dx12"`, `"vulkan,gl"`, `"auto"`, `"off"`…). `None`: nothing usable in
/// it (empty or unknown names).
pub fn parse_backends(s: &str) -> Option<BackendChoice> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => None,
        "auto" | "default" => Some(BackendChoice::Auto),
        "off" | "none" | "cpu" | "0" | "false" | "no" => Some(BackendChoice::Off),
        // the window's OpenGL renderer (see `WindowRenderer`): compute keeps its default
        "glow" => Some(BackendChoice::Auto),
        _ => {
            let b = Backends::from_comma_list(&s);
            (!b.is_empty()).then_some(BackendChoice::Use(b))
        }
    }
}

/// The choice from the app's variable `app_var` (value `app`) or else `WGPU_BACKEND` (`wgpu`): the
/// first one that parses wins; an unknown value is skipped (with a warning).
pub fn choose(app_var: &str, app: Option<&str>, wgpu: Option<&str>) -> BackendChoice {
    for (name, v) in [(app_var, app), ("WGPU_BACKEND", wgpu)] {
        let Some(v) = v else { continue };
        match parse_backends(v) {
            // WGPU_BACKEND is wgpu's variable: it selects backends, it never turns the GPU off
            Some(BackendChoice::Off) if name == "WGPU_BACKEND" => {
                log::warn!("gpu: WGPU_BACKEND={v} ignored (use {app_var}=off)")
            }
            Some(c) => return c,
            None => log::warn!("gpu: {name}={v} names no known backend; ignored"),
        }
    }
    BackendChoice::Auto
}

/// The backends compute devices use when nothing is asked for on `os` (`std::env::consts::OS`).
pub fn default_compute_backends(os: &str) -> Backends {
    match os {
        // never touch the Vulkan driver unless asked: see the module docs (issue #136)
        "windows" => Backends::DX12,
        "macos" | "ios" => Backends::METAL,
        _ => Backends::PRIMARY,
    }
}

/// The backends the window uses when nothing is asked for on `os`.
pub fn default_window_backends(os: &str) -> Backends {
    match os {
        "windows" => Backends::DX12,
        "macos" | "ios" => Backends::METAL,
        // eframe's default (GL only when the build has it)
        _ => Backends::PRIMARY | Backends::GL,
    }
}

/// Resolve `choice` against the backends this build contains (`compiled`): `None` = GPU off.
/// Asked-for backends that aren't compiled in fall back to `default`.
pub fn resolve(choice: BackendChoice, default: Backends, compiled: Backends) -> Option<Backends> {
    match choice {
        BackendChoice::Off => None,
        BackendChoice::Auto => Some(default),
        BackendChoice::Use(b) if b.intersects(compiled) => Some(b & compiled),
        BackendChoice::Use(b) => {
            log::warn!("gpu: backend {b:?} is not in this build ({compiled:?}); using {default:?}");
            Some(default)
        }
    }
}

fn env_choice(app_var: &str) -> BackendChoice {
    let app = std::env::var(app_var).ok();
    let wgpu = std::env::var("WGPU_BACKEND").ok();
    choose(app_var, app.as_deref(), wgpu.as_deref())
}

/// The backends of the compute device (`None`: the app's variable is `off`).
pub fn compute_backends(app_var: &str) -> Option<Backends> {
    resolve(
        env_choice(app_var),
        default_compute_backends(std::env::consts::OS),
        wgpu::Instance::enabled_backend_features(),
    )
}

/// The backends for a desktop window's renderer. The window always needs one: `off` keeps the
/// platform default.
pub fn window_backends(app_var: &str) -> Backends {
    let default = default_window_backends(std::env::consts::OS);
    let choice = match env_choice(app_var) {
        BackendChoice::Off => BackendChoice::Auto,
        c => c,
    };
    resolve(choice, default, wgpu::Instance::enabled_backend_features()).unwrap_or(default)
}

/// The DX12 shader compiler: FXC (`d3dcompiler_47.dll`, part of Windows) unless
/// `WGPU_DX12_COMPILER` (`wgpu`, wgpu's own variable: `fxc`, `dxc`, `auto`…) names another.
///
/// Issue #471: wgpu's default (`Auto`) loads whichever `dxcompiler.dll` the DLL search path finds
/// first. An app ships none, so it is some other program's (an SDK's, a folder on `PATH`). A
/// copy without its `dxil.dll` beside it warns that the DXIL is unsigned; wgpu takes the warning for
/// a compile error, its own validation pipelines fail, the device is lost and the window never
/// opens. FXC is always there and compiles what the apps' shaders need.
pub fn dx12_compiler(env: Option<&str>) -> wgpu::Dx12Compiler {
    let Some(v) = env else {
        return wgpu::Dx12Compiler::Fxc;
    };
    v.parse().unwrap_or_else(|e| {
        log::warn!("gpu: WGPU_DX12_COMPILER={v} ignored: {e}");
        wgpu::Dx12Compiler::Fxc
    })
}

/// wgpu's backend options for every instance an app creates — the window's and the compute
/// devices': wgpu's environment variables, with the DX12 compiler from [`dx12_compiler`].
pub fn backend_options() -> wgpu::BackendOptions {
    let mut o = wgpu::BackendOptions::from_env_or_default();
    o.dx12.shader_compiler = dx12_compiler(std::env::var("WGPU_DX12_COMPILER").ok().as_deref());
    o
}

/// wgpu's instance flags for every instance an app creates, from `flags` (wgpu's defaults with its
/// environment variables applied) without `VALIDATION_INDIRECT_CALL`, unless
/// `WGPU_VALIDATION_INDIRECT_CALL` asks for it (any value but `0`). That validation builds an internal
/// compute pipeline when each device is created; Intel Macs' Metal compiler fails on it ("Compiler
/// encountered an internal error"), the device is lost and the window never opens (#250). It only
/// guards indirect draws and dispatches, which the apps do not issue.
pub fn instance_flags(flags: wgpu::InstanceFlags, env: Option<&str>) -> wgpu::InstanceFlags {
    if env.is_some_and(|v| v != "0") {
        flags | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    } else {
        flags - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    }
}

/// [`instance_flags`] with `WGPU_VALIDATION_INDIRECT_CALL` read from the environment.
pub fn instance_flags_from_env(flags: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    instance_flags(
        flags,
        std::env::var("WGPU_VALIDATION_INDIRECT_CALL")
            .ok()
            .as_deref(),
    )
}

/// Whether the app's variable `app_var` is `off`.
pub fn env_off(app_var: &str) -> bool {
    env_choice(app_var) == BackendChoice::Off
}

#[derive(Default)]
struct MarkerState {
    path: Option<PathBuf>,
    startup_active: bool,
}

/// One tracker per process.
#[derive(Default)]
struct MarkerTracker {
    state: Mutex<MarkerState>,
    active: Mutex<u32>,
}

static MARKER: MarkerTracker = MarkerTracker {
    state: Mutex::new(MarkerState {
        path: None,
        startup_active: false,
    }),
    active: Mutex::new(0),
};

impl MarkerTracker {
    fn set_init_marker(&self, path: Option<PathBuf>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let old = state.path.clone();
        let clear_old = path.is_none();
        let same_path = old == path;
        state.path = path;
        if !same_path && state.startup_active {
            if let Some(old) = old.as_deref() {
                disarm(old, &self.active);
            }
            state.startup_active = false;
        }
        drop(state);
        if clear_old && let Some(old) = old {
            let _ = std::fs::remove_file(old);
        }
    }

    fn begin_startup_marker(&self, backends: Backends) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(path) = state.path.as_deref() else {
            return false;
        };
        if !state.startup_active {
            arm(
                path,
                &self.active,
                &format!("startup (backends {backends:?})"),
            );
            state.startup_active = true;
        }
        true
    }

    fn startup_succeeded(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.startup_active {
            if let Some(path) = state.path.as_deref() {
                disarm(path, &self.active);
            }
            state.startup_active = false;
        }
    }

    fn with_marker<T>(&self, what: &str, f: impl FnOnce() -> T) -> T {
        let marker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .path
            .clone();
        if let Some(m) = &marker {
            arm(m, &self.active, what);
        }
        let _armed = Armed {
            marker,
            active: &self.active,
        };
        f()
    }
}

/// Configure the marker used around compute device creation and native startup. `None` (the
/// default) writes nothing.
pub fn set_init_marker(path: Option<PathBuf>) {
    MARKER.set_init_marker(path);
}

/// Begin guarding native window startup and its first frame with the same marker used by GPU
/// device creation. A process terminated by a driver while this is active leaves the marker for
/// the next launch to recover from. Returns whether a marker path is configured.
pub fn begin_startup_marker(backends: Backends) -> bool {
    MARKER.begin_startup_marker(backends)
}

/// Finish a successful startup guard without clearing concurrent driver-work scopes.
pub fn startup_succeeded() {
    MARKER.startup_succeeded();
}

/// Finish a startup guard after a recoverable window creation error.
pub fn startup_failed() {
    startup_succeeded();
}

/// The marker's text: what started, and when.
fn marker_text(what: &str, secs: u64) -> String {
    format!("GPU {what} started (unix time {secs})\n")
}

/// Enter a driver-work scope on `marker`: the first one writes the file.
fn arm(marker: &std::path::Path, active: &Mutex<u32>, what: &str) {
    let mut n = active.lock().unwrap_or_else(|e| e.into_inner());
    if *n == 0 {
        if let Some(d) = marker.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = rightkit_fs::atomic_write(marker, marker_text(what, secs).as_bytes());
    }
    *n = n.saturating_add(1);
}

/// Leave a scope: the last one removes the file.
fn disarm(marker: &std::path::Path, active: &Mutex<u32>) {
    let mut n = active.lock().unwrap_or_else(|e| e.into_inner());
    *n = n.saturating_sub(1);
    if *n == 0 {
        let _ = std::fs::remove_file(marker);
    }
}

/// Removes the marker on the way out, a panic included (the process survived).
struct Armed<'a> {
    marker: Option<PathBuf>,
    active: &'a Mutex<u32>,
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        if let Some(m) = &self.marker {
            disarm(m, self.active);
        }
    }
}

/// Run driver work `f` (`what`: "device creation", "kernel compilation"…) between writing and
/// removing the init marker (if one is set).
pub fn with_marker<T>(what: &str, f: impl FnOnce() -> T) -> T {
    MARKER.with_marker(what, f)
}

/// Run device creation (and the kernel build that follows it) `f` under the init marker.
pub fn with_init_marker<T>(backends: Backends, f: impl FnOnce() -> T) -> T {
    with_marker(
        &format!("device and kernel creation (backends {backends:?})"),
        f,
    )
}

/// If the init marker `path` is present — the last process died while creating the GPU device —
/// remove it and return what it recorded.
pub fn take_init_marker(path: &std::path::Path) -> Option<String> {
    let text = read_init_marker(path)?;
    let _ = std::fs::remove_file(path);
    Some(text)
}

/// What the init marker `path` recorded, if it is present — without removing it. For sessions
/// that must leave the disk as they found it (a memory-only session): GPU rendering
/// still starts off, and the marker stays for the next ordinary launch to report and clear.
pub fn read_init_marker(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string())
}
