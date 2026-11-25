//! Small pieces of the background work behind an Apps page: a one-at-a-time flag per job, and the
//! icon queue that returns cached icons at once and renders the rest on a few worker threads.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Locks a mutex, recovering the data when another thread panicked while holding it.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Holds a job's busy flag; clears it when dropped, even if the job panics.
pub struct BusyGuard(&'static AtomicBool);

impl BusyGuard {
    /// Takes `flag` for one job. `None` when a job already holds it (the caller does nothing: the
    /// running job reports by event).
    pub fn try_begin(flag: &'static AtomicBool) -> Option<BusyGuard> {
        if flag.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(BusyGuard(flag))
        }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The most icon requests handled in one call.
pub const ICON_BATCH: usize = 500;
/// The largest icon PNG sent to a page.
pub const ICON_MAX_BYTES: usize = 512 * 1024;
/// How many icons are rendered at once in the background.
pub const ICON_WORKERS: usize = 2;

/// A cached icon PNG's bytes, when the file reads and is no larger than [`ICON_MAX_BYTES`].
pub fn read_icon_png(png: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(png).ok()?;
    (bytes.len() <= ICON_MAX_BYTES).then_some(bytes)
}

/// The app paths whose icon is being rendered now, so a second request for one does not start a
/// second render.
#[derive(Default)]
pub struct IconQueue {
    pending: Mutex<HashSet<String>>,
}

impl IconQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// From the first [`ICON_BATCH`] of `paths`, the ones `cached` answers `false` for (it is
    /// called once per path, and is where the caller takes a cached icon) and no render is
    /// running for yet; each is now marked as being rendered.
    pub fn claim(&self, paths: Vec<String>, mut cached: impl FnMut(&str) -> bool) -> Vec<String> {
        let missing: Vec<String> = paths
            .into_iter()
            .take(ICON_BATCH)
            .filter(|path| !cached(path))
            .collect();
        let mut set = lock(&self.pending);
        missing
            .into_iter()
            .filter(|path| set.insert(path.clone()))
            .collect()
    }

    /// Renders every claimed path on [`ICON_WORKERS`] threads and waits for them. `render` makes
    /// one icon (`None` when the app has no usable icon); `done` gets the path and the result
    /// after the path is no longer marked as being rendered.
    pub fn render_all<T>(
        &self,
        claimed: Vec<String>,
        render: impl Fn(&str) -> Option<T> + Sync,
        done: impl Fn(String, Option<T>) + Sync,
    ) {
        let queue = Mutex::new(claimed.into_iter());
        std::thread::scope(|scope| {
            for _ in 0..ICON_WORKERS {
                scope.spawn(|| {
                    loop {
                        let next = lock(&queue).next();
                        let Some(path) = next else {
                            break;
                        };
                        let icon = render(&path);
                        lock(&self.pending).remove(&path);
                        done(path, icon);
                    }
                });
            }
        });
    }
}

/// One process's listening ports and open files, on demand only. The identity is checked first,
/// as Quit checks it, so a recycled pid is refused rather than inspected.
pub fn inspect_process(
    pid: u32,
    start_time: u64,
) -> Result<rightkit_system_monitor::process_inspect::ProcessInspection, String> {
    use rightkit_system_monitor::process_inspect::{inspect, live_identity};
    match live_identity(pid) {
        None => Err("That process has already exited.".to_string()),
        Some((current, _)) if current != start_time => {
            Err("That process was replaced by a different one; nothing was inspected.".to_string())
        }
        Some(_) => Ok(inspect(pid)),
    }
}

/// The last path segment without its extension, for a name in a message ("an app" when there is
/// none).
pub fn app_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("an app")
        .to_string()
}
