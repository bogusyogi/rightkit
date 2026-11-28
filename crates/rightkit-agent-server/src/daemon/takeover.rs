//! Safe stale-owner takeover for the host's daemon (DL-002).
//!
//! Termination and identity pinning are `rightkit_daemon::stop_identity`: the
//! predecessor's identity is captured once (`rightkit_child_process::AdoptedIdentity`:
//! pid plus the host's start token) before any signal, and `stop_identity`
//! re-verifies that identity and the executable path immediately before each
//! signal. Success is the ownership lock being released and now held by this
//! process, never pid death alone; the lock is never bypassed. Any unresolved
//! identity is ambiguous ownership and refuses takeover.

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use rightkit_child_process::AdoptedIdentity;
use rightkit_daemon::{stop_identity, IncumbentError, StopOptions};

use super::ownership::{read_lockfile_pid, OwnershipLock};

/// How long a takeover waits at each stage (ask, graceful signal, forced stop)
/// for the predecessor to release the lockfile lock.
pub(crate) const TAKEOVER_GRACE: Duration = Duration::from_secs(2);

/// Acquire the ownership lock, taking over from a live predecessor daemon of this host
/// if one holds it: a cooperative shutdown request, a graceful signal,
/// then a forced stop, each bounded by `grace`, through
/// `rightkit_daemon::stop_identity`. The lock is never bypassed: a guard is
/// returned only with the lock held.
///
/// `Ok(None)` means the caller should exit quietly: the holder's identity
/// could not be pinned (pid recycled or process gone), or the lock could not
/// be won after escalation (e.g. a concurrent newer spawn took it).
pub fn acquire_or_take_over(
    path: &Path,
    grace: Duration,
) -> std::io::Result<Option<OwnershipLock>> {
    if let Some(guard) = OwnershipLock::acquire(path)? {
        return Ok(Some(guard));
    }

    let Some(pid) = read_lockfile_pid(path) else {
        return Ok(None);
    };
    if pid == std::process::id() {
        return Ok(None);
    }
    // Audit 2026-11-05 daemon H1: `daemon.lock` is also held by an inline `run`
    // (R13) and by a `serve` that has not yet published its discovery record. Only a
    // holder that is the live process the published `daemon.port` names is a daemon this
    // takeover may stop; any other holder is refused untouched.
    if !holder_is_published_daemon(path, pid) {
        eprintln!(
            "lock holder pid {pid} is not the daemon named in daemon.port (a `{run}` or a daemon still starting); declining takeover",
            run = crate::host_identity::command("run")
        );
        return Ok(None);
    }

    // Pin the predecessor identity once, before any signal. An unresolved
    // identity is ambiguous ownership and refuses takeover.
    let Some(pid_nz) = NonZeroU32::new(pid) else {
        return Ok(None);
    };
    let Some(identity) = AdoptedIdentity::capture(pid_nz).ok().flatten() else {
        return Ok(None);
    };

    eprintln!(
        "taking over from predecessor {app} daemon (pid {pid})",
        app = crate::host_identity::app()
    );
    let mut acquired = None;
    let exe = std::env::current_exe()?;
    let options = StopOptions::new(grace).expected_exe(exe);
    match stop_identity(
        &identity,
        &options,
        || {
            if request_cooperative_shutdown(path, pid) {
                Ok(())
            } else {
                Err(std::io::Error::other("no cooperative shutdown"))
            }
        },
        || match OwnershipLock::acquire(path) {
            Ok(Some(guard)) => {
                acquired = Some(guard);
                true
            }
            _ => false,
        },
    ) {
        Ok(_) => Ok(acquired),
        Err(IncumbentError::OwnProcess) => Ok(None),
        Err(IncumbentError::ExecutableMismatch { .. }) => {
            eprintln!(
                "lockfile pid {pid} does not resolve to a {app} executable; declining takeover (likely pid reuse)",
                app = crate::host_identity::app()
            );
            Ok(None)
        }
        Err(IncumbentError::NotIncumbent { .. } | IncumbentError::NoIdentity { .. }) => {
            eprintln!("predecessor identity changed before the signal; declining takeover");
            Ok(None)
        }
        Err(IncumbentError::NotReleased { .. } | IncumbentError::Survived { .. }) => {
            eprintln!(
                "predecessor (pid {pid}) did not release the lock in time; lock is still held after killing it; exiting"
            );
            Ok(None)
        }
        Err(IncumbentError::Io(error)) => {
            eprintln!("failed to stop predecessor (pid {pid}): {error}");
            Ok(None)
        }
        Err(error) => {
            eprintln!("failed to stop predecessor (pid {pid}): {error}");
            Ok(None)
        }
    }
}

/// Whether `pid`, the current lock holder, is the live process recorded in the discovery file
/// next to `lock_path`. A missing, unparsable or legacy (no birth time) record, a record naming
/// another pid, or a recorded pid whose start time no longer matches all answer `false`.
fn holder_is_published_daemon(lock_path: &Path, pid: u32) -> bool {
    let Some(discovery_path) = lock_path.parent().map(|parent| parent.join("daemon.port")) else {
        return false;
    };
    std::fs::read_to_string(discovery_path)
        .ok()
        .is_some_and(|content| record_names_live_holder(&content, pid))
}

fn record_names_live_holder(record_text: &str, pid: u32) -> bool {
    rightkit_daemon::loopback::LoopbackRecord::parse(record_text)
        .is_some_and(|record| record.pid == pid && record.process_matches())
}

fn request_cooperative_shutdown(lock_path: &Path, pid: u32) -> bool {
    let Some(discovery_path) = lock_path.parent().map(|parent| parent.join("daemon.port")) else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(discovery_path) else {
        return false;
    };
    let Some(info) = rightkit_daemon::loopback::LoopbackRecord::parse(&content) else {
        return false;
    };
    if info.pid != pid || info.port == 0 || info.token.is_empty() {
        return false;
    }
    // The blocking `rightkit_http::Client` owns no async runtime, so it is safe to call
    // from sync and async contexts alike. Loopback only: the discovery record names
    // this machine's daemon.
    let client = rightkit_http::Client::new(rightkit_http::ClientConfig {
        connect_timeout: Some(Duration::from_millis(500)),
        request_timeout: Duration::from_millis(500),
        response_timeout: Some(Duration::from_millis(500)),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: rightkit_http::AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |_| true),
        proxy: rightkit_http::ProxyMode::None,
        ..rightkit_http::ClientConfig::default()
    });
    client
        .send_any(
            &rightkit_http::Request::post(format!(
                "http://127.0.0.1:{}/internal/takeover",
                info.port
            ))
            .bearer(&info.token),
        )
        .is_ok_and(|response| (200..300).contains(&response.status))
}

#[cfg(test)]
mod published_holder_tests {
    use super::{holder_is_published_daemon, record_names_live_holder};
    use rightkit_daemon::loopback::{LoopbackRecord, RecordLabels};

    fn own_record() -> LoopbackRecord {
        LoopbackRecord::for_this_process(4242, "token", &RecordLabels::default())
    }

    #[test]
    fn record_for_the_live_holder_permits_takeover() {
        let record = own_record();
        assert!(record_names_live_holder(
            &record.to_line(),
            std::process::id()
        ));
    }

    #[test]
    fn holder_not_named_by_record_is_refused() {
        // A stale record names a dead daemon while a `run` (another pid) holds the lock.
        let record = own_record();
        assert!(!record_names_live_holder(
            &record.to_line(),
            std::process::id().wrapping_add(1)
        ));
    }

    #[test]
    fn legacy_or_malformed_record_is_refused() {
        let mut record = own_record();
        record.boot_epoch_ms = 0;
        assert!(!record_names_live_holder(
            &record.to_line(),
            std::process::id()
        ));
        assert!(!record_names_live_holder("not json", std::process::id()));
        assert!(!record_names_live_holder("", std::process::id()));
    }

    #[test]
    fn missing_discovery_file_is_refused() {
        let temp = tempfile::tempdir().expect("temp dir");
        let dir = temp.path();
        let lock = dir.join("daemon.lock");
        assert!(!holder_is_published_daemon(&lock, std::process::id()));
        std::fs::write(dir.join("daemon.port"), own_record().to_line()).expect("record");
        assert!(holder_is_published_daemon(&lock, std::process::id()));
    }
}
