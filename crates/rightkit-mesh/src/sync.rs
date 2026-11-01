//! Sync reconciliation by record id, on ALPN `right/sync/1`.
//!
//! "What do you have since X": on connect, a device asks its peer for the changes to a collection
//! after the last cursor it holds (`since`), compares record versions with its own, and fetches
//! only what is missing or newer. HeardRight needs this to bring a phone and a laptop to the same
//! set of records after either was offline. The protocol is app-neutral: records are opaque bodies
//! named by `(collection, id)` with a monotonically increasing version, and the application's
//! [`RecordStore`] decides what exists, what a given peer may read and how to apply a record.
//!
//! One exchange is one stream: request, listing of changes, the ids the requester wants, then one
//! frame per record. Pulling is one-directional; each side pulls from the other to converge.
//! Bodies are bounded: 64 KB when the connection is relayed, 1 MB when it is direct; a record over
//! its bound is reported skipped (`direct_required`) and the cursor stops before it, so it is
//! fetched on a later direct connection.

use std::fmt::Debug;
use std::sync::Arc;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};

use crate::caps::{PathKind, SMALL_ITEM_MAX};
use crate::endpoint::path_kind;
use crate::error::MeshError;
use crate::frame::{read_frame, write_frame};
use crate::identity::{PeerInfo, PeerTrust};
use crate::util::stream_err;

pub const ALPN_SYNC: &[u8] = b"right/sync/1";
/// Largest record body on a direct connection.
pub const DIRECT_RECORD_MAX: usize = 1024 * 1024;
/// Most changes listed, and most records fetched, per exchange.
pub const MAX_CHANGES: u32 = 100;

/// One change in a collection: the record's id, its version and whether it was deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The source store's change sequence; the requester's next `since`.
    pub seq: u64,
    pub id: String,
    pub version: u64,
    pub deleted: bool,
}

/// The application's records. Every method that names a peer lets the store decide what that peer
/// may see or write; the mesh only guarantees the peer is an active paired device.
pub trait RecordStore: Debug + Send + Sync + 'static {
    /// Changes in `collection` with a sequence greater than `since`, oldest first, at most `limit`.
    fn changes_since(
        &self,
        peer: &PeerInfo,
        collection: &str,
        since: u64,
        limit: usize,
    ) -> Vec<Change>;

    /// The sequence of the newest change in `collection` visible to `peer` (0 when empty).
    fn head(&self, peer: &PeerInfo, collection: &str) -> u64;

    /// The local version of a record, or `None` when this device has never had it.
    fn version_of(&self, collection: &str, id: &str) -> Option<u64>;

    /// The body of a record `peer` may read.
    fn get(&self, peer: &PeerInfo, collection: &str, id: &str) -> Option<Vec<u8>>;

    /// Apply a record from `peer`: `body` is `None` for a deletion. Must be idempotent.
    fn put(
        &self,
        peer: &PeerInfo,
        collection: &str,
        id: &str,
        version: u64,
        body: Option<Vec<u8>>,
    ) -> Result<(), MeshError>;
}

#[derive(Serialize, Deserialize)]
struct PullRequest {
    collection: String,
    since: u64,
    limit: u32,
}

#[derive(Serialize, Deserialize)]
struct Listing {
    head: u64,
    changes: Vec<Change>,
    more: bool,
}

#[derive(Serialize, Deserialize)]
struct Want {
    ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RecordHeader {
    id: String,
    version: u64,
    deleted: bool,
    /// Set instead of a body when the record is too big for this connection (`direct_required`).
    skipped: Option<String>,
}

/// What one pull from a peer did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullResult {
    /// The cursor to store and pass as `since` next time.
    pub next_cursor: u64,
    /// Records applied.
    pub applied: u32,
    /// Ids not fetched because the connection was relayed and the body too big.
    pub needs_direct: Vec<String>,
    /// True when another pull may return more.
    pub more: bool,
}

/// The accepting side: serves pulls from active paired devices.
#[derive(Clone, Debug)]
pub struct SyncHandler {
    trust: Arc<dyn PeerTrust>,
    store: Arc<dyn RecordStore>,
}

impl SyncHandler {
    pub fn new(trust: Arc<dyn PeerTrust>, store: Arc<dyn RecordStore>) -> Self {
        Self { trust, store }
    }

    async fn serve(
        &self,
        peer: &PeerInfo,
        path: PathKind,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<(), MeshError> {
        let (request, _): (PullRequest, _) = read_frame(recv, 0).await?;
        let limit = request.limit.clamp(1, MAX_CHANGES) as usize;
        let mut changes =
            self.store
                .changes_since(peer, &request.collection, request.since, limit + 1);
        let more = changes.len() > limit;
        changes.truncate(limit);
        let listing = Listing {
            head: self.store.head(peer, &request.collection),
            changes,
            more,
        };
        write_frame(send, &listing, &[]).await?;
        let (want, _): (Want, _) = read_frame(recv, 0).await?;
        let bound = if path == PathKind::Direct {
            DIRECT_RECORD_MAX
        } else {
            SMALL_ITEM_MAX as usize
        };
        for change in listing
            .changes
            .iter()
            .filter(|c| want.ids.contains(&c.id))
            .take(MAX_CHANGES as usize)
        {
            let body = if change.deleted {
                Some(Vec::new())
            } else {
                self.store.get(peer, &request.collection, &change.id)
            };
            let (header, body) = match body {
                Some(body) if body.len() <= bound => (
                    RecordHeader {
                        id: change.id.clone(),
                        version: change.version,
                        deleted: change.deleted,
                        skipped: None,
                    },
                    body,
                ),
                Some(_) => (
                    RecordHeader {
                        id: change.id.clone(),
                        version: change.version,
                        deleted: false,
                        skipped: Some("direct_required".to_string()),
                    },
                    Vec::new(),
                ),
                None => (
                    RecordHeader {
                        id: change.id.clone(),
                        version: change.version,
                        deleted: false,
                        skipped: Some("unavailable".to_string()),
                    },
                    Vec::new(),
                ),
            };
            write_frame(send, &header, &body).await?;
        }
        Ok(())
    }
}

impl ProtocolHandler for SyncHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = *connection.remote_id().as_bytes();
        let Some(peer) = self.trust.peer(&remote) else {
            connection.close(1u32.into(), b"not a paired device");
            return Ok(());
        };
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            let path = path_kind(&connection);
            if self.serve(&peer, path, &mut send, &mut recv).await.is_err() {
                break;
            }
            if send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// Pull one page of `collection` from the peer on `connection` (ALPN `right/sync/1`) and apply it
/// to `local`. Store `next_cursor` and call again while `more` is true.
pub async fn pull_collection(
    connection: &Connection,
    local: &dyn RecordStore,
    peer: &PeerInfo,
    collection: &str,
    since: u64,
) -> Result<PullResult, MeshError> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(stream_err)?;
    let request = PullRequest {
        collection: collection.to_string(),
        since,
        limit: MAX_CHANGES,
    };
    write_frame(&mut send, &request, &[]).await?;
    let (listing, _): (Listing, _) = read_frame(&mut recv, 0).await?;
    let wanted: Vec<&Change> = listing
        .changes
        .iter()
        .filter(|c| local.version_of(collection, &c.id) < Some(c.version))
        .collect();
    let want = Want {
        ids: wanted.iter().map(|c| c.id.clone()).collect(),
    };
    write_frame(&mut send, &want, &[]).await?;
    send.finish().map_err(stream_err)?;
    let mut result = PullResult {
        more: listing.more,
        ..PullResult::default()
    };
    let mut first_skipped: Option<u64> = None;
    for _ in 0..wanted.len() {
        let (header, body): (RecordHeader, _) = read_frame(&mut recv, DIRECT_RECORD_MAX).await?;
        let seq = listing
            .changes
            .iter()
            .find(|c| c.id == header.id)
            .map_or(0, |c| c.seq);
        if header.skipped.is_some() {
            result.needs_direct.push(header.id);
            first_skipped = Some(first_skipped.map_or(seq, |s| s.min(seq)));
            continue;
        }
        let body = if header.deleted { None } else { Some(body) };
        local.put(peer, collection, &header.id, header.version, body)?;
        result.applied += 1;
    }
    let newest = listing.changes.iter().map(|c| c.seq).max().unwrap_or(since);
    result.next_cursor = match first_skipped {
        Some(seq) => seq.saturating_sub(1).max(since),
        None => newest.max(since),
    };
    result.more = listing.more || first_skipped.is_some();
    Ok(result)
}
