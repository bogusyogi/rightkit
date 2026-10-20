//! File paths the OS hands the app at launch, from a second launch, or (macOS) as `file://` URLs
//! of `RunEvent::Opened`, delivered to the renderer only after the path broker granted them.
//!
//! Donor: ViewRight's `PendingOpenPaths` (`commands/dialogs.rs`) and its second-instance path
//! filter, with two changes: only existing regular files survive the filter, and the renderer never
//! receives a raw path. A path on the command line (`app.exe C:\docs\a.docx`, `app /tmp/a.md`) is
//! queued until the renderer calls [`rk_shell_open_paths_ready`], which grants every queued file
//! through [`crate::PathBroker::grant_file`] (canonical path, regular file) and returns the granted
//! display paths. After that, later paths (a second launch, a macOS open-file event) are granted and
//! emitted as [`EVENT_OPEN_PATHS`] `{ paths }` to the main webview only.
//!
//! The gate has the same shape as the deep-link queue (`rk_shell_ready` flushes
//! [`crate::deep_link::DeepLinkQueue`]): paths that arrive before the renderer listens wait, in
//! arrival order, and are handed over exactly once. The queue holds at most [`MAX_QUEUED`] paths; the
//! oldest are kept and extra ones dropped. A renderer that reloads and calls ready again receives
//! only paths that arrived since.
//!
//! The filter keeps path-shaped arguments only: flags (`-psn_...`, `--background`), URLs (those go
//! through [`crate::open_url`] and deep links), empty or control-character arguments are dropped, a
//! relative path joins the working directory of the process that was given it (the second launch's
//! `cwd`, never the primary's), and what is left must be an existing regular file. The file can still
//! disappear before the grant; the grant step then skips it.
//!
//! The app installs the queue with [`install`] during plugin setup (the shell does this when
//! wired); without it every function here is a quiet no-op and the command answers `unsupported`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Webview, Wry};

use crate::broker::PathBroker;
use crate::error::ShellError;
use crate::{Module, ShellState};

/// File paths from launch argv, a second launch or an OS open-file event, once the renderer called
/// [`rk_shell_open_paths_ready`]. Payload `{ "paths": [granted display paths] }`.
pub const EVENT_OPEN_PATHS: &str = "rightkit-shell://open-paths";

/// Most paths held while waiting for the renderer; the oldest are kept, extra ones are dropped.
pub const MAX_QUEUED: usize = 256;
const MAX_PATH_LEN: usize = 32_768;

/// True when `arg` can be a file path argument: not a flag, not a URL, no control characters.
pub fn is_path_arg(arg: &str) -> bool {
    let a = arg.trim();
    !a.is_empty()
        && a.len() <= MAX_PATH_LEN
        && !a.starts_with('-')
        && !a.chars().any(char::is_control)
        && !crate::open_url::is_url(a)
}

/// The path-shaped entries of `args` as absolute candidates, in order and without repeats. The
/// program name is the caller's to skip. A relative entry joins `base` (the working directory of the
/// launch that supplied it); with no absolute `base` a relative entry is dropped, so it can never
/// resolve against an unrelated directory. Nothing is checked on disk here.
pub fn candidate_paths<I, S>(args: I, base: Option<&Path>) -> Vec<PathBuf>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    let base = base.filter(|b| b.is_absolute());
    let mut out: Vec<PathBuf> = Vec::new();
    for arg in args {
        let arg: std::ffi::OsString = arg.into();
        if !is_path_arg(&arg.to_string_lossy()) {
            continue;
        }
        let path = PathBuf::from(arg);
        let path = if path.is_absolute() {
            path
        } else if let Some(base) = base {
            base.join(path)
        } else {
            continue;
        };
        // `base.join("C:rel")` on Windows can still yield a drive-relative path.
        if path.is_absolute() && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Keep the entries that are existing regular files (symlinks are followed, directories dropped).
pub fn keep_regular_files(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().filter(|p| p.is_file()).collect()
}

/// The existing regular files named by `args` (see [`candidate_paths`]).
pub fn file_paths_from_args<I, S>(args: I, base: Option<&Path>) -> Vec<PathBuf>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    keep_regular_files(candidate_paths(args, base))
}

/// The existing regular files on a forwarded second-launch `argv` (`argv[0]`, the executable, is
/// skipped). A relative path joins that launch's `cwd`; an empty or relative `cwd` drops them.
pub fn file_paths_from_argv(argv: &[String], cwd: &str) -> Vec<PathBuf> {
    let base = (!cwd.is_empty()).then(|| Path::new(cwd));
    file_paths_from_args(argv.iter().skip(1).map(String::as_str), base)
}

/// The existing regular files on this process's own command line.
pub fn file_paths_from_launch() -> Vec<PathBuf> {
    let base = std::env::current_dir().ok();
    file_paths_from_args(std::env::args_os().skip(1), base.as_deref())
}

/// `RunEvent::Opened` URLs split into local files and everything else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenedUrls {
    /// Local paths of `file://` URLs (not yet checked on disk).
    pub files: Vec<PathBuf>,
    /// The other URLs, as text, for the open-URL hub and deep links.
    pub links: Vec<String>,
}

/// Route OS-opened URLs: a `file:` URL becomes a path for [`deliver_candidates`]; any other scheme
/// stays a link. A `file:` URL that does not name a local path (a remote host, a bad escape) is
/// dropped from both lists: it is neither a file the app can open nor a link it should receive.
pub fn split_opened_urls<'a>(urls: impl IntoIterator<Item = &'a tauri::Url>) -> OpenedUrls {
    let mut out = OpenedUrls::default();
    for url in urls {
        if url.scheme() == "file" {
            if let Ok(path) = url.to_file_path() {
                if !out.files.contains(&path) {
                    out.files.push(path);
                }
            }
        } else {
            out.links.push(url.to_string());
        }
    }
    out
}

#[derive(Default)]
struct Gate {
    paths: Vec<PathBuf>,
    renderer_ready: bool,
}

/// Paths queued until the renderer is ready.
#[derive(Default)]
pub struct PendingOpenPaths(Mutex<Gate>);

impl PendingOpenPaths {
    /// Empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from this process's own command line (existing regular files only).
    pub fn from_launch() -> Self {
        Self::from_paths(file_paths_from_launch())
    }

    /// Seed with `paths` as already filtered by the caller (bounded to [`MAX_QUEUED`]).
    pub fn from_paths(mut paths: Vec<PathBuf>) -> Self {
        paths.truncate(MAX_QUEUED);
        Self(Mutex::new(Gate {
            paths,
            renderer_ready: false,
        }))
    }

    fn gate(&self) -> std::sync::MutexGuard<'_, Gate> {
        // The gate is a Vec and a flag: a panic elsewhere cannot leave it half-updated.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The renderer listens: take everything queued, in arrival order, and deliver later paths live.
    pub fn mark_ready_and_drain(&self) -> Vec<PathBuf> {
        let mut gate = self.gate();
        gate.renderer_ready = true;
        std::mem::take(&mut gate.paths)
    }

    /// Ready: hand `paths` back to deliver now. Not ready: queue them (up to [`MAX_QUEUED`]) and
    /// return nothing. The check and the queueing share one lock, so a path is never both queued and
    /// delivered, and never lost between the two.
    pub fn queue_or_take(&self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut gate = self.gate();
        if gate.renderer_ready {
            paths
        } else {
            let room = MAX_QUEUED.saturating_sub(gate.paths.len());
            gate.paths.extend(paths.into_iter().take(room));
            Vec::new()
        }
    }

    /// Number of paths waiting for the renderer.
    pub fn pending(&self) -> usize {
        self.gate().paths.len()
    }

    /// True once [`PendingOpenPaths::mark_ready_and_drain`] ran.
    pub fn is_ready(&self) -> bool {
        self.gate().renderer_ready
    }
}

/// Grant each path through the broker and return the granted display paths, in order. A path the
/// broker refuses (it vanished, is not a regular file, cannot be read) is skipped.
pub fn grant_paths(broker: &PathBroker, paths: Vec<PathBuf>) -> Vec<String> {
    let mut granted: Vec<String> = Vec::with_capacity(paths.len());
    for path in paths {
        if let Ok(display) = broker.grant_file(&path) {
            if !granted.contains(&display) {
                granted.push(display);
            }
        }
    }
    granted
}

/// Create the queue, seeded from this process's command line, as managed state. The shell calls
/// this once during plugin setup, before any webview exists.
pub fn install(app: &AppHandle<Wry>) {
    if app.try_state::<PendingOpenPaths>().is_none() {
        app.manage(PendingOpenPaths::from_launch());
    }
}

/// Queue or emit `paths` that are already known to be existing files (see [`PendingOpenPaths`]).
/// Before the renderer is ready they wait; afterwards they are granted and emitted to the main
/// webview at once. Does nothing when the queue is not installed or the app opted out of the broker
/// (nothing could be granted, and a raw path is never sent instead).
pub fn deliver(app: &AppHandle<Wry>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let (Some(pending), Some(state)) = (
        app.try_state::<PendingOpenPaths>(),
        app.try_state::<ShellState>(),
    ) else {
        return;
    };
    if !state.modules.contains(Module::Broker) {
        return;
    }
    let now = pending.queue_or_take(paths);
    if now.is_empty() {
        return;
    }
    let granted = grant_paths(&state.broker, now);
    if !granted.is_empty() {
        let _ = app.emit_to(
            state.main_label.as_str(),
            EVENT_OPEN_PATHS,
            json!({ "paths": granted }),
        );
    }
}

/// [`deliver`] for paths from a source that did not check them (macOS open-file events): only
/// existing regular files go on.
pub fn deliver_candidates(app: &AppHandle<Wry>, paths: Vec<PathBuf>) {
    deliver(app, keep_regular_files(paths));
}

/// Deliver the file paths on a forwarded second launch's `argv`. The shell calls this for every
/// second launch, including when the app replaces the default behaviour with `on_second_instance`.
pub fn deliver_argv(app: &AppHandle<Wry>, argv: &[String], cwd: &str) {
    deliver(app, file_paths_from_argv(argv, cwd));
}

/// The renderer listens for [`EVENT_OPEN_PATHS`]: hand over the files that arrived first, already
/// granted, and switch to live delivery. Answers the granted display paths (use them with the
/// `rk_shell_path_*` commands); a file that vanished since launch is skipped. Only an app webview
/// may call it ([`crate::caller_gate`]). The grants run off the UI thread.
#[tauri::command]
pub async fn rk_shell_open_paths_ready(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
) -> Result<Vec<String>, ShellError> {
    crate::commands::guard_command(&webview, "rk_shell_open_paths_ready")?;
    crate::caller_gate::require_app_caller(&webview)?;
    let state = app
        .try_state::<ShellState>()
        .ok_or_else(|| ShellError::Unsupported("the shell is not installed".into()))?;
    crate::commands::require(&state, Module::Broker)?;
    let pending = app
        .try_state::<PendingOpenPaths>()
        .ok_or_else(|| ShellError::Unsupported("open paths are not installed".into()))?;
    let queued = pending.mark_ready_and_drain();
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        grant_paths(&handle.state::<ShellState>().broker, queued)
    })
    .await
    .map_err(|e| ShellError::Other(e.to_string()))
}
