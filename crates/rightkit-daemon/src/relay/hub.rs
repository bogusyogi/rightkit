//! The relay's state machine. It owns no sockets and no clock: the adapter that carries frames
//! calls [`RelayCore::connect`] when a connection opens, [`RelayCore::handle`] for each decoded
//! frame with the current time, [`RelayCore::disconnect`] when it closes and
//! [`RelayCore::expire`] now and then, and writes out what comes back.

use super::{
    auth_message, code, ticket_message, CircleId, ClientFrame, Class, MailboxId, ServerFrame,
};
use crate::remote::util::{hex, unhex};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ring::signature::{UnparsedPublicKey, ED25519};
use std::collections::{HashMap, HashSet, VecDeque};

/// A connection id chosen by the adapter.
pub type ConnId = u64;

const MONTH_SECS: u64 = 30 * 24 * 3600;
const MIB: usize = 1024 * 1024;

/// Bounds the relay enforces. The defaults come from the 2026-10-16 decisions (25 MiB per
/// relayed transfer, 10 MiB per clipboard item or attachment, 250 MiB per circle per month); the
/// rest are conservative choices that are plain configuration.
#[derive(Clone, Debug)]
pub struct RelayLimits {
    /// Cap for [`Class::Small`] bodies.
    pub max_small: usize,
    /// Cap for [`Class::Clipboard`] bodies.
    pub max_clipboard: usize,
    /// Cap for [`Class::Transfer`] bodies.
    pub max_transfer: usize,
    /// Bytes one circle may deposit per 30-day bucket.
    pub circle_month_bytes: u64,
    /// Messages one (mailbox, circle) queue may hold.
    pub queue_max_messages: usize,
    /// Bytes one (mailbox, circle) queue may hold.
    pub queue_max_bytes: usize,
    /// How long an undelivered message is kept, seconds.
    pub message_ttl_secs: u64,
    /// Furthest a ticket's expiry may lie in the future, seconds.
    pub max_ticket_ttl_secs: u64,
    /// How long a connection has to answer its challenge, seconds.
    pub challenge_ttl_secs: u64,
    /// Connections allowed to wait for authentication at once.
    pub max_unauthenticated: usize,
    /// Messages one `Fetch` may return at most.
    pub max_fetch: u32,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            max_small: 64 * 1024,
            max_clipboard: 10 * MIB,
            max_transfer: 25 * MIB,
            circle_month_bytes: 250 * MIB as u64,
            queue_max_messages: 1000,
            queue_max_bytes: 50 * MIB,
            message_ttl_secs: 7 * 24 * 3600,
            max_ticket_ttl_secs: 7 * 24 * 3600,
            challenge_ttl_secs: 10,
            max_unauthenticated: 1024,
            max_fetch: 100,
        }
    }
}

impl RelayLimits {
    pub(crate) fn cap(&self, class: Class) -> Option<usize> {
        match class {
            Class::Small => Some(self.max_small),
            Class::Clipboard => Some(self.max_clipboard),
            Class::Transfer => Some(self.max_transfer),
            Class::Screen => None,
        }
    }
}

/// Why [`RelayCore::connect`] refused.
#[derive(Debug, Eq, PartialEq)]
pub enum RelayError {
    /// Too many connections are waiting for authentication.
    TooManyUnauthenticated,
    /// The OS random source failed.
    Random,
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::TooManyUnauthenticated => f.write_str("too many unauthenticated connections"),
            RelayError::Random => f.write_str("the OS random source failed"),
        }
    }
}
impl std::error::Error for RelayError {}

/// What the adapter must do after a call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outgoing {
    Send(ConnId, ServerFrame),
    /// Close this connection after sending what precedes it.
    Close(ConnId),
}

struct Stored {
    id: [u8; 16],
    seq: u64,
    from: MailboxId,
    class: Class,
    body: Vec<u8>,
    expires_at: u64,
}

#[derive(Default)]
struct Queue {
    messages: VecDeque<Stored>,
    bytes: usize,
}

struct Session {
    mailbox: MailboxId,
    circle: CircleId,
    ticket_exp: u64,
}

struct Conn {
    nonce: [u8; 32],
    issued_at: u64,
    session: Option<Session>,
    /// Ids already delivered on this connection.
    delivered: HashSet<[u8; 16]>,
}

/// The relay. See the module docs.
pub struct RelayCore {
    limits: RelayLimits,
    conns: HashMap<ConnId, Conn>,
    queues: HashMap<(MailboxId, CircleId), Queue>,
    /// Bytes deposited per circle per 30-day bucket.
    usage: HashMap<(CircleId, u64), u64>,
    next_seq: u64,
}

fn verify(public: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ED25519, public)
        .verify(message, signature)
        .is_ok()
}

fn array<const N: usize>(text: &str) -> Option<[u8; N]> {
    super::unhex_array::<N>(text)
}

impl RelayCore {
    pub fn new(limits: RelayLimits) -> Self {
        Self {
            limits,
            conns: HashMap::new(),
            queues: HashMap::new(),
            usage: HashMap::new(),
            next_seq: 1,
        }
    }

    pub fn limits(&self) -> &RelayLimits {
        &self.limits
    }

    /// Open a connection. Returns the challenge frame to send to it.
    pub fn connect(&mut self, conn: ConnId, now: u64) -> Result<ServerFrame, RelayError> {
        let waiting = self.conns.values().filter(|c| c.session.is_none()).count();
        if waiting >= self.limits.max_unauthenticated {
            return Err(RelayError::TooManyUnauthenticated);
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|_| RelayError::Random)?;
        self.conns.insert(
            conn,
            Conn {
                nonce,
                issued_at: now,
                session: None,
                delivered: HashSet::new(),
            },
        );
        Ok(ServerFrame::Challenge { nonce: hex(&nonce) })
    }

    pub fn disconnect(&mut self, conn: ConnId) {
        self.conns.remove(&conn);
    }

    /// Bytes currently queued, for metrics.
    pub fn queued_bytes(&self) -> usize {
        self.queues.values().map(|q| q.bytes).sum()
    }

    /// Handle one frame from `conn`.
    pub fn handle(&mut self, conn: ConnId, now: u64, frame: ClientFrame) -> Vec<Outgoing> {
        let Some(state) = self.conns.get(&conn) else {
            return vec![Outgoing::Close(conn)];
        };
        match frame {
            ClientFrame::Ping => vec![Outgoing::Send(conn, ServerFrame::Pong)],
            ClientFrame::Auth {
                owner_pub,
                device_pub,
                exp,
                ticket_sig,
                challenge_sig,
            } => {
                if state.session.is_some() {
                    return fail(conn, code::BAD_FRAME, "already authenticated");
                }
                self.authenticate(conn, now, &owner_pub, &device_pub, exp, &ticket_sig, &challenge_sig)
            }
            other => {
                let Some(session) = &state.session else {
                    return fail(conn, code::UNAUTHENTICATED, "authenticate first");
                };
                if session.ticket_exp <= now {
                    return fail(conn, code::EXPIRED, "ticket expired");
                }
                let (mailbox, circle) = (session.mailbox, session.circle);
                match other {
                    ClientFrame::Put { id, to, class, body } => {
                        self.put(conn, now, mailbox, circle, &id, &to, class, &body)
                    }
                    ClientFrame::Fetch { limit } => self.fetch(conn, now, mailbox, circle, limit),
                    ClientFrame::Ack { ids } => self.ack(conn, mailbox, circle, &ids),
                    ClientFrame::Ping | ClientFrame::Auth { .. } => Vec::new(),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn authenticate(
        &mut self,
        conn: ConnId,
        now: u64,
        owner_pub: &str,
        device_pub: &str,
        exp: u64,
        ticket_sig: &str,
        challenge_sig: &str,
    ) -> Vec<Outgoing> {
        let (nonce, issued_at) = match self.conns.get(&conn) {
            Some(c) => (c.nonce, c.issued_at),
            None => return vec![Outgoing::Close(conn)],
        };
        if now.saturating_sub(issued_at) > self.limits.challenge_ttl_secs {
            return fail(conn, code::EXPIRED, "challenge expired");
        }
        let (Some(owner), Some(device), Some(ticket), Some(proof)) = (
            array::<32>(owner_pub),
            array::<32>(device_pub),
            unhex(ticket_sig),
            unhex(challenge_sig),
        ) else {
            return fail(conn, code::BAD_FRAME, "malformed auth");
        };
        if exp <= now || exp - now > self.limits.max_ticket_ttl_secs {
            return fail(conn, code::EXPIRED, "ticket expiry out of range");
        }
        // Both checks always run, so a failure does not reveal which part was wrong.
        let ticket_ok = verify(&owner, &ticket_message(&owner, &device, exp), &ticket);
        let proof_ok = verify(&device, &auth_message(&nonce, &device), &proof);
        if !(ticket_ok & proof_ok) {
            return fail(conn, code::AUTH_FAILED, "authentication failed");
        }
        let mailbox = MailboxId::of_device(&device);
        let circle = CircleId::of_owner(&owner);
        if let Some(c) = self.conns.get_mut(&conn) {
            c.session = Some(Session {
                mailbox,
                circle,
                ticket_exp: exp,
            });
        }
        vec![Outgoing::Send(
            conn,
            ServerFrame::Authed {
                mailbox: mailbox.to_hex(),
                circle: circle.to_hex(),
            },
        )]
    }

    #[allow(clippy::too_many_arguments)]
    fn put(
        &mut self,
        conn: ConnId,
        now: u64,
        from: MailboxId,
        circle: CircleId,
        id: &str,
        to: &str,
        class: Class,
        body: &str,
    ) -> Vec<Outgoing> {
        let (Some(id), Some(to)) = (array::<16>(id), MailboxId::from_hex(to)) else {
            return reply_error(conn, code::BAD_FRAME, "malformed put");
        };
        let Some(cap) = self.limits.cap(class) else {
            return reply_error(conn, code::DIRECT_ONLY, "this class is direct only");
        };
        // Reject on the encoded length before allocating the decoded body.
        if body.len() > cap.div_ceil(3) * 4 {
            return reply_error(conn, code::TOO_LARGE, "body exceeds the class cap");
        }
        let Ok(bytes) = STANDARD.decode(body) else {
            return reply_error(conn, code::BAD_FRAME, "body is not base64");
        };
        if bytes.is_empty() {
            return reply_error(conn, code::BAD_FRAME, "empty body");
        }
        if bytes.len() > cap {
            return reply_error(conn, code::TOO_LARGE, "body exceeds the class cap");
        }
        let key = (to, circle);
        // A retry of a stored message is answered with its first sequence number.
        if let Some(existing) = self
            .queues
            .get(&key)
            .and_then(|q| q.messages.iter().find(|m| m.id == id && m.from == from))
        {
            return vec![Outgoing::Send(
                conn,
                ServerFrame::Stored {
                    id: hex(&id),
                    seq: existing.seq,
                },
            )];
        }
        let bucket = now / MONTH_SECS;
        let used = self.usage.get(&(circle, bucket)).copied().unwrap_or(0);
        if used.saturating_add(bytes.len() as u64) > self.limits.circle_month_bytes {
            return vec![Outgoing::Send(
                conn,
                ServerFrame::Error {
                    code: code::QUOTA_EXCEEDED.into(),
                    message: "the circle's monthly relay allowance is used".into(),
                    retry_after: Some((bucket + 1) * MONTH_SECS - now),
                },
            )];
        }
        let queue = self.queues.entry(key).or_default();
        if queue.messages.len() >= self.limits.queue_max_messages
            || queue.bytes + bytes.len() > self.limits.queue_max_bytes
        {
            return reply_error(conn, code::QUEUE_FULL, "the recipient's queue is full");
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        queue.bytes += bytes.len();
        let deliver = ServerFrame::Deliver {
            id: hex(&id),
            seq,
            from: from.to_hex(),
            class,
            body: body.to_string(),
        };
        queue.messages.push_back(Stored {
            id,
            seq,
            from,
            class,
            body: bytes.clone(),
            expires_at: now.saturating_add(self.limits.message_ttl_secs),
        });
        *self.usage.entry((circle, bucket)).or_insert(0) += bytes.len() as u64;
        let mut out = vec![Outgoing::Send(
            conn,
            ServerFrame::Stored { id: hex(&id), seq },
        )];
        // Live delivery to a recipient that is connected now; the message stays queued until
        // the recipient acknowledges it.
        for (other_id, other) in self.conns.iter_mut() {
            let live = other
                .session
                .as_ref()
                .is_some_and(|s| s.mailbox == to && s.circle == circle);
            if live && other.delivered.insert(id) {
                out.push(Outgoing::Send(*other_id, deliver.clone()));
            }
        }
        out
    }

    fn fetch(
        &mut self,
        conn: ConnId,
        now: u64,
        mailbox: MailboxId,
        circle: CircleId,
        limit: u32,
    ) -> Vec<Outgoing> {
        let limit = limit.clamp(1, self.limits.max_fetch) as usize;
        let Some(queue) = self.queues.get(&(mailbox, circle)) else {
            return Vec::new();
        };
        let Some(state) = self.conns.get_mut(&conn) else {
            return vec![Outgoing::Close(conn)];
        };
        let mut out = Vec::new();
        for m in queue.messages.iter() {
            if out.len() >= limit {
                break;
            }
            if m.expires_at <= now || !state.delivered.insert(m.id) {
                continue;
            }
            out.push(Outgoing::Send(
                conn,
                ServerFrame::Deliver {
                    id: hex(&m.id),
                    seq: m.seq,
                    from: m.from.to_hex(),
                    class: m.class,
                    body: STANDARD.encode(&m.body),
                },
            ));
        }
        out
    }

    fn ack(
        &mut self,
        conn: ConnId,
        mailbox: MailboxId,
        circle: CircleId,
        ids: &[String],
    ) -> Vec<Outgoing> {
        let wanted: HashSet<[u8; 16]> = ids.iter().filter_map(|i| array::<16>(i)).collect();
        let key = (mailbox, circle);
        let mut removed = 0u32;
        if let Some(queue) = self.queues.get_mut(&key) {
            let mut kept = VecDeque::with_capacity(queue.messages.len());
            for m in queue.messages.drain(..) {
                if wanted.contains(&m.id) {
                    queue.bytes -= m.body.len();
                    removed += 1;
                } else {
                    kept.push_back(m);
                }
            }
            queue.messages = kept;
            if queue.messages.is_empty() {
                self.queues.remove(&key);
            }
        }
        vec![Outgoing::Send(conn, ServerFrame::Acked { count: removed })]
    }

    /// Drop expired messages, stale unauthenticated connections and old usage buckets. Returns
    /// the connections to close.
    pub fn expire(&mut self, now: u64) -> Vec<Outgoing> {
        for queue in self.queues.values_mut() {
            let mut bytes = 0;
            queue.messages.retain(|m| {
                let keep = m.expires_at > now;
                if keep {
                    bytes += m.body.len();
                }
                keep
            });
            queue.bytes = bytes;
        }
        self.queues.retain(|_, q| !q.messages.is_empty());
        let bucket = now / MONTH_SECS;
        self.usage.retain(|(_, b), _| *b >= bucket);
        let ttl = self.limits.challenge_ttl_secs;
        let stale: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.session.is_none() && now.saturating_sub(c.issued_at) > ttl)
            .map(|(id, _)| *id)
            .collect();
        stale
            .into_iter()
            .map(|id| {
                self.conns.remove(&id);
                Outgoing::Close(id)
            })
            .collect()
    }
}

fn fail(conn: ConnId, error: &str, message: &str) -> Vec<Outgoing> {
    vec![
        Outgoing::Send(conn, ServerFrame::error(error, message)),
        Outgoing::Close(conn),
    ]
}

fn reply_error(conn: ConnId, error: &str, message: &str) -> Vec<Outgoing> {
    vec![Outgoing::Send(conn, ServerFrame::error(error, message))]
}
