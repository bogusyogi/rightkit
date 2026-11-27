// The shell tool's change record, used when the host has turned on `set_shell_change_diff`.
//
// Before a command runs, the workspace's files are read into a manifest (size, modified time and
// content up to a bound). Afterwards the workspace is compared with it: every file the command
// changed, created or deleted gets a restore point in the session's history (so a rewind puts it
// back, exactly, whatever its encoding), and a bounded unified diff of the changes is appended to
// the result. With the option off none of this runs.

/// The hidden per-workspace state directory the history lives in (the same one the edit tools use):
/// `rightkit_agent_config::state_dir_name`.
fn change_state_dir() -> &'static str {
    rightkit_agent_config::state_dir_name()
}
const MANIFEST_MAX_FILES: usize = 20_000;
const MANIFEST_MAX_FILE_BYTES: u64 = 1024 * 1024;
const MANIFEST_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// What a bounded read of a file found.
enum BoundedRead {
    /// Missing, or could not be read.
    Gone,
    Bytes(Vec<u8>),
    /// More than the bound: nothing is held.
    TooLarge,
}

/// Reads at most `MANIFEST_MAX_FILE_BYTES` of `path`; a larger file is never read into memory.
fn read_bounded(path: &std::path::Path) -> BoundedRead {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return BoundedRead::Gone;
    };
    let mut bytes = Vec::new();
    if file
        .take(MANIFEST_MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return BoundedRead::Gone;
    }
    if bytes.len() as u64 > MANIFEST_MAX_FILE_BYTES {
        BoundedRead::TooLarge
    } else {
        BoundedRead::Bytes(bytes)
    }
}

/// The workspace's files (size, modified time, and content up to a bound) before a shell
/// command ran. Build output, dependency folders, version-control internals and the host's state
/// directory are not tracked.
struct WorkspaceManifest {
    files: std::collections::BTreeMap<std::path::PathBuf, ManifestEntry>,
}

struct ManifestEntry {
    len: u64,
    modified: Option<std::time::SystemTime>,
    /// `None` when the file was above the size bound; such a file cannot be restored.
    content: Option<Vec<u8>>,
}

impl WorkspaceManifest {
    fn walk(
        workspace: &std::path::Path,
        mut visit: impl FnMut(std::path::PathBuf, &std::fs::Metadata) -> bool,
    ) {
        let walker = ignore::WalkBuilder::new(workspace)
            .hidden(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|entry| {
                let name = entry.file_name().to_str();
                !(name == Some(change_state_dir())
                    || matches!(
                        name,
                        Some(".git" | "node_modules" | "target" | ".venv" | "__pycache__")
                    ))
            })
            .build();
        let mut seen = 0usize;
        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            seen += 1;
            if seen > MANIFEST_MAX_FILES || !visit(entry.into_path(), &meta) {
                break;
            }
        }
    }

    fn take(workspace: &std::path::Path) -> Self {
        let mut files = std::collections::BTreeMap::new();
        let mut total = 0u64;
        Self::walk(workspace, |path, meta| {
            let content = (meta.len() <= MANIFEST_MAX_FILE_BYTES
                && total.saturating_add(meta.len()) <= MANIFEST_MAX_TOTAL_BYTES)
                .then(|| match read_bounded(&path) {
                    BoundedRead::Bytes(bytes) => Some(bytes),
                    _ => None,
                })
                .flatten();
            total = total.saturating_add(content.as_ref().map_or(0, |c| c.len() as u64));
            files.insert(
                path,
                ManifestEntry {
                    len: meta.len(),
                    modified: meta.modified().ok(),
                    content,
                },
            );
            true
        });
        Self { files }
    }

    /// Write a restore point (pre-command state and post-command record) for every file the
    /// command changed, created or deleted. Reports the paths that could not be recorded and a
    /// bounded text diff of the changes.
    fn record_changes(&self, workspace: &std::path::Path, session_id: &str) -> ChangeReport {
        let mut after: std::collections::BTreeSet<std::path::PathBuf> = Default::default();
        let mut changed: Vec<std::path::PathBuf> = Vec::new();
        Self::walk(workspace, |path, meta| {
            let differs = match self.files.get(&path) {
                None => true,
                Some(old) => {
                    (old.len != meta.len() || old.modified != meta.modified().ok())
                        && match &old.content {
                            // Too large to have been held: changed unless it cannot be read now.
                            None => std::fs::File::open(&path).is_ok(),
                            Some(before) => match read_bounded(&path) {
                                BoundedRead::Bytes(now) => now != *before,
                                BoundedRead::TooLarge => true,
                                BoundedRead::Gone => true,
                            },
                        }
                }
            };
            if differs {
                changed.push(path.clone());
            }
            after.insert(path);
            true
        });
        for path in self.files.keys() {
            if !after.contains(path) && !path.exists() {
                changed.push(path.clone());
            }
        }
        if changed.is_empty() {
            return ChangeReport::default();
        }
        let store = rightkit_agent_engine_history::HistoryStore::new(
            workspace,
            change_state_dir(),
            session_id,
        );
        let mut failed = Vec::new();
        let mut diff = DiffBudget::default();
        let name = |path: &std::path::Path| {
            path.strip_prefix(workspace)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        for path in changed {
            let before_bytes = self
                .files
                .get(&path)
                .and_then(|entry| entry.content.as_deref());
            let existed_before = self.files.contains_key(&path);
            let (after_bytes, after_too_large) = match read_bounded(&path) {
                BoundedRead::Bytes(bytes) => (Some(bytes), false),
                BoundedRead::TooLarge => (None, true),
                BoundedRead::Gone => (None, false),
            };
            diff.add(
                &name(&path),
                existed_before,
                before_bytes,
                after_bytes.as_deref(),
                after_too_large,
            );
            let ok = if !store.can_record(&path) {
                // The history slot of this path is shared with another file's: recording here
                // would let a rewind write one file's content over the other.
                Err(std::io::Error::other(
                    "history slot belongs to another path",
                ))
            } else {
                match self.files.get(&path) {
                    Some(ManifestEntry {
                        content: Some(bytes),
                        ..
                    }) => store.snapshot_bytes(&path, bytes),
                    // Too large to have been held: no restore point is possible.
                    Some(_) => Err(std::io::Error::other("file too large to snapshot")),
                    None => store.snapshot_absent(&path),
                }
            };
            let recorded = ok.and_then(|()| {
                if after_too_large {
                    store.record_after_file(&path)
                } else {
                    store.record_after_bytes(&path, after_bytes.as_deref())
                }
            });
            if recorded.is_err() {
                failed.push(name(&path));
            }
        }
        ChangeReport {
            failed,
            diff: diff.render(),
        }
    }
}

/// What one command changed: the files with no restore point and the text diff to show the model.
#[derive(Default)]
struct ChangeReport {
    failed: Vec<String>,
    /// The `changed by this command` notice and the per-file unified diffs; empty when nothing
    /// text-like changed.
    diff: String,
}

/// Most files whose diff a result carries, and most diff bytes in all.
const DIFF_MAX_FILES: usize = 5;
const DIFF_MAX_BYTES: usize = 4096;
/// A file pair whose line counts multiply past this is named but not diffed (the line diff is
/// quadratic).
const DIFF_MAX_LINE_PRODUCT: usize = 4_000_000;

/// Collects the per-file diffs of one command under the file and byte caps.
#[derive(Default)]
struct DiffBudget {
    shown: Vec<String>,
    omitted: Vec<String>,
    bytes: usize,
}

impl DiffBudget {
    fn add(
        &mut self,
        name: &str,
        existed_before: bool,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
        after_too_large: bool,
    ) {
        // Credential-style files are named, never quoted back to the model.
        let file_name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if file_name.starts_with(".env") {
            self.omitted.push(format!("{name} (not shown)"));
            return;
        }
        // A file too large to have been held, before or after, cannot be diffed.
        if (existed_before && before.is_none()) || after_too_large {
            self.omitted.push(format!("{name} (too large to diff)"));
            return;
        }
        let as_text = |bytes: Option<&[u8]>| -> Option<String> {
            match bytes {
                None => Some(String::new()),
                Some(bytes) if bytes.contains(&0) => None,
                Some(bytes) => String::from_utf8(bytes.to_vec()).ok(),
            }
        };
        let (Some(old), Some(new)) = (as_text(before), as_text(after)) else {
            self.omitted.push(format!("{name} (binary)"));
            return;
        };
        if old.lines().count().saturating_mul(new.lines().count()) > DIFF_MAX_LINE_PRODUCT {
            self.omitted.push(format!("{name} (too large to diff)"));
            return;
        }
        let text = rightkit_agent_engine_history::unified_diff(&old, &new, name);
        if text.is_empty() {
            return;
        }
        if self.shown.len() >= DIFF_MAX_FILES || self.bytes + text.len() > DIFF_MAX_BYTES {
            self.omitted.push(format!("{name} (not shown)"));
            return;
        }
        self.bytes += text.len();
        self.shown.push(text);
    }

    fn render(self) -> String {
        if self.shown.is_empty() && self.omitted.is_empty() {
            return String::new();
        }
        let mut out = format!("[{}: changed by this command]\n", shell_notice_label());
        for text in &self.shown {
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
        if !self.omitted.is_empty() {
            out.push_str(&format!("also changed: {}\n", self.omitted.join(", ")));
        }
        out
    }
}

/// A background command's pre-command workspace state, waiting for the command to exit or the
/// turn to end.
#[cfg(feature = "host")]
struct BackgroundTracker {
    shell_id: String,
    workspace: std::path::PathBuf,
    before: std::sync::Arc<WorkspaceManifest>,
    store: crate::background_shell::BgStore,
    settled: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(feature = "host")]
impl BackgroundTracker {
    fn still_running(&self) -> bool {
        self.store
            .lock()
            .ok()
            .and_then(|shells| shells.get(&self.shell_id).cloned())
            .is_some_and(|shell| shell.status() == crate::background_shell::BgStatus::Running)
    }
}

#[cfg(feature = "host")]
fn background_trackers(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Vec<BackgroundTracker>>> {
    static TRACKERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Vec<BackgroundTracker>>>,
    > = std::sync::OnceLock::new();
    TRACKERS.get_or_init(Default::default)
}

/// Record the changes of tracked background commands through the same history store a foreground
/// command uses. With `keep_running`, a command that is still running stays tracked after its
/// changes so far are recorded: a fast turn can end (and a rewind can be asked for) before the
/// command has written anything, and what it writes later must still get a restore point.
#[cfg(feature = "host")]
async fn settle_background_tracker(session_id: &str, shell_id: Option<&str>, keep_running: bool) {
    let taken: Vec<BackgroundTracker> = {
        let mut all = background_trackers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(list) = all.get_mut(session_id) else {
            return;
        };
        let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(list)
            .into_iter()
            .partition(|tracker| shell_id.is_none_or(|id| tracker.shell_id == id));
        *list = kept;
        if list.is_empty() {
            all.remove(session_id);
        }
        taken
    };
    for tracker in taken {
        let session = session_id.to_string();
        let (workspace, before) = (tracker.workspace.clone(), tracker.before.clone());
        let failed =
            tokio::task::spawn_blocking(move || before.record_changes(&workspace, &session))
                .await
                .unwrap_or_default()
                .failed;
        if !failed.is_empty() {
            tracing::warn!(session_id, files = ?failed, "background command: no restore point for some files");
        }
        if keep_running && tracker.still_running() {
            background_trackers()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(session_id.to_string())
                .or_default()
                .push(tracker);
        } else {
            tracker.settled.notify_one();
        }
    }
}

#[cfg(feature = "host")]
fn watch_background_changes(
    session_id: String,
    workspace: std::path::PathBuf,
    shell_id: String,
    store: crate::background_shell::BgStore,
    before: WorkspaceManifest,
) {
    let settled = std::sync::Arc::new(tokio::sync::Notify::new());
    background_trackers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(session_id.clone())
        .or_default()
        .push(BackgroundTracker {
            shell_id: shell_id.clone(),
            workspace,
            before: std::sync::Arc::new(before),
            store: store.clone(),
            settled: settled.clone(),
        });
    tokio::spawn(async move {
        let mut events = crate::background_shell::subscribe_shell_events();
        loop {
            let running = store
                .lock()
                .ok()
                .and_then(|shells| shells.get(&shell_id).cloned())
                .is_some_and(|shell| shell.status() == crate::background_shell::BgStatus::Running);
            if !running {
                settle_background_tracker(&session_id, Some(&shell_id), false).await;
                return;
            }
            tokio::select! {
                changed = events.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                // The turn ended first and recorded this command's changes so far.
                _ = settled.notified() => return,
            }
        }
    });
}

/// With `set_shell_change_diff`: the turn ended, or a rewind was asked for. Records what every
/// background command of the session has changed so far; a command that is still running stays
/// tracked until it exits. A host calls this before it rewinds. Does nothing for a session with
/// no tracked command.
#[cfg(feature = "host")]
pub async fn settle_background_file_tracking(session_id: &str) {
    settle_background_tracker(session_id, None, true).await;
}

/// With `set_shell_change_diff`: runs the command with the workspace read before and compared
/// after (see the top of this file), so a rewind restores what it changed and the result shows the
/// change. A command run in the background changes files after this call returns: the workspace is
/// read now and the changes are recorded when the command exits or `settle_background_file_tracking`
/// is called, whichever comes first.
pub(crate) async fn execute_shell_tracked(
    args: Value,
    ctx: &ToolContext,
    flavor: ShellFlavor,
) -> ToolResult {
    let background = args["run_in_background"].as_bool().unwrap_or(false);
    let track =
        !ctx.workspace_root.as_os_str().is_empty() && !ctx.session_id.is_empty() && !background;
    #[cfg(feature = "host")]
    {
        let track_background = !ctx.workspace_root.as_os_str().is_empty()
            && !ctx.session_id.is_empty()
            && background
            && ctx.background_shells.is_some();
        if track_background {
            let root = ctx.workspace_root.clone();
            let before = tokio::task::spawn_blocking(move || WorkspaceManifest::take(&root))
                .await
                .ok();
            let result = execute_shell_protected(args, ctx, flavor).await;
            if let (Some(before), false, Some(store)) =
                (before, result.is_error, ctx.background_shells.clone())
            {
                if let Some(shell_id) = result
                    .content
                    .strip_prefix("Started background shell '")
                    .and_then(|rest| rest.split('\'').next())
                {
                    watch_background_changes(
                        ctx.session_id.clone(),
                        ctx.workspace_root.clone(),
                        shell_id.to_string(),
                        store,
                        before,
                    );
                }
            }
            return result;
        }
    }
    let before = if track {
        let root = ctx.workspace_root.clone();
        tokio::task::spawn_blocking(move || WorkspaceManifest::take(&root))
            .await
            .ok()
    } else {
        None
    };
    let result = execute_shell_protected(args, ctx, flavor).await;
    let Some(before) = before else {
        return result;
    };
    let root = ctx.workspace_root.clone();
    let session = ctx.session_id.clone();
    let report = tokio::task::spawn_blocking(move || before.record_changes(&root, &session))
        .await
        .unwrap_or_default();
    let ChangeReport { failed, diff } = report;
    let mut result = result;
    if !diff.is_empty() {
        result.content = format!("{}\n\n{}", result.content, diff.trim_end());
    }
    if failed.is_empty() {
        return result;
    }
    result.content = format!(
        "{}\n\n[{}: no restore point could be stored for {}; a rewind will not restore {}.]",
        result.content,
        shell_notice_label(),
        failed.join(", "),
        if failed.len() == 1 { "it" } else { "them" }
    );
    result
}
