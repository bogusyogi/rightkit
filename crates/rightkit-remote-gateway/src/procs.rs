//! `procs.list` and `procs.stop`: the user's own running programs, listed and stopped by a remote
//! caller, tier 2. Same switch (`remoteTerminals`) and the same local grant as `term.*`
//! (`term::gate`): one approval per door, or the standing policy.
//!
//! * `procs.list` shows only processes owned by the same user as the host app, never the host app itself, with
//!   `pid` and `startTime` (Unix seconds), the pair a stop must repeat.
//! * `procs.stop` verifies PID plus start time against a fresh read of the system, captures the
//!   process's identity with RightKit's process owner (`rightkit_child_process::AdoptedIdentity`),
//!   reads the start time again, and only then signals; the owner re-verifies the identity
//!   inside the signal call. A pid that was reused, or that belongs to another user, is refused and
//!   nothing is sent. the host app itself and its parent are never stopped. Graceful (`SIGTERM`) is the
//!   default; `force` is a separate, explicit argument (Windows has one way to stop, so both are
//!   the same there).

use super::settings::Settings;
use super::{Origin, Refusal, Result, term};
use rightkit_child_process::{AdoptedIdentity, AdoptedSignal, IdentityCheck, SignalResult};
use rightkit_system_monitor::process_sampler;
use serde_json::{Value, json};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const DEFAULT_LIMIT: u64 = 200;
const MAX_LIMIT: u64 = 1000;
const QUERY_MAX: usize = 256;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// `procs.list`. `args`: `query` (part of the name, case-insensitive), `limit` (default 200, at
/// most 1000). Largest memory first.
pub fn list(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    term::gate(settings, origin)?;
    let query = match &args["query"] {
        Value::Null => None,
        Value::String(q) if !q.is_empty() && q.chars().count() <= QUERY_MAX => {
            Some(q.to_lowercase())
        }
        _ => return Err(invalid(format!("query is 1 to {QUERY_MAX} characters"))),
    };
    let limit = match &args["limit"] {
        Value::Null => DEFAULT_LIMIT,
        value => match value.as_u64() {
            Some(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(invalid(format!("limit is 1 to {MAX_LIMIT}"))),
        },
    } as usize;
    let me = std::process::id();
    let (rows, total) = process_sampler::with_processes(|system| {
        let mine = system
            .process(Pid::from_u32(me))
            .and_then(|p| p.user_id().cloned());
        let mut rows: Vec<(u64, Value)> = system
            .processes()
            .values()
            .filter(|p| {
                let pid = p.pid().as_u32();
                pid > 1
                    && pid != me
                    && mine.is_some()
                    && p.user_id().cloned() == mine
                    && p.start_time() > 0
            })
            .filter(|p| {
                query
                    .as_ref()
                    .is_none_or(|q| p.name().to_string_lossy().to_lowercase().contains(q))
            })
            .map(|p| {
                (
                    p.memory(),
                    json!({
                        "pid": p.pid().as_u32(),
                        "startTime": p.start_time(),
                        "name": p.name().to_string_lossy(),
                        "exe": p.exe().map(|e| e.to_string_lossy().into_owned()),
                        "parentPid": p.parent().map(|pp| pp.as_u32()),
                        "cpuPercent": p.cpu_usage(),
                        "memoryBytes": p.memory(),
                    }),
                )
            })
            .collect();
        rows.sort_by_key(|(memory, _)| std::cmp::Reverse(*memory));
        let total = rows.len();
        (rows, total)
    })
    .map_err(|e| {
        Refusal::new(
            "unavailable",
            format!("the process table cannot be read ({e})"),
        )
    })?;
    let shown: Vec<Value> = rows.into_iter().take(limit).map(|(_, row)| row).collect();
    let returned = shown.len();
    Ok(json!({
        "ok": true,
        "processes": shown,
        "returned": returned,
        "total": total,
        "truncated": total > returned,
    }))
}

/// A fresh read of one process: its start time and owner, not the shared table's cached copy.
fn fresh(pid: u32) -> (Option<u64>, Option<sysinfo::Uid>, Option<u32>) {
    let Ok(mut system) = System::new() else {
        return (None, None, None);
    };
    let id = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[id]),
        true,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
    );
    match system.process(id) {
        Some(p) => (
            Some(p.start_time()),
            p.user_id().cloned(),
            p.parent().map(|pp| pp.as_u32()),
        ),
        None => (None, None, None),
    }
}

/// `procs.stop`. `args`: `pid` and `startTime` (both from `procs.list`), `force` (true: kill
/// instead of asking it to quit).
pub fn stop(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    term::gate(settings, origin)?;
    let pid = args["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 1)
        .ok_or_else(|| invalid("pid is a process number from procs.list"))?;
    let start = args["startTime"]
        .as_u64()
        .filter(|s| *s > 0)
        .ok_or_else(|| invalid("startTime is needed: repeat it from procs.list"))?;
    let force = match &args["force"] {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => return Err(invalid("force is true or false")),
    };
    let me = std::process::id();
    let (_, my_owner, my_parent) = fresh(me);
    if pid == me || my_parent == Some(pid) {
        return Err(Refusal::new(
            "refused",
            "The app does not stop itself or the program that started it",
        ));
    }
    let checked = |why: &str| -> Result<()> {
        let (seen, owner, _) = fresh(pid);
        match seen {
            None => Err(Refusal::new("not_found", "no such process any more")),
            Some(s) if s != start => Err(Refusal::new(
                "identity_mismatch",
                format!("that pid now names a different process ({why}); nothing was sent"),
            )),
            Some(_) if my_owner.is_none() || owner != my_owner => Err(Refusal::new(
                "refused",
                "that process belongs to another user; nothing was sent",
            )),
            Some(_) => Ok(()),
        }
    };
    checked("start time differs")?;
    let nonzero = NonZeroU32::new(pid).ok_or_else(|| invalid("pid is a process number"))?;
    let identity = AdoptedIdentity::capture(nonzero)
        .map_err(|e| Refusal::new("read_failed", format!("could not read it: {}", e.kind())))?
        .ok_or_else(|| Refusal::new("not_found", "no such process any more"))?;
    // The pid could have been reused between the first check and the capture.
    checked("it changed while it was identified")?;
    let signal = if force {
        AdoptedSignal::Force
    } else {
        AdoptedSignal::Graceful
    };
    let delivered = match identity.signal(signal) {
        Ok(SignalResult::Delivered) => true,
        Ok(SignalResult::Gone) => false,
        Ok(SignalResult::IdentityMismatch) => {
            return Err(Refusal::new(
                "identity_mismatch",
                "that pid now names a different process; nothing was sent",
            ));
        }
        Err(e) => {
            return Err(Refusal::new(
                "stop_failed",
                format!("the signal could not be sent: {}", e.kind()),
            ));
        }
    };
    // Look for the end for a few seconds; a graceful request is not a promise.
    let until = Instant::now() + Duration::from_secs(3);
    let mut gone = !delivered;
    while !gone && Instant::now() < until {
        gone = !matches!(identity.verify(), Ok(IdentityCheck::Same));
        if !gone {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(json!({
        "ok": true,
        "pid": pid,
        "signal": if force { "force" } else { "graceful" },
        "alreadyGone": !delivered,
        "gone": gone,
    }))
}
