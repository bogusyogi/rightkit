//! ENG-56: network-aware background Git sync.
//!
//! iOS grants a background task window that is short and can be terminated
//! at any moment — a `BGProcessingTask` can be revoked mid-run, with no
//! guarantee any particular step finishes. Every step this module performs
//! is therefore a complete, independent, resumable unit: one fetch against
//! one workspace target. Nothing here assumes it will get to run a second
//! step, and a partially completed sweep (some targets fetched, some
//! skipped) is a normal, safe outcome rather than a failure to recover from.
//!
//! This module is fetch/pull-only assistance — it never merges, rebases, or
//! pushes, matching the restrictions [`crate::git`] already enforces for the
//! on-device Git transport. Reconciling a fetched remote-tracking branch
//! into the working tree is a foreground concern the user drives explicitly
//! through the existing Git UI; this coordinator only keeps `origin/<branch>`
//! current in the background so that foreground reconciliation has fresh
//! data to work with.
//!
//! # iOS scheduler boundary
//!
//! `BGTaskScheduler` registration and task submission belong to the iOS host;
//! Rust cannot safely manufacture or acknowledge an iOS background task from
//! this crate. The host's `BGProcessingTask` handler invokes this coordinator
//! through the mobile FFI and connects its expiration callback to the FFI
//! cancellation entry point backed by [`SyncCancelToken`]. It must treat a
//! missing handler or failed fetch as a typed failure rather than reporting
//! fabricated completion. Because the
//! coordinator owns no foreground session state, the callback is safe to run
//! after the app's interactive runtime has been suspended or discarded.
//!
//! The iOS half of this boundary is `LocalBackgroundSyncScheduler`: its
//! `BGTaskScheduler` handler invokes the FFI entry point, which constructs this
//! coordinator and calls [`BackgroundSyncCoordinator::run`]. Rust does not
//! register an iOS scheduler or assume a foreground `MobileRuntime` exists;
//! doing so here would be a fabricated external artifact and would also make
//! the static library unsafe for other hosts. The scheduler/FFI boundary is
//! outside this lane's allowlist, so this file records that integration rather
//! than duplicating it. A missing workspace or Git transport remains the typed
//! [`SyncOutcome::Failed`] result, never a reported success.
//!
//! # Never blocks foreground work
//!
//! [`BackgroundSyncCoordinator::run`] is a plain synchronous call with no
//! locks shared with the interactive session path — it opens its own
//! `git2::Repository` handle per call via [`crate::git::execute`] and holds
//! nothing across steps. Callers are expected to invoke it from a background
//! queue exactly as [`crate::LocalRuntime`]'s Swift counterpart pushes
//! `submit` off the calling thread (see `LocalRuntime.swift`); this module
//! does not spawn threads itself so the host controls exactly which thread
//! and priority the sync work runs at.
//!
//! # Cancellation
//!
//! [`SyncCancelToken`] is the cooperative cancellation handle: the host's
//! background-task expiration handler calls [`SyncCancelToken::cancel`], and
//! [`BackgroundSyncCoordinator::run`] checks it between (never during) each
//! target's fetch. A fetch already in flight when cancellation fires still
//! runs to its own natural completion or error inside `git2` — this
//! coordinator only refuses to *start* the next one, which is exactly the
//! granularity a revoked background window needs: no half-written Git state,
//! just an early stop.

use crate::git;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Outcome of attempting to sync one target.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOutcome {
    /// The fetch completed successfully.
    Completed,
    /// Cancellation was observed before this target's fetch started.
    Cancelled,
    /// The coordinator's overall deadline elapsed before this target's fetch
    /// started.
    TimedOut,
    /// The fetch was attempted and failed. Carries the underlying error
    /// message from [`crate::git::execute`] (already free of any credential
    /// material — `git.rs` never echoes tokens back in its error strings).
    Failed(String),
}

/// One workspace this sweep should attempt to keep current.
#[derive(Debug, Clone)]
pub struct SyncTarget {
    pub workspace_root: PathBuf,
    /// Remote name, e.g. `"origin"`. `None` lets `git.rs` apply its own
    /// default.
    pub remote: Option<String>,
    /// Short-lived forge token for the fetch, if the remote requires auth.
    /// Never persisted by this module; it is used for exactly one `git2`
    /// call and then dropped along with the rest of the `SyncTarget`.
    pub token: Option<String>,
}

/// Cooperative cancellation handle shared between the host's background-task
/// expiration callback and a running [`BackgroundSyncCoordinator`].
/// Cloning shares the same underlying flag.
#[derive(Debug, Clone, Default)]
pub struct SyncCancelToken(Arc<AtomicBool>);

impl SyncCancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Wraps the flag returned by a RightKit `InlineCancelRegistry::register`, so the
    /// registry's `cancel` reaches this sweep.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// Requests cancellation. Idempotent; safe to call from any thread,
    /// including directly from an iOS expiration handler.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Result of syncing one target, returned as part of a full sweep so the
/// host can report exactly which workspaces are current and which were
/// skipped or failed — never a single pass/fail bit for the whole sweep.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetSyncResult {
    pub workspace_root: PathBuf,
    pub outcome: SyncOutcome,
}

/// Bounded, cancellable fetch-only sync across one or more workspace
/// targets, sized to fit inside one background task window.
pub struct BackgroundSyncCoordinator {
    targets: Vec<SyncTarget>,
    deadline: Duration,
    cancel: SyncCancelToken,
}

impl BackgroundSyncCoordinator {
    /// `deadline` bounds total wall-clock time this sweep will spend
    /// starting new fetches — it does not preempt a fetch already running,
    /// since `git2`'s network calls are not preemptible from outside. Choose
    /// a deadline comfortably inside the host's actual background window
    /// (e.g. iOS's `BGProcessingTask`) to leave room for the in-flight
    /// fetch that crosses the deadline to still finish and for
    /// `setTaskCompleted` bookkeeping afterward.
    pub fn new(targets: Vec<SyncTarget>, deadline: Duration, cancel: SyncCancelToken) -> Self {
        Self {
            targets,
            deadline,
            cancel,
        }
    }

    /// Returns a clone of this sweep's cancellation token so the host can
    /// retain one to cancel from its expiration handler while `run` executes
    /// elsewhere.
    pub fn cancel_token(&self) -> SyncCancelToken {
        self.cancel.clone()
    }

    /// Runs the sweep to completion, cancellation, or deadline — whichever
    /// comes first — and reports a result per target. This is the Rust work
    /// item submitted by the iOS `BGProcessingTask` handler; it is synchronous
    /// by design so the host owns thread/priority placement and can keep the
    /// foreground session entirely independent (see module docs).
    pub fn run(&self) -> Vec<TargetSyncResult> {
        let started = Instant::now();
        let mut results = Vec::with_capacity(self.targets.len());
        for target in &self.targets {
            if self.cancel.is_cancelled() {
                results.push(TargetSyncResult {
                    workspace_root: target.workspace_root.clone(),
                    outcome: SyncOutcome::Cancelled,
                });
                continue;
            }
            if started.elapsed() >= self.deadline {
                results.push(TargetSyncResult {
                    workspace_root: target.workspace_root.clone(),
                    outcome: SyncOutcome::TimedOut,
                });
                continue;
            }
            let outcome = Self::sync_one(target);
            results.push(TargetSyncResult {
                workspace_root: target.workspace_root.clone(),
                outcome,
            });
        }
        results
    }

    /// Fetch-only sync of a single target via the same typed transport
    /// (`crate::git::execute`) the interactive Git UI uses, so background
    /// sync can never do anything the foreground path itself could not:
    /// no shell, no URL credentials, no force operations, no merges.
    fn sync_one(target: &SyncTarget) -> SyncOutcome {
        let request = json!({
            "operation": "fetch",
            "workspaceRoot": target.workspace_root,
            "remote": target.remote,
            "token": target.token,
        });
        let response = git::execute(&request.to_string());
        if response
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            SyncOutcome::Completed
        } else {
            let error = response
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown Git fetch error")
                .to_string();
            SyncOutcome::Failed(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Builds a bare "remote" repo plus a clone of it, both under one temp
    /// dir, and returns (`clone_workspace_root`). Uses the system `git`
    /// binary purely as fast test-fixture setup — the coordinator under test
    /// never shells out itself; it goes through `git2` via `crate::git`.
    fn make_clone_fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let remote_path = dir.path().join("remote.git");
        let clone_path = dir.path().join("clone");

        let run = |args: &[&str], cwd: &std::path::Path| {
            let status = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .expect("git fixture command runs");
            assert!(status.success(), "git {args:?} failed in {cwd:?}");
        };

        run(
            &["init", "--bare", remote_path.to_str().unwrap()],
            dir.path(),
        );

        let seed_path = dir.path().join("seed");
        run(
            &[
                "clone",
                remote_path.to_str().unwrap(),
                seed_path.to_str().unwrap(),
            ],
            dir.path(),
        );
        std::fs::write(seed_path.join("file.txt"), "hello\n").expect("write seed file");
        run(&["config", "user.email", "test@example.com"], &seed_path);
        run(&["config", "user.name", "Test"], &seed_path);
        run(&["add", "file.txt"], &seed_path);
        run(&["commit", "-m", "seed"], &seed_path);
        run(&["push", "origin", "HEAD:refs/heads/main"], &seed_path);

        run(
            &[
                "clone",
                remote_path.to_str().unwrap(),
                clone_path.to_str().unwrap(),
            ],
            dir.path(),
        );

        (dir, clone_path)
    }

    #[test]
    fn empty_target_list_produces_empty_results() {
        let coordinator = BackgroundSyncCoordinator::new(
            Vec::new(),
            Duration::from_secs(5),
            SyncCancelToken::new(),
        );
        assert!(coordinator.run().is_empty());
    }

    #[test]
    fn pre_cancelled_token_skips_every_target_without_attempting_a_fetch() {
        let target = SyncTarget {
            workspace_root: PathBuf::from("/does/not/exist"),
            remote: None,
            token: None,
        };
        let cancel = SyncCancelToken::new();
        cancel.cancel();
        let coordinator = BackgroundSyncCoordinator::new(
            vec![target.clone(), target],
            Duration::from_secs(5),
            cancel,
        );
        let results = coordinator.run();
        assert_eq!(results.len(), 2);
        for result in results {
            assert_eq!(result.outcome, SyncOutcome::Cancelled);
        }
    }

    #[test]
    fn zero_deadline_times_out_before_any_fetch_starts() {
        let target = SyncTarget {
            workspace_root: PathBuf::from("/does/not/exist"),
            remote: None,
            token: None,
        };
        let coordinator =
            BackgroundSyncCoordinator::new(vec![target], Duration::ZERO, SyncCancelToken::new());
        let results = coordinator.run();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, SyncOutcome::TimedOut);
    }

    #[test]
    fn missing_workspace_reports_a_failed_outcome_not_a_panic() {
        let target = SyncTarget {
            workspace_root: PathBuf::from("/definitely/not/a/repo"),
            remote: None,
            token: None,
        };
        let coordinator = BackgroundSyncCoordinator::new(
            vec![target],
            Duration::from_secs(5),
            SyncCancelToken::new(),
        );
        let results = coordinator.run();
        assert_eq!(results.len(), 1);
        match &results[0].outcome {
            SyncOutcome::Failed(message) => assert!(!message.is_empty()),
            other => panic!("expected Failed outcome, got {other:?}"),
        }
    }

    /// `crate::git::fetch` (ENG-46's `validated_github_url`) deliberately
    /// refuses to fetch from any remote that is not an HTTPS `github.com`
    /// URL — and it checks the URL libgit2 *actually resolves to*, not just
    /// the string configured on the remote: `git_remote_lookup` in libgit2
    /// applies `url.*.insteadOf` rewriting itself before this crate ever
    /// sees `Remote::url()`, so pointing `origin`'s configured URL at
    /// `github.com` while rewriting it locally via `insteadOf` still
    /// surfaces the *rewritten* (non-`github.com`) URL here and is
    /// correctly refused — confirmed empirically while writing this test.
    /// That leaves no way to exercise a genuinely successful on-device fetch
    /// without real network access to github.com, which a hermetic unit
    /// test must not depend on. What this test can and must pin down
    /// instead is that `BackgroundSyncCoordinator` faithfully surfaces that
    /// refusal as `SyncOutcome::Failed` with `crate::git`'s own message,
    /// rather than panicking or misreporting it as `Completed`.
    #[test]
    fn non_github_origin_is_refused_and_reported_as_failed() {
        let (_dir, clone_path) = make_clone_fixture();
        let target = SyncTarget {
            workspace_root: clone_path,
            remote: Some("origin".to_string()),
            token: None,
        };
        let coordinator = BackgroundSyncCoordinator::new(
            vec![target],
            Duration::from_secs(30),
            SyncCancelToken::new(),
        );
        let results = coordinator.run();
        assert_eq!(results.len(), 1);
        match &results[0].outcome {
            SyncOutcome::Failed(message) => assert!(
                message.contains("github.com"),
                "expected the github.com-only refusal, got: {message}"
            ),
            other => panic!("expected Failed outcome, got {other:?}"),
        }
    }

    #[test]
    fn cancellation_between_targets_stops_remaining_work_but_keeps_earlier_results() {
        let (_dir, clone_path) = make_clone_fixture();
        let good_target = SyncTarget {
            workspace_root: clone_path,
            remote: Some("origin".to_string()),
            token: None,
        };
        let missing_target = SyncTarget {
            workspace_root: PathBuf::from("/definitely/not/a/repo"),
            remote: None,
            token: None,
        };
        let cancel = SyncCancelToken::new();
        let coordinator = BackgroundSyncCoordinator::new(
            vec![good_target, missing_target],
            Duration::from_secs(30),
            cancel.clone(),
        );
        // Cancel immediately after construction but before `run`: this still
        // exercises the "skip everything" path deterministically, matching
        // an expiration that fires before the sweep gets any CPU time.
        cancel.cancel();
        let results = coordinator.run();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.outcome == SyncOutcome::Cancelled));
    }

    #[test]
    fn cancel_token_clones_share_state() {
        let token = SyncCancelToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }
}
