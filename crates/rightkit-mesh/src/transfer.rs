//! Bulk transfers on ALPN `right/bulk/1` with the D7 caps enforced on relayed paths.
//!
//! Direct first. A transfer on a direct path is never capped and never counted. Otherwise:
//! the sender waits for a direct path (default 5 s; 10 s above the relayed cap), then
//! [`check_route`] decides: within the kind's relayed cap and the circle's monthly budget the bytes
//! go through the relay, end-to-end encrypted by the QUIC session between the two devices (the
//! relay forwards ciphertext and stores nothing); over the cap, over budget, or a screen, the
//! transfer is refused with a plain sentence and the application offers "wait for a direct
//! connection". The receiver applies the same check to its own view of the path, so a sender that
//! skips it still cannot push more than the caps through the relay.
//!
//! A transfer that starts direct and falls back to the relay mid-stream beyond its cap is stopped.
//! The file moves as a plain QUIC stream: iroh-blobs (BLAKE3-verified, resumable) was not adopted
//! because its API was not read; resume is the application's job for now.

use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::caps::{Kind, PathKind, Refusal, RefusalCode, RelayBudget, Route, check_route};
use crate::endpoint::{Mesh, path_kind};
use crate::error::MeshError;
use crate::frame::{read_frame, write_frame};
use crate::identity::{PeerInfo, PeerTrust};
use crate::settings::MeshSettings;
use crate::util::{now_ms, stream_err, unhex};

pub const ALPN_BULK: &[u8] = b"right/bulk/1";
const CHUNK: usize = 64 * 1024;

/// What the sender announces before the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// 32 hex characters; unique per transfer, named in the receipt.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub size: u64,
    /// For images: `image/jpeg` or `image/webp` after the phone's downscale.
    pub mime: Option<String>,
}

/// The typed status an accepted answer carries (a distinct field, never a refusal string).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// The receiver stored the bytes and, when it keeps receipts, the receipt.
    #[default]
    Saved,
    /// The receiver already committed this exact attempt (the batch id and attempt id it names);
    /// no bytes were sent and none were stored again.
    AlreadyCommitted,
    /// The receiver stored the file but could not write its receipt, so it cannot recognise a
    /// retry of this attempt later.
    SavedNoReceipt,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    ok: bool,
    code: Option<RefusalCode>,
    message: Option<String>,
    /// Absent from an older receiver: `Saved`.
    #[serde(default)]
    status: Delivery,
    /// The offer's batch (the name up to its first `/`) and attempt (the offer id), echoed by the
    /// receiver on `AlreadyCommitted` and on the final confirmation. The sender checks both.
    #[serde(default)]
    batch: Option<String>,
    #[serde(default)]
    attempt: Option<String>,
}

/// The batch of an offer name (`<batch>/<relative path>`).
fn batch_of(name: &str) -> Option<&str> {
    name.split_once('/').map(|(batch, _)| batch)
}

impl Answer {
    fn accepted() -> Self {
        Self {
            ok: true,
            code: None,
            message: None,
            status: Delivery::Saved,
            batch: None,
            attempt: None,
        }
    }

    fn confirmed(offer: &Offer, status: Delivery) -> Self {
        Self {
            status,
            batch: batch_of(&offer.name).map(str::to_string),
            attempt: Some(offer.id.clone()),
            ..Self::accepted()
        }
    }

    fn refused(refusal: &Refusal) -> Self {
        Self {
            ok: false,
            code: Some(refusal.code),
            message: Some(refusal.message.clone()),
            status: Delivery::Saved,
            batch: None,
            attempt: None,
        }
    }
}

/// Where received bytes go. The application writes them to a temporary file and moves it into
/// place in `finish`; `abort` discards it.
pub trait BulkSink: Send {
    fn write(&mut self, chunk: &[u8]) -> Result<(), MeshError>;
    fn finish(self: Box<Self>) -> Result<(), MeshError>;
    fn abort(self: Box<Self>);

    /// [`BulkSink::finish`], also saying whether the application's durable receipt of the commit
    /// was written (`false`: the file is saved but a later retry cannot be recognised). The
    /// default has no receipt to lose.
    fn finish_with_receipt(self: Box<Self>) -> Result<bool, MeshError> {
        self.finish().map(|()| true)
    }
}

/// What an offer starts on the receiving side.
pub enum Begin {
    /// Receive the bytes into this sink.
    Sink(Box<dyn BulkSink>),
    /// This exact attempt (batch id and offer id) was committed earlier: nothing to receive.
    AlreadyCommitted,
}

/// The application's receiving side: accept or refuse an offer from a paired device.
pub trait BulkReceiver: Debug + Send + Sync + 'static {
    fn begin(&self, from: &PeerInfo, offer: &Offer) -> Result<Box<dyn BulkSink>, Refusal>;

    /// [`BulkReceiver::begin`] that may also answer "already committed". The default never does.
    fn begin_typed(&self, from: &PeerInfo, offer: &Offer) -> Result<Begin, Refusal> {
        self.begin(from, offer).map(Begin::Sink)
    }
}

/// How long to wait for a direct path to appear. Polls the connection's selected path.
pub async fn wait_direct(connection: &Connection, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        if path_kind(connection) == PathKind::Direct {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Resolves once `flag` is set (polled every 100 ms).
async fn cancelled(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The accepting side of `right/bulk/1`.
#[derive(Clone, Debug)]
pub struct BulkHandler {
    pub(crate) trust: Arc<dyn PeerTrust>,
    pub(crate) receiver: Arc<dyn BulkReceiver>,
    pub(crate) budget: Arc<Mutex<RelayBudget>>,
    pub(crate) settings: MeshSettings,
}

/// How many chunks an accepted stream moves between two checks that the sender is still a
/// trusted paired device (R09): a revoked or lapsed peer stops within this many chunks (1 MiB).
const REVALIDATE_EVERY_CHUNKS: u32 = 16;

fn not_trusted_now_refusal() -> Refusal {
    Refusal {
        code: RefusalCode::Declined,
        message: "That device is no longer paired with this computer.".to_string(),
    }
}

fn not_trusted_now() -> MeshError {
    MeshError::Refused(not_trusted_now_refusal())
}

impl BulkHandler {
    async fn serve(
        &self,
        peer: &PeerInfo,
        connection: &Connection,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<(), MeshError> {
        let remote = *connection.remote_id().as_bytes();
        let (offer, _): (Offer, _) = read_frame(recv, 0).await?;
        // Trust is read again for every stream (R09): `accept` resolved the peer once, and a
        // device revoked since must not start, or be answered for, another transfer.
        if self.trust.peer(&remote).is_none() {
            write_frame(send, &Answer::refused(&not_trusted_now_refusal()), &[]).await?;
            return Err(not_trusted_now());
        }
        unhex::<16>(&offer.id)?;
        let path = path_kind(connection);
        let now = now_ms();
        let route = {
            let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
            let remaining = budget.remaining(now);
            let route = check_route(
                offer.kind,
                offer.size,
                path,
                remaining,
                self.settings.direct_for_large,
                self.settings.relay_small_files,
            );
            match route {
                Ok(Route::Relayed) => match budget.reserve(now, offer.size) {
                    Ok(()) => Ok(Route::Relayed),
                    Err(MeshError::Refused(r)) => Err(r),
                    Err(e) => return Err(e),
                },
                other => other,
            }
        };
        let route = match route {
            Ok(route) => route,
            Err(refusal) => {
                write_frame(send, &Answer::refused(&refusal), &[]).await?;
                return Ok(());
            }
        };
        let refund = |this: &Self| {
            if route == Route::Relayed {
                let mut budget = this.budget.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = budget.refund(offer.size);
            }
        };
        // `begin` may wait for the owner's answer (an accept card), so it runs off the async
        // workers.
        let begun = {
            let receiver = self.receiver.clone();
            let (peer, offer) = (peer.clone(), offer.clone());
            tokio::task::spawn_blocking(move || receiver.begin_typed(&peer, &offer))
                .await
                .map_err(|e| MeshError::Stream(e.to_string()))?
        };
        let mut sink = match begun {
            Ok(Begin::Sink(sink)) => sink,
            Ok(Begin::AlreadyCommitted) => {
                // Typed, on this authenticated connection only: no bytes follow.
                refund(self);
                write_frame(
                    send,
                    &Answer::confirmed(&offer, Delivery::AlreadyCommitted),
                    &[],
                )
                .await?;
                return Ok(());
            }
            Err(refusal) => {
                refund(self);
                write_frame(send, &Answer::refused(&refusal), &[]).await?;
                return Ok(());
            }
        };
        if let Err(e) = write_frame(send, &Answer::accepted(), &[]).await {
            sink.abort();
            refund(self);
            return Err(e);
        }
        let mut remaining = offer.size;
        let mut buffer = vec![0u8; CHUNK];
        let over_cap = offer.size > offer.kind.relay_cap();
        let mut chunks: u32 = 0;
        while remaining > 0 {
            let want = remaining.min(CHUNK as u64) as usize;
            chunks = chunks.wrapping_add(1);
            let revoked = chunks.is_multiple_of(REVALIDATE_EVERY_CHUNKS)
                && self.trust.peer(&remote).is_none();
            let step: Result<(), MeshError> = async {
                if revoked {
                    return Err(not_trusted_now());
                }
                recv.read_exact(&mut buffer[..want]).await.map_err(stream_err)?;
                if route == Route::Direct && over_cap && path_kind(connection) != PathKind::Direct
                {
                    return Err(MeshError::Refused(Refusal {
                        code: RefusalCode::DirectRequired,
                        message: "The direct connection dropped, and this transfer is too large for the relay. It will resume when a direct connection returns.".to_string(),
                    }));
                }
                sink.write(&buffer[..want])
            }
            .await;
            if let Err(e) = step {
                sink.abort();
                refund(self);
                return Err(e);
            }
            remaining -= want as u64;
        }
        // The last check before the bytes are committed (R09).
        if self.trust.peer(&remote).is_none() {
            sink.abort();
            refund(self);
            write_frame(send, &Answer::refused(&not_trusted_now_refusal()), &[]).await?;
            return Err(not_trusted_now());
        }
        let receipt = match sink.finish_with_receipt() {
            Ok(receipt) => receipt,
            Err(e) => {
                refund(self);
                let refusal = Refusal {
                    code: RefusalCode::TooLarge,
                    message: e.to_string(),
                };
                write_frame(send, &Answer::refused(&refusal), &[]).await?;
                return Err(e);
            }
        };
        let status = if receipt {
            Delivery::Saved
        } else {
            Delivery::SavedNoReceipt
        };
        write_frame(send, &Answer::confirmed(&offer, status), &[]).await
    }
}

impl ProtocolHandler for BulkHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = *connection.remote_id().as_bytes();
        if self.trust.peer(&remote).is_none() {
            connection.close(1u32.into(), b"not a paired device");
            return Ok(());
        }
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            // Resolve again for every stream: a revoke closes the connection from the engine, and
            // this is the backstop if that close races an accepted stream (R09).
            let Some(peer) = self.trust.peer(&remote) else {
                connection.close(1u32.into(), b"not a paired device");
                break;
            };
            if self
                .serve(&peer, &connection, &mut send, &mut recv)
                .await
                .is_err()
            {
                break;
            }
            if send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// What a finished send did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendOutcome {
    pub route: Route,
    pub bytes: u64,
    /// What the receiver said: saved, already committed (no bytes sent) or saved without a receipt.
    pub delivery: Delivery,
}

impl Mesh {
    /// Send `offer.size` bytes from `data` to the paired device `peer`.
    ///
    /// Waits for a direct path first, applies the D7 caps, reserves relayed bytes against the
    /// monthly budget, and returns only after the receiver confirmed it stored the bytes (the
    /// recipient's word, not the relay's). A refusal comes back as [`MeshError::Refused`] with the
    /// sentence to show; nothing was sent and no budget was spent.
    pub async fn send_bulk(
        &self,
        peer: &[u8; 32],
        offer: Offer,
        data: impl AsyncRead + Unpin + Send,
    ) -> Result<SendOutcome, MeshError> {
        let never = AtomicBool::new(false);
        self.send_bulk_until(peer, offer, data, &never).await
    }

    /// [`Mesh::send_bulk`] that stops when `cancel` becomes true (Pulse #465): the dial, the wait
    /// for a direct path, the offer, the answer, the bytes and the receiver's final confirmation
    /// are all abandoned within about 100 ms, the connection (dialled for this one send, never
    /// shared) is closed so the receiver's read fails and discards its temporary file, a reserved
    /// relay budget is refunded, and the caller gets `MeshError::Cancelled`. The caller
    /// sets the flag for a user cancel and for its own stall or deadline limits.
    pub async fn send_bulk_until(
        &self,
        peer: &[u8; 32],
        offer: Offer,
        mut data: impl AsyncRead + Unpin + Send,
        cancel: &AtomicBool,
    ) -> Result<SendOutcome, MeshError> {
        unhex::<16>(&offer.id)?;
        let stopped = || MeshError::Cancelled;
        let connection = tokio::select! {
            connection = self.connect(peer, ALPN_BULK) => connection?,
            () = cancelled(cancel) => return Err(stopped()),
        };
        let settings = self.settings();
        let over_cap = offer.size > offer.kind.relay_cap();
        let wait = Duration::from_secs(if over_cap {
            settings.direct_wait_secs.max(10)
        } else {
            settings.direct_wait_secs
        });
        tokio::select! {
            _ = wait_direct(&connection, wait) => {}
            () = cancelled(cancel) => {
                connection.close(2u32.into(), b"cancelled");
                return Err(stopped());
            }
        }
        let path = path_kind(&connection);
        let now = now_ms();
        let route = {
            let mut budget = self.budget().lock().unwrap_or_else(PoisonError::into_inner);
            let routed = check_route(
                offer.kind,
                offer.size,
                path,
                budget.remaining(now),
                settings.direct_for_large,
                settings.relay_small_files,
            )
            .map_err(MeshError::Refused);
            let route = match routed {
                Ok(route) => route,
                Err(e) => {
                    connection.close(2u32.into(), b"refused");
                    return Err(e);
                }
            };
            if route == Route::Relayed {
                budget.reserve(now, offer.size)?;
            }
            route
        };
        let result = tokio::select! {
            result = self.stream_out(&connection, peer, &offer, route, &mut data) => result,
            () = cancelled(cancel) => {
                connection.close(2u32.into(), b"cancelled");
                Err(stopped())
            }
        };
        // Nothing was sent when the receiver already held the file: the reservation is returned.
        if route == Route::Relayed
            && !matches!(result, Ok(Delivery::Saved | Delivery::SavedNoReceipt))
        {
            let mut budget = self.budget().lock().unwrap_or_else(PoisonError::into_inner);
            let _ = budget.refund(offer.size);
        }
        result.map(|delivery| SendOutcome {
            route,
            bytes: if delivery == Delivery::AlreadyCommitted {
                0
            } else {
                offer.size
            },
            delivery,
        })
    }

    async fn stream_out(
        &self,
        connection: &Connection,
        peer: &[u8; 32],
        offer: &Offer,
        route: Route,
        data: &mut (impl AsyncRead + Unpin + Send),
    ) -> Result<Delivery, MeshError> {
        let (mut send, mut recv) = connection.open_bi().await.map_err(stream_err)?;
        write_frame(&mut send, offer, &[]).await?;
        let (answer, _): (Answer, _) = read_frame(&mut recv, 0).await?;
        if !answer.ok {
            return Err(MeshError::Refused(Refusal {
                code: answer.code.unwrap_or(RefusalCode::DirectRequired),
                message: answer.message.unwrap_or_default(),
            }));
        }
        // The receiver's typed word that it already holds this exact attempt: only from the peer
        // we dialled, and only naming our own batch and attempt.
        let genuine = |answer: &Answer| {
            connection.remote_id().as_bytes() == peer
                && answer.attempt.as_deref() == Some(offer.id.as_str())
                && answer.batch.as_deref() == batch_of(&offer.name)
        };
        if answer.status == Delivery::AlreadyCommitted {
            if genuine(&answer) {
                return Ok(Delivery::AlreadyCommitted);
            }
            return Err(MeshError::Stream(
                "the confirmation named another transfer".to_string(),
            ));
        }
        let over_cap = offer.size > offer.kind.relay_cap();
        let mut remaining = offer.size;
        let mut buffer = vec![0u8; CHUNK];
        while remaining > 0 {
            let want = remaining.min(CHUNK as u64) as usize;
            data.read_exact(&mut buffer[..want])
                .await
                .map_err(stream_err)?;
            if route == Route::Direct && over_cap && path_kind(connection) != PathKind::Direct {
                return Err(MeshError::Refused(Refusal {
                    code: RefusalCode::DirectRequired,
                    message: "The direct connection dropped, and this transfer is too large for the relay. Pulse will send it when a direct connection returns.".to_string(),
                }));
            }
            send.write_all(&buffer[..want]).await.map_err(stream_err)?;
            remaining -= want as u64;
        }
        send.finish().map_err(stream_err)?;
        let (done, _): (Answer, _) = read_frame(&mut recv, 0).await?;
        if !done.ok {
            return Err(MeshError::Refused(Refusal {
                code: done.code.unwrap_or(RefusalCode::TooLarge),
                message: done.message.unwrap_or_default(),
            }));
        }
        // An older receiver names neither id; a receiver that names them must name ours.
        if done.attempt.is_some() && !genuine(&done) || done.status == Delivery::AlreadyCommitted {
            return Err(MeshError::Stream(
                "the confirmation named another transfer".to_string(),
            ));
        }
        Ok(done.status)
    }
}
