//! The device side of `rk-relay/1`: authenticate to the relay, seal and deposit messages, take
//! delivery, drop duplicates, acknowledge. Frame transport is the caller's; this holds the logic
//! and the rules a device must follow so the relay stays blind.

use super::hub::RelayLimits;
use super::{
    auth_message, ticket_message, CircleId, ClientFrame, Class, MailboxId, ServerFrame,
};
use crate::remote::util::hex;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::{HashSet, VecDeque};

/// How many delivered ids a session remembers to drop duplicates.
const SEEN_LIMIT: usize = 4096;
/// Most ids sent in one `Ack`.
const ACK_BATCH: usize = 100;

/// A sealing failure reported by a [`Sealer`].
#[derive(Debug)]
pub struct SealError(pub String);

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "seal: {}", self.0)
    }
}
impl std::error::Error for SealError {}

/// The end-to-end cipher between two devices. The relay never sees what this produces beyond its
/// length. The caller supplies it: the concrete suite and key agreement are a design decision
/// still open (see the CHANGELOG). An implementation must authenticate the sender inside the
/// sealed bytes (the `from` mailbox the relay reports is routing information, not proof) and
/// must be safe against replay of a sealed message (this module drops duplicate ids, which is
/// not a substitute).
pub trait Sealer {
    fn seal(&self, to: &MailboxId, plaintext: &[u8]) -> Result<Vec<u8>, SealError>;
    fn open(&self, from: &MailboxId, sealed: &[u8]) -> Result<Vec<u8>, SealError>;
}

/// Why a device-side call failed.
#[derive(Debug)]
pub enum ClientError {
    /// The class is direct only ([`Class::Screen`]); nothing was sealed or sent.
    DirectOnly,
    /// The sealed message is larger than the class allows through the relay.
    TooLarge { len: usize, cap: usize },
    Seal(SealError),
    /// The OS random source failed.
    Random,
    /// The key and the credential name different devices.
    KeyMismatch,
    /// The relay sent something this state cannot accept.
    Protocol(&'static str),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::DirectOnly => f.write_str("this class is direct only; the relay refuses it"),
            ClientError::TooLarge { len, cap } => {
                write!(f, "sealed message is {len} bytes, the relay cap is {cap}")
            }
            ClientError::Seal(e) => write!(f, "{e}"),
            ClientError::Random => f.write_str("the OS random source failed"),
            ClientError::KeyMismatch => f.write_str("the credential is for a different device key"),
            ClientError::Protocol(what) => write!(f, "relay protocol: {what}"),
        }
    }
}
impl std::error::Error for ClientError {}

/// A device's authority to use the relay: its key, the circle owner's key, an expiry and the
/// owner's signature over all three ([`issue_ticket`]).
#[derive(Clone, Debug)]
pub struct DeviceCredential {
    pub owner_pub: [u8; 32],
    pub device_pub: [u8; 32],
    /// Unix seconds.
    pub exp: u64,
    pub ticket_sig: Vec<u8>,
}

fn public_of(pair: &Ed25519KeyPair) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(pair.public_key().as_ref());
    out
}

/// Sign a ticket for `device_pub` that the relay accepts until `exp` (unix seconds). Run by the
/// circle owner's device; the ticket goes to the device over the pairing channel. The relay's
/// own `max_ticket_ttl_secs` limits how far ahead `exp` may be.
pub fn issue_ticket(owner: &Ed25519KeyPair, device_pub: &[u8; 32], exp: u64) -> DeviceCredential {
    let owner_pub = public_of(owner);
    let ticket_sig = owner
        .sign(&ticket_message(&owner_pub, device_pub, exp))
        .as_ref()
        .to_vec();
    DeviceCredential {
        owner_pub,
        device_pub: *device_pub,
        exp,
        ticket_sig,
    }
}

/// A message taken from the relay and opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub id: [u8; 16],
    /// Routing information from the relay; authenticate the sender from the opened content.
    pub from: MailboxId,
    pub class: Class,
    pub plaintext: Vec<u8>,
}

/// What one relay frame meant for the device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// Send this frame to the relay (the answer to its challenge).
    Reply(ClientFrame),
    Authed,
    Message(Delivery),
    /// A message already delivered earlier; it is re-acknowledged and not shown again.
    Duplicate,
    /// A message that would not open; it is acknowledged so it does not block the queue.
    Undecryptable { id: [u8; 16] },
    Stored { id: [u8; 16], seq: u64 },
    Acked(u32),
    /// The relay refused something. `code` is one of [`crate::relay::code`].
    Failed {
        code: String,
        message: String,
        retry_after: Option<u64>,
    },
    Pong,
}

/// One device's session with the relay.
pub struct RelaySession<S: Sealer> {
    key: Ed25519KeyPair,
    credential: DeviceCredential,
    sealer: S,
    limits: RelayLimits,
    seen: HashSet<[u8; 16]>,
    order: VecDeque<[u8; 16]>,
    to_ack: Vec<String>,
}

impl<S: Sealer> RelaySession<S> {
    /// `limits` should match the relay's (`RelayLimits::default()` for the shared relay): the
    /// device checks caps before sealing so it never uploads what the relay will refuse.
    pub fn new(
        key: Ed25519KeyPair,
        credential: DeviceCredential,
        sealer: S,
        limits: RelayLimits,
    ) -> Result<Self, ClientError> {
        if public_of(&key) != credential.device_pub {
            return Err(ClientError::KeyMismatch);
        }
        Ok(Self {
            key,
            credential,
            sealer,
            limits,
            seen: HashSet::new(),
            order: VecDeque::new(),
            to_ack: Vec::new(),
        })
    }

    pub fn mailbox(&self) -> MailboxId {
        MailboxId::of_device(&self.credential.device_pub)
    }

    pub fn circle(&self) -> CircleId {
        CircleId::of_owner(&self.credential.owner_pub)
    }

    /// Seal `plaintext` for `to` and build the `Put` frame. Returns the message id (hex) so the
    /// caller can match the relay's `Stored`. A [`Class::Screen`] message is refused here.
    pub fn put(
        &self,
        to: &MailboxId,
        class: Class,
        plaintext: &[u8],
    ) -> Result<(String, ClientFrame), ClientError> {
        let cap = self.limits.cap(class).ok_or(ClientError::DirectOnly)?;
        let sealed = self
            .sealer
            .seal(to, plaintext)
            .map_err(ClientError::Seal)?;
        if sealed.len() > cap {
            return Err(ClientError::TooLarge {
                len: sealed.len(),
                cap,
            });
        }
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|_| ClientError::Random)?;
        let id_hex = hex(&id);
        Ok((
            id_hex.clone(),
            ClientFrame::Put {
                id: id_hex,
                to: to.to_hex(),
                class,
                body: STANDARD.encode(sealed),
            },
        ))
    }

    /// Ask for queued messages (after authenticating, and again after a reconnect).
    pub fn fetch(&self, limit: u32) -> ClientFrame {
        ClientFrame::Fetch { limit }
    }

    /// The acknowledgement for everything delivered since the last call, if any. Send it after
    /// the messages were handled; an unacknowledged message is delivered again next time.
    pub fn take_ack(&mut self) -> Option<ClientFrame> {
        if self.to_ack.is_empty() {
            return None;
        }
        let take = self.to_ack.len().min(ACK_BATCH);
        let ids: Vec<String> = self.to_ack.drain(..take).collect();
        Some(ClientFrame::Ack { ids })
    }

    fn remember(&mut self, id: [u8; 16]) {
        if self.seen.insert(id) {
            self.order.push_back(id);
            if self.order.len() > SEEN_LIMIT {
                if let Some(old) = self.order.pop_front() {
                    self.seen.remove(&old);
                }
            }
        }
    }

    /// Take one frame from the relay.
    pub fn handle(&mut self, frame: ServerFrame) -> Result<Event, ClientError> {
        match frame {
            ServerFrame::Challenge { nonce } => {
                let nonce = super::unhex_array::<32>(&nonce)
                    .ok_or(ClientError::Protocol("malformed challenge"))?;
                let proof = self
                    .key
                    .sign(&auth_message(&nonce, &self.credential.device_pub));
                Ok(Event::Reply(ClientFrame::Auth {
                    owner_pub: hex(&self.credential.owner_pub),
                    device_pub: hex(&self.credential.device_pub),
                    exp: self.credential.exp,
                    ticket_sig: hex(&self.credential.ticket_sig),
                    challenge_sig: hex(proof.as_ref()),
                }))
            }
            ServerFrame::Authed { mailbox, circle } => {
                if MailboxId::from_hex(&mailbox) != Some(self.mailbox())
                    || CircleId::from_hex(&circle) != Some(self.circle())
                {
                    return Err(ClientError::Protocol("relay named a different mailbox"));
                }
                Ok(Event::Authed)
            }
            ServerFrame::Stored { id, seq } => {
                let id = super::unhex_array::<16>(&id)
                    .ok_or(ClientError::Protocol("malformed id"))?;
                Ok(Event::Stored { id, seq })
            }
            ServerFrame::Deliver {
                id,
                from,
                class,
                body,
                ..
            } => {
                let id_bytes = super::unhex_array::<16>(&id)
                    .ok_or(ClientError::Protocol("malformed id"))?;
                let from = MailboxId::from_hex(&from)
                    .ok_or(ClientError::Protocol("malformed sender"))?;
                if self.seen.contains(&id_bytes) {
                    self.to_ack.push(id);
                    return Ok(Event::Duplicate);
                }
                let sealed = STANDARD
                    .decode(body)
                    .map_err(|_| ClientError::Protocol("body is not base64"))?;
                self.to_ack.push(id);
                self.remember(id_bytes);
                match self.sealer.open(&from, &sealed) {
                    Ok(plaintext) => Ok(Event::Message(Delivery {
                        id: id_bytes,
                        from,
                        class,
                        plaintext,
                    })),
                    Err(_) => Ok(Event::Undecryptable { id: id_bytes }),
                }
            }
            ServerFrame::Acked { count } => Ok(Event::Acked(count)),
            ServerFrame::Error {
                code,
                message,
                retry_after,
            } => Ok(Event::Failed {
                code,
                message,
                retry_after,
            }),
            ServerFrame::Pong => Ok(Event::Pong),
        }
    }
}
