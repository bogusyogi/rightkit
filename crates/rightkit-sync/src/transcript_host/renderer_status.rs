//! The settings screen's view of transcript sync: [`StatusReport`] and [`DeleteOutcome`] mapped
//! to the three-state answer a renderer reads, with the words supplied by the app.
//!
//! This module holds no wording. Every sentence a user can read comes from the host's
//! [`StatusCopy`] (and the one message passed to [`delete_everywhere_for_renderer`]); the mapping
//! only decides which state a report is and which copy it asks for.

use std::sync::Arc;

use serde::Serialize;

use super::{DeleteOutcome, StatusReport, TranscriptSync};

/// Sync availability as the renderer reads it. Serialized as `{"state": "disabled"}`,
/// `{"state": "unavailable", "detail": ...}` or `{"state": "ready"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RendererSyncStatus {
    /// The user has not opted in on this device (the default).
    Disabled,
    /// Every answer that is not success, with the text to show.
    Unavailable { detail: Option<String> },
    /// Success: [`StatusReport::Ready`] only.
    Ready,
}

/// The words for the non-success [`StatusReport`] answers. The host owns all of them.
///
/// `StatusReport::AccountChanged` and `StatusReport::Unavailable` carry their own text and are
/// passed through unchanged.
pub trait StatusCopy: Sync {
    /// A pass is running ([`StatusReport::Syncing`]).
    fn syncing(&self) -> String;
    /// A large history is processed over several passes ([`StatusReport::Partial`]).
    fn partial(&self, passes: u32, downloaded: u64, uploaded: u64) -> String;
    /// The host waits out one backoff between passes ([`StatusReport::Paused`]).
    fn paused(&self, passes: u32, downloaded: u64, uploaded: u64) -> String;
    /// The remote change feed cannot be read to its end ([`StatusReport::FeedStalled`]).
    /// `reason` is one of `stalled`, `cycle`, `no_progress`.
    fn feed_stalled(&self, reason: &str, passes: u32, downloaded: u64, uploaded: u64) -> String;
    /// Sync is stopped but turning it off could not be saved
    /// ([`StatusReport::RevocationIncomplete`]).
    fn revocation_incomplete(&self) -> String;
    /// The evidence for `Ready` is incomplete ([`StatusReport::NotConfirmed`]). `tag` is the tag
    /// the report carries.
    fn not_confirmed(&self, tag: &str) -> String;
}

/// Map one report to the renderer's answer. Only [`StatusReport::Ready`] is a success state:
/// every other answer except `Disabled` reaches the renderer as `Unavailable` with its copy.
pub fn map_status_report(report: StatusReport, copy: &dyn StatusCopy) -> RendererSyncStatus {
    let unavailable = |detail: String| RendererSyncStatus::Unavailable {
        detail: Some(detail),
    };
    match report {
        StatusReport::Disabled => RendererSyncStatus::Disabled,
        StatusReport::Ready => RendererSyncStatus::Ready,
        StatusReport::Syncing => unavailable(copy.syncing()),
        StatusReport::Partial {
            passes,
            downloaded,
            uploaded,
        } => unavailable(copy.partial(passes, downloaded, uploaded)),
        StatusReport::Paused {
            passes,
            downloaded,
            uploaded,
            ..
        } => unavailable(copy.paused(passes, downloaded, uploaded)),
        StatusReport::FeedStalled {
            reason,
            passes,
            downloaded,
            uploaded,
        } => unavailable(copy.feed_stalled(reason, passes, downloaded, uploaded)),
        StatusReport::AccountChanged(detail) | StatusReport::Unavailable(detail) => {
            unavailable(detail)
        }
        StatusReport::RevocationIncomplete => unavailable(copy.revocation_incomplete()),
        StatusReport::NotConfirmed(tag) => unavailable(copy.not_confirmed(tag)),
    }
}

/// The settings screen's status: [`TranscriptSync::status`] mapped by [`map_status_report`].
pub async fn renderer_status(
    sync: &Arc<TranscriptSync>,
    copy: &dyn StatusCopy,
) -> Result<RendererSyncStatus, String> {
    Ok(map_status_report(sync.status().await?, copy))
}

/// The count a [`DeleteOutcome`] reports to the renderer, or the error it reports instead.
/// `Nothing` is 0 and `Synced(n)` is `n`; any other outcome is an error.
pub fn map_delete_outcome(outcome: DeleteOutcome) -> Result<u32, String> {
    match outcome {
        DeleteOutcome::Nothing => Ok(0),
        DeleteOutcome::Synced(count) => Ok(count),
        DeleteOutcome::Incomplete(state) => Err(format!("delete everywhere not complete: {state}")),
        DeleteOutcome::Deferred => Err("delete everywhere deferred".to_string()),
    }
}

/// The explicit, confirmed delete-everywhere action for the renderer.
///
/// Refuses with `sync_off_message` while sync is not enabled (with sync off there is no live
/// channel, so there is nothing to propagate), otherwise runs
/// [`TranscriptSync::delete_everywhere`] and maps its outcome with [`map_delete_outcome`].
/// Confirmation is the caller's.
pub async fn delete_everywhere_for_renderer(
    sync: &Arc<TranscriptSync>,
    sync_off_message: &str,
) -> Result<u32, String> {
    if !sync.sync_enabled() {
        return Err(sync_off_message.to_string());
    }
    map_delete_outcome(sync.delete_everywhere().await?)
}
