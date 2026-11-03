//! Elevation broker for root-owned uninstall moves through a privileged helper.
//!
//! A privileged helper usually accepts connections only from one trusted UI process (checked by the
//! kernel, for example by audit token), so the background process that runs an uninstall cannot
//! speak to it. The broker parks the root-owned paths of an uninstall here; the trusted UI polls
//! [`next`], shows the owner a card naming the app and the exact paths, asks the helper only after
//! the owner confirms, and reports what moved with [`complete`]. Requests carry no authority of
//! their own: the helper still applies its allowed-path policy to every path.
//!
//! Only an uninstall the owner started with `elevate: true` installs an elevator ([`run`]). A
//! request waits at most 90 s for an answer, and none is parked unless the UI polled within the
//! last 30 s, so a stopped UI costs nothing and the uninstall goes straight to the Trash. State
//! lives in this process's memory only.
//!
//! [`next`] takes `{"waitMs": n}` (at most 20 s) and answers `{"request": {"id", "app", "paths"}}` or
//! `{"request": null}`; [`complete`] takes `{"id", "moved": [paths], "declined": bool}`. On Windows
//! there is no helper: [`run`] ignores the flag and the methods refuse.

#[cfg(unix)]
mod imp {
    use std::path::PathBuf;
    use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    /// The trusted UI must have polled this recently for a request to be parked.
    const SERVED_WITHIN: Duration = Duration::from_secs(30);
    /// How long an uninstall waits for the UI's answer (the owner's click) before the Trash fallback takes over.
    const ANSWER_WITHIN: Duration = Duration::from_secs(90);
    /// The longest one [`next`] holds (below `SERVED_WITHIN`).
    const POLL_MAX: Duration = Duration::from_secs(20);

    struct Request {
        id: u64,
        app: String,
        paths: Vec<String>,
        taken: bool,
        moved: Option<Vec<String>>,
    }

    #[derive(Default)]
    struct State {
        next_id: u64,
        last_poll: Option<Instant>,
        requests: Vec<Request>,
    }

    fn state() -> &'static (Mutex<State>, Condvar) {
        static STATE: OnceLock<(Mutex<State>, Condvar)> = OnceLock::new();
        STATE.get_or_init(|| (Mutex::new(State::default()), Condvar::new()))
    }

    fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs an uninstall of the app at `app` with the helper route when `elevate` is true.
    pub fn run<R>(elevate: bool, app: &str, work: impl FnOnce() -> R) -> R {
        if elevate {
            let app = app.to_string();
            rightkit_app_manager::app_manager::with_elevator(
                Box::new(move |paths| request(&app, paths)),
                work,
            )
        } else {
            work()
        }
    }

    /// Parks `paths` for the trusted UI and waits for its answer: the paths it moved, or `None` when the
    /// UI is not serving, the owner declined, no answer came in time or the helper failed.
    fn request(app: &str, paths: &[PathBuf]) -> Option<Vec<PathBuf>> {
        let paths: Vec<String> = paths
            .iter()
            .map(|p| p.to_str().map(str::to_string))
            .collect::<Option<_>>()?;
        let (mutex, signal) = state();
        let mut guard = lock(mutex);
        if guard
            .last_poll
            .is_none_or(|at| at.elapsed() > SERVED_WITHIN)
        {
            return None;
        }
        guard.next_id += 1;
        let id = guard.next_id;
        guard.requests.push(Request {
            id,
            app: app.chars().take(1024).collect(),
            paths,
            taken: false,
            moved: None,
        });
        signal.notify_all();
        let deadline = Instant::now() + ANSWER_WITHIN;
        loop {
            let answered = guard
                .requests
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.moved.clone());
            if let Some(moved) = answered {
                guard.requests.retain(|r| r.id != id);
                return Some(moved.into_iter().map(PathBuf::from).collect());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                guard.requests.retain(|r| r.id != id);
                return None;
            }
            guard = signal
                .wait_timeout(guard, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// [`next`] with optional `waitMs`: hands the oldest request nobody has taken to the
    /// caller (`id`, `app`, `paths`), waiting up to `waitMs` (at most 20 s) for one.
    /// `{"request": null}` when none came.
    pub fn next(params: &Value) -> Result<Value, String> {
        let wait = params
            .get("waitMs")
            .and_then(Value::as_u64)
            .map_or(Duration::ZERO, Duration::from_millis)
            .min(POLL_MAX);
        let (mutex, signal) = state();
        let mut guard = lock(mutex);
        guard.last_poll = Some(Instant::now());
        let deadline = Instant::now() + wait;
        loop {
            if let Some(request) = guard.requests.iter_mut().find(|r| !r.taken) {
                request.taken = true;
                return Ok(json!({ "request": {
                    "id": request.id, "app": request.app, "paths": request.paths,
                } }));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(json!({ "request": null }));
            }
            guard = signal
                .wait_timeout(guard, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            guard.last_poll = Some(Instant::now());
        }
    }

    /// [`complete`] with `id`, `moved` (the paths the helper moved) and `declined` (the
    /// owner chose Cancel or did not answer; nothing was sent to the helper): answers a request the
    /// caller took. Only paths that were in the request count; an unknown, untaken or already
    /// answered id is refused.
    pub fn complete(params: &Value) -> Result<Value, String> {
        let id = params
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("`id` must be a number")?;
        let declined = params
            .get("declined")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let moved: Vec<String> = params
            .get("moved")
            .and_then(Value::as_array)
            .ok_or("`moved` must be a list of paths")?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let (mutex, signal) = state();
        let mut guard = lock(mutex);
        let request = guard
            .requests
            .iter_mut()
            .find(|r| r.id == id && r.taken && r.moved.is_none())
            .ok_or("no such request waiting for an answer")?;
        let kept: Vec<String> = if declined {
            Vec::new()
        } else {
            moved
                .into_iter()
                .filter(|p| request.paths.contains(p))
                .collect()
        };
        request.moved = Some(kept);
        signal.notify_all();
        Ok(json!({ "ok": true }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // One test: the broker state is process-wide, so the sequence runs in order.
        #[test]
        fn requests_park_for_a_polling_ui_and_resolve_with_only_requested_paths() {
            // Nobody has polled: an uninstall is not parked and falls back to the Trash.
            assert_eq!(request("A", &[PathBuf::from("/x")]), None);
            assert_eq!(next(&json!({})).unwrap(), json!({ "request": null }));

            let worker = std::thread::spawn(|| {
                request(
                    "Example",
                    &[PathBuf::from("/Library/a"), PathBuf::from("/Library/b")],
                )
            });
            let taken = next(&json!({ "waitMs": 5000 })).unwrap();
            let id = taken["request"]["id"].as_u64().unwrap();
            assert_eq!(taken["request"]["app"], "Example");
            assert_eq!(
                taken["request"]["paths"],
                json!(["/Library/a", "/Library/b"])
            );
            // A taken request is not handed out twice.
            assert_eq!(next(&json!({})).unwrap(), json!({ "request": null }));
            // Only paths that were in the request count.
            assert_eq!(
                complete(&json!({ "id": id, "moved": ["/Library/a", "/etc/passwd"] })).unwrap(),
                json!({ "ok": true })
            );
            assert_eq!(
                worker.join().unwrap(),
                Some(vec![PathBuf::from("/Library/a")])
            );
            // Answered and removed: the same id is refused.
            assert!(complete(&json!({ "id": id, "moved": [] })).is_err());

            let worker = std::thread::spawn(|| request("Other", &[PathBuf::from("/Library/c")]));
            let id = next(&json!({ "waitMs": 5000 })).unwrap()["request"]["id"]
                .as_u64()
                .unwrap();
            complete(&json!({ "id": id, "moved": ["/Library/c"], "declined": true })).unwrap();
            assert_eq!(worker.join().unwrap(), Some(Vec::new()));

            assert!(complete(&json!({ "moved": [] })).is_err());
            assert!(complete(&json!({ "id": 999_999 })).is_err());
            assert!(complete(&json!({ "id": 999_999, "moved": [] })).is_err());
            assert_eq!(run(false, "App", || 7), 7);
        }
    }
}

#[cfg(unix)]
pub use imp::{complete, next, run};

/// Windows has no helper: the flag is ignored and the methods refuse.
#[cfg(not(unix))]
pub fn run<R>(_elevate: bool, _app: &str, work: impl FnOnce() -> R) -> R {
    work()
}

#[cfg(not(unix))]
pub fn next(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    Err("not available on Windows".into())
}

#[cfg(not(unix))]
pub fn complete(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    Err("not available on Windows".into())
}

#[cfg(all(test, not(unix)))]
mod non_unix_tests {
    use super::*;

    #[test]
    fn the_helper_methods_refuse_and_run_just_does_the_work() {
        assert!(next(&serde_json::json!({})).is_err());
        assert!(complete(&serde_json::json!({ "id": 1, "moved": [] })).is_err());
        assert_eq!(run(true, "App", || 7), 7);
    }
}
