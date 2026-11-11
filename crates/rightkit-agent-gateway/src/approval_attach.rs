//! The user's side of an `attach` approval: the card (path and size, read from
//! file metadata only) and the Approve step. Called from `approval::card` and
//! `approval::approve`, the latter only after the approval was claimed (one
//! guarded UPDATE), so a file is never opened for a request that is pending,
//! denied, expired or already decided.
//!
//! Approve order, never reordered: stat the file again, compare the card's hash
//! with the hash of what is there now (refuse `file_changed` on any difference),
//! refuse an over-size file, read the file (the reader re-checks size and time
//! on the opened handle and the length read), then copy the bytes into the
//! agent's export folder. Every refusal closes the approval as failed with a
//! reason code; none retries.

use super::access::ConnAccess;
use super::approval::{self, ApprovalCard, Clock, DecisionView};
use super::approval_store::{self as store, Approval, MailApproval};
use super::attach_path::{self, AttachFiles, AttachSummary};
use super::draft_files::TOTAL_MAX;

/// The card for one pending attach approval. Reads metadata, never the bytes.
pub fn card<A: ConnAccess>(
    access: &A,
    files: &dyn AttachFiles,
    a: Approval,
    agent_name: String,
) -> Result<ApprovalCard, String> {
    let path = access
        .read(|c| store::attach_path(c, a.id()))?
        .ok_or("This request is no longer available.")?;
    let meta = files
        .stat(&path)
        .map_err(|e| e.card_message().to_string())?;
    Ok(ApprovalCard {
        content_hash: attach_path::binding(a.account_id(), a.agent_id(), &path, meta),
        attach: Some(AttachSummary {
            path,
            size: meta.size,
        }),
        id: a.id().to_string(),
        kind: a.kind().to_string(),
        agent_name,
        account: a.account_id().to_string(),
        expires_at: a.expires_at(),
        ..ApprovalCard::default()
    })
}

/// Approve for an attach, after the claim was won.
pub fn finish<A: ConnAccess>(
    access: &A,
    files: &dyn AttachFiles,
    clock: &dyn Clock,
    a: &Approval,
    shown_hash: &str,
) -> Result<DecisionView, String> {
    let Some(path) = access.read(|c| store::attach_path(c, a.id()))? else {
        return approval::failed(access, clock, a, "file_missing", "");
    };
    let meta = match files.stat(&path) {
        Ok(m) => m,
        Err(e) => return approval::failed(access, clock, a, e.reason(), ""),
    };
    let counts = format!("size={}", meta.size);
    if attach_path::binding(a.account_id(), a.agent_id(), &path, meta) != shown_hash {
        return approval::failed(access, clock, a, "file_changed", &counts);
    }
    if meta.size > TOTAL_MAX {
        return approval::failed(access, clock, a, "file_too_large", &counts);
    }
    let bytes = match files.read(&path, meta) {
        Ok(b) => b,
        Err(e) => return approval::failed(access, clock, a, e.reason(), &counts),
    };
    match files.store(a.agent_id(), &path, &bytes) {
        Ok(stored) if !stored.extension_blocked => {
            approval::completed(access, clock, a, &stored.name, &counts)
        }
        Ok(_) => approval::failed(access, clock, a, "type_blocked", &counts),
        Err(_) => approval::failed(access, clock, a, "store_failed", &counts),
    }
}
