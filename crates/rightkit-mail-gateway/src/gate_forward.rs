//! Gate steps for `forward`. Same shape as `draft`: `authorise_forward`
//! validates and resolves the original message under short closures and
//! returns a typed [`Step`]; `perform_forward` builds the forward draft through
//! the injected creator with no database lock held, then records it as an agent
//! draft of kind forward and creates the pending approval. If the draft cannot
//! be built, no approval is created (`draft_failed`). The request never sends:
//! from here it is exactly the send flow, decided by the user's click.
//! The gate audits the final outcome once; nothing here writes the audit log.
//! A forward does not take the process-wide mutation lock (that lock is for
//! label, archive and move limits): two racing forwards are kept inside the
//! pending cap and the limits by `create_pending`, whose `check_room` re-check
//! and insert run in one database closure.

use super::access::ConnAccess;
use super::approval::Clock;
use super::approval_kind as kinds;
use super::approval_request::{check_room, create_pending};
use super::approval_store as store;
use super::draft::DraftCreator;
use super::forward::{self, ForwardRequest};
use super::gate::{Ctx, Step};
use super::outcome::{from_db_error, AgentError, Request};
use super::registry::Agent;
use serde_json::Value;

/// Validate a `forward` request, resolve the original message and its
/// attachments from the local database, and check the agent still has room
/// (pending cap and send limits) before any draft is built. Nothing is created.
pub fn authorise_forward<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    agent: &Agent,
    req: &Request,
    ctx: &mut Ctx,
) -> Result<Step, AgentError> {
    let account = ctx.account.clone().unwrap_or_default();
    let query = forward::validate(&account, &req.params)?;
    let resolved = access
        .read(|c| Ok(forward::resolve(c, query)))
        .map_err(|e| from_db_error(&e))??;
    let key = forward::target_key(&resolved);
    let room = access
        .write(|c| {
            let room = check_room(c, clock, agent, &account, kinds::FORWARD, &key);
            if room.is_ok() {
                crate::host::host().guard(
                    c,
                    crate::host::MutationClass::SafeMutation,
                    "agent_forward_draft",
                    Some(&account),
                )?;
            }
            Ok(room)
        })
        .map_err(|e| from_db_error(&e))?;
    room?;
    Ok(Step::Forward {
        req: resolved,
        agent: agent.clone(),
    })
}

/// Build the forward draft and create the pending approval. No database
/// closure is open while the draft is built.
pub fn perform_forward<A: ConnAccess>(
    access: &A,
    creator: &dyn DraftCreator,
    clock: &dyn Clock,
    agent: &Agent,
    req: &ForwardRequest,
    ctx: &mut Ctx,
) -> Result<Value, AgentError> {
    let counts = forward::counts(req);
    let made = match creator.forward(req) {
        Ok(m) => m,
        Err(why) => {
            ctx.detail = Some(format!("{counts} reason={}", why.as_str()));
            return Err(AgentError::DraftFailed);
        }
    };
    let now = clock.now_ms();
    let saved = access.write(|c| {
        store::record_forward_draft(
            c,
            &agent.id,
            &req.account,
            &made.draft_id,
            &made.hash,
            &made.attachment_digest,
            now,
        )?;
        let key = forward::target_key(req);
        Ok(create_pending(
            c,
            clock,
            agent,
            &req.account,
            kinds::FORWARD,
            &made.draft_id,
            Some(&key),
        ))
    });
    match saved {
        Ok(Ok((value, created))) => {
            let id = created.clone().unwrap_or_default();
            let bytes = made.attachment_bytes;
            ctx.detail = Some(format!("approval={id} {counts} attachment_bytes={bytes}"));
            ctx.new_approval = created;
            Ok(value)
        }
        Ok(Err(refused)) => {
            ctx.detail = Some(format!("{counts} reason={}", refused.code()));
            Err(refused)
        }
        Err(_) => {
            ctx.detail = Some(format!("{counts} reason=record_failed"));
            Err(AgentError::DraftFailed)
        }
    }
}
