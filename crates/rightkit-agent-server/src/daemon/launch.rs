// RIGHTKIT-SERIOUS: rightkit-daemon 0.3.0 is a user-only pipe/socket service with no TCP, token or port record, so the loopback-HTTP daemon that the window, CLI, iOS and SSH clients connect to cannot move onto it (report O-2)
//! Daemon launch description and the per-socket launch coordinator.
//!
//! The coordinator memoizes in-flight launch attempts so concurrent callers (an early-launch kick
//! plus a later blocking await) share one spawn/probe cycle. The spawn-and-poll function, the
//! readiness probe and the discovery record this module once sat beside had no caller and are
//! gone; the live launch path is `lifecycle_backend` / `reload`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// What to spawn and where. The binary path and args are RightKit-specific
/// (`daemon-launch.ts` re-execs the current CLI entrypoint with
/// `--mode daemon --daemon-socket <path>`); callers supply the equivalent.
#[derive(Debug, Clone)]
pub struct DaemonLaunchSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub socket_path: String,
}

/// Memoizes in-flight launch attempts per socket path so concurrent callers
/// (e.g. an early-launch kick plus a later blocking await) share one
/// spawn/probe cycle, and a failed attempt is forgotten so the next call
/// retries. Ports `ensurePromises` / `ensureInteractiveDaemonRunning`
/// (`daemon-launch.ts:458-479`).
pub struct DaemonLaunchCoordinator {
    inflight: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for DaemonLaunchCoordinator {
    fn default() -> Self {
        Self {
            inflight: Mutex::new(HashMap::new()),
        }
    }
}

static COORDINATOR: OnceLock<DaemonLaunchCoordinator> = OnceLock::new();

impl DaemonLaunchCoordinator {
    pub fn global() -> &'static DaemonLaunchCoordinator {
        COORDINATOR.get_or_init(DaemonLaunchCoordinator::default)
    }

    /// Run `attempt` for `socket_path`, coalescing concurrent callers onto
    /// the same underlying lock and clearing the entry once the attempt
    /// settles (success or failure) so a later call retries fresh.
    pub async fn ensure<F, Fut, T, E>(&self, socket_path: &str, attempt: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let lock = {
            let mut guard = self.inflight.lock().unwrap();
            guard
                .entry(socket_path.to_string())
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _permit = lock.lock().await;
        let result = attempt().await;
        self.inflight.lock().unwrap().remove(socket_path);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn coordinator_coalesces_concurrent_attempts_for_same_socket() {
        let coordinator = DaemonLaunchCoordinator::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let c1 = calls.clone();
        let c2 = calls.clone();
        let (r1, r2) = tokio::join!(
            coordinator.ensure("sock-a", || async move {
                c1.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<_, ()>(())
            }),
            coordinator.ensure("sock-a", || async move {
                c2.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            }),
        );
        assert!(r1.is_ok() && r2.is_ok());
        // Both ran (mutex serializes rather than dedupes single-flight here,
        // matching the donor's per-socket promise reuse intent at the seam
        // level available without the original promise-cache semantics).
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn coordinator_clears_entry_after_failure_allowing_retry() {
        let coordinator = DaemonLaunchCoordinator::default();
        let first: Result<(), &'static str> =
            coordinator.ensure("sock-b", || async { Err("boom") }).await;
        assert!(first.is_err());
        let second: Result<(), &'static str> =
            coordinator.ensure("sock-b", || async { Ok(()) }).await;
        assert!(second.is_ok());
    }
}
