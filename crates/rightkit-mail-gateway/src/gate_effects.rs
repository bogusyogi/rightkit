//! Gate steps for the methods with an effect outside the database: `label`,
//! `archive`, `move` (a Gmail label change) and `attachment` (a file written to
//! the export directory). Same shape as `draft`: `authorise_*` validates and
//! resolves under short closures and returns a typed [`Step`]; `perform_*`
//! runs the effect through the injected creator with no database lock held.
//! The gate audits the final outcome once; nothing here writes the audit log.

use super::access::ConnAccess;
use super::approval::Clock;
use super::attachment::{self, AttachmentRequest, TOO_LARGE_MESSAGE, TOTAL_BUDGET};
use super::content::untrusted;
use super::draft::DraftCreator;
use super::export::{self, ExportError, DIR_FULL_MESSAGE, FILE_CAP};
use super::gate::{Ctx, Step};
use super::mutate::{self, Op};
use super::mutate_local::{self, Plan};
use super::outcome::{from_db_error, AgentError, Request};
use super::registry::Agent;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

/// One mutation at a time per process, so two racing requests cannot both
/// pass the hourly limit. Taken after the key, lock, profile and account
/// checks and before the limit is read, with no database lock held; kept in
/// [`Ctx`] until the gate has written the audit row. Read methods never take it.
static MUTATION_LOCK: Mutex<()> = Mutex::new(());
/// A waiting mutation gives up after this, so wait plus the 15 second Gmail
/// deadline still fits the engine's 20 second request deadline.
/// A host's tests share this one lock across parallel threads on a loaded machine and may
/// lengthen the wait with [`set_mutation_wait`].
static MUTATION_WAIT_MS: AtomicU64 = AtomicU64::new(4_000);

/// How long a mutation waits for the one-at-a-time lock (default 4 seconds).
pub fn mutation_wait() -> Duration {
    Duration::from_millis(MUTATION_WAIT_MS.load(Ordering::Relaxed))
}

/// Change the wait for the one-at-a-time mutation lock, for hosts' tests on a loaded machine.
pub fn set_mutation_wait(wait: Duration) {
    MUTATION_WAIT_MS.store(wait.as_millis() as u64, Ordering::Relaxed);
}

pub fn hold_mutation_for(wait: Duration) -> Option<MutexGuard<'static, ()>> {
    let deadline = Instant::now() + wait;
    loop {
        match MUTATION_LOCK.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Validate a `label`, `archive` or `move` request, resolve it against the
/// account's labels and threads, enforce the hourly limit and call the app's
/// mutation guard. Nothing is changed here.
pub fn authorise_mutate<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    agent: &Agent,
    method: &str,
    req: &Request,
    ctx: &mut Ctx,
) -> Result<Step, AgentError> {
    let op = Op::from_method(method).ok_or(AgentError::Internal)?;
    let account = ctx.account.clone().unwrap_or_default();
    let parsed = mutate::validate(op, &account, &req.params)?;
    ctx.held = Some(hold_mutation_for(mutation_wait()).ok_or(AgentError::MutationFailed("busy"))?);
    let now = clock.now_ms();
    let plan = access
        .read(|c| Ok(mutate_local::resolve(c, &agent.id, parsed, now)))
        .map_err(|e| from_db_error(&e))??;
    access
        .write(|c| {
            crate::host::host().guard(
                c,
                plan.class,
                mutate_local::guard_action(op),
                Some(&account),
            )
        })
        .map_err(|e| from_db_error(&e))?;
    Ok(Step::Mutate(plan))
}

/// Ask Gmail for the change, then mirror it locally the way the app's own
/// actions do. A Gmail failure changes nothing locally, but Gmail may have
/// taken some batches (the audit row counts them); the next sync shows the
/// real state. A local failure after Gmail accepted the change is reported in
/// the note; the next sync repairs it.
pub fn perform_mutate<A: ConnAccess>(
    access: &A,
    creator: &dyn DraftCreator,
    plan: &Plan,
    ctx: &mut Ctx,
) -> Result<Value, AgentError> {
    let counts = mutate::counts(&plan.req);
    if let Err(f) = creator.modify(&plan.modify) {
        let chunks = mutate::chunk_count(plan.modify.message_ids.len());
        ctx.detail = Some(format!(
            "{counts} reason={} applied_chunks={} of {chunks}",
            f.why.as_str(),
            f.applied_chunks
        ));
        return Err(AgentError::MutationFailed(f.why.as_str()));
    }
    let local = access.write(|c| mutate_local::apply_local(c, &plan.req));
    let note = if local.is_ok() {
        ctx.detail = Some(counts);
        format!("Done in Gmail and in {}.", crate::host::product())
    } else {
        ctx.detail = Some(format!("{counts} local=pending_sync"));
        format!(
            "Done in Gmail. {} will show it after the next sync.",
            crate::host::product()
        )
    };
    Ok(json!({
        "threads": plan.req.threads.len(),
        "add": plan.req.add,
        "remove": plan.req.remove,
        "note": note,
    }))
}

/// Validate an `attachment` request and resolve it from the local tables.
pub fn authorise_attachment<A: ConnAccess>(
    access: &A,
    agent: &Agent,
    req: &Request,
    ctx: &mut Ctx,
) -> Result<Step, AgentError> {
    let account = ctx.account.clone().unwrap_or_default();
    let query = attachment::validate(&account, &req.params)?;
    let resolved = access
        .read(|c| Ok(attachment::resolve(c, &query)))
        .map_err(|e| from_db_error(&e))??;
    Ok(Step::Attachment {
        req: resolved,
        agent_id: agent.id.clone(),
    })
}

fn export_error(e: ExportError) -> AgentError {
    match e {
        ExportError::TooLarge => AgentError::Quota(TOO_LARGE_MESSAGE),
        ExportError::DirFull => AgentError::Quota(DIR_FULL_MESSAGE),
        ExportError::Blocked => AgentError::AttachmentFailed("blocked"),
        ExportError::ScanIncomplete => AgentError::AttachmentFailed("scan_incomplete"),
        ExportError::Unsafe | ExportError::Io => AgentError::AttachmentFailed("write_failed"),
    }
}

/// Fetch the attachment and write it into the agent's export directory. Fetch
/// and scan share one budget; a file that could not be scanned in time is
/// removed rather than returned.
pub fn perform_attachment(
    creator: &dyn DraftCreator,
    req: &AttachmentRequest,
    agent_id: &str,
    ctx: &mut Ctx,
) -> Result<Value, AgentError> {
    let fail = |ctx: &mut Ctx, e: AgentError| {
        ctx.detail = Some(format!("reason={}", e.reason().unwrap_or(e.code())));
        e
    };
    let Some(root) = creator.export_root() else {
        return Err(fail(ctx, AgentError::AttachmentFailed("write_failed")));
    };
    // Refuse before downloading when the folder cannot take the file.
    if !export::has_room_for(&root, agent_id, req.size.max(0) as u64) {
        return Err(fail(ctx, AgentError::Quota(DIR_FULL_MESSAGE)));
    }
    let started = Instant::now();
    let bytes = match creator.fetch_attachment(req) {
        Ok(b) => b,
        Err(why) => return Err(fail(ctx, AgentError::AttachmentFailed(why.as_str()))),
    };
    if bytes.len() as u64 > FILE_CAP {
        return Err(fail(ctx, AgentError::Quota(TOO_LARGE_MESSAGE)));
    }
    let scan =
        |p: &std::path::Path| creator.scan(p, TOTAL_BUDGET.saturating_sub(started.elapsed()));
    match export::store_with(&root, agent_id, &req.name, &bytes, &scan) {
        Ok(stored) => {
            ctx.detail = Some(format!("size={}", stored.size));
            Ok(json!({
                "path": stored.path.to_string_lossy(),
                "size": stored.size,
                "extension_blocked": stored.extension_blocked,
                "name": untrusted(&req.name),
                "mime": untrusted(&req.mime),
            }))
        }
        Err(e) => Err(fail(ctx, export_error(e))),
    }
}
