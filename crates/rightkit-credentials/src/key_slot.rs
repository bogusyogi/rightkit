//! One key credential that other state depends on (a database key): a read that keeps "the
//! store says there is none" apart from "the store could not be asked", an optional in-process
//! copy, a write whose outcome is typed (succeeded, definitely failed, never started, or
//! indeterminate), a settle read, and get-or-create.
//!
//! A caller must never treat an `Err` of [`KeySlot::state`] as absence: minting a fresh key, or
//! moving the database aside, on a transient read failure (a locked keychain, a denied prompt, a
//! busy slot, a timeout) would strand data the real key can still unlock. Only `Ok(None)` means the
//! credential is authoritatively absent.
//!
//! The namespace, the account, the interaction mode and the guard are the host's: it hands in the
//! [`Store`] it wants. This module names no product and no coordinate, and never prints, logs or
//! formats a value. Error text is fixed wording plus the owner's [`crate::Error`] text (no variant
//! carries a value).
//!
//! ## What is shared and what is not
//!
//! Handles for the same native credential share COORDINATION and nothing else. Nothing a handle
//! reads, writes or mints through is taken from another handle.
//!
//! | | Where it lives | Set by |
//! | --- | --- | --- |
//! | busy flag, closed flag, unsettled flag, version stamp, in-process copy (with the access it was read under) | the registry record (`Shared`), one per logical slot | any handle's sequence, under the record's lock |
//! | the registry key (`SlotKey`) and the [`KeySlotPolicy`] | the registry record | the first handle to register the key; a later handle must match the policy |
//! | pinned [`Store`] (backend binding, namespace, guard, timeout) | the handle (`Binding`) | that handle's [`KeySlot::new`] |
//! | account string | the handle | that handle's [`KeySlot::new`] |
//! | access (blob codec, guard) | the handle | derived from the handle's own store in [`KeySlot::new`] |
//!
//! The record also keeps one clone of the FIRST registering handle's store, named
//! `_identity_anchor`. No operation reads it: it exists only so that the allocation behind an
//! `instance:` identity (`store.rs:251-255`) cannot be freed and its address reused by another
//! backend while the registry entry exists (entries are never removed). It confers no right on any
//! handle.
//!
//! ## One pinned store per handle
//!
//! [`KeySlot::new`] resolves the host's store with the owner's [`Store::pinned`] (`store.rs:394-400`)
//! and keeps that pinned `Store` in the handle's own `Binding`. The same binding is used for all
//! three things that must agree on which physical store is meant:
//!
//! * the registry identity (`SlotKey`, below), computed by `Store::mutation_target` on that pinned
//!   store with that handle's account;
//! * every operation of that handle: [`KeySlot::state`], [`KeySlot::persist`],
//!   [`KeySlot::get_or_create`] and [`KeySlot::holds`] run on a clone of the handle's pinned store
//!   (only its interaction mode is set per call), so a default keychain that changes during the
//!   process cannot move a read or a write to another keychain, and a write is subject to exactly
//!   the restrictions of the handle's own backend and namespace (below);
//! * the in-flight lookup (`Store::mutation_in_flight`, `store.rs:533-536`) of that handle's own
//!   pinned store, which names the entry the owner's `Store::run` claimed for a write
//!   (`store.rs:564-576`: pinned store identity and native credential identity).
//!
//! Every operation first re-checks that the handle's own binding still resolves to the key of the
//! record it registered under (`Op::bound`: `mutation_target` on the handle's store and account
//! equals the record's `SlotKey`, no credential access). If it does not, the operation fails closed
//! before any read, write or mint: it is not the slot it claims to be.
//!
//! No identity is derived from a backend name, and no secret is read to derive one. Resolving the
//! pinned store can fail (the macOS user default keychain cannot be resolved or named) or take an
//! OS call (`SecKeychainCopyDomainDefault`, `SecKeychainGetPath`, a path canonicalisation: no
//! credential access, no prompt). That happens only inside [`KeySlot::new`], on a bounded
//! resolver thread, and ends in a typed [`OpenError`]; a handle that could not be bound does not
//! exist and nothing reads, writes or mints through it.
//!
//! ## One coordination point per native credential (process-wide registry)
//!
//! The registry key is `SlotKey { store, credential }`, both parts taken from the handle's PINNED
//! store through `Store::mutation_target` (`store.rs:523-529`: the owner's exclusion key for that
//! store: its `Backend::exclusion_identity`, else its allocation, and
//! `Backend::credential_identity`):
//!
//! | Backend (as pinned) | store part | credential part |
//! | --- | --- | --- |
//! | Windows Credential Manager | `windows-credential-manager` (`windows.rs:359-362`) | [`crate::windows_credential_identity`]: `windows:` + the validated Credential Manager target, normalised by `namespace.rs::normalize_target_name` (per-character upper then lower case) because Windows compares target names case-insensitively (the owner's documented rule; not re-verified against the OS here) |
//! | macOS Keychain, user default (pinned to the keychain file the default is at construction) | `macos-keychain-file:` + the canonical path of that file (`macos.rs:205-209`, `:217-225`) | [`crate::macos_credential_identity`]: `macos:` + the folded service and account |
//! | macOS Keychain, explicit file (read and delete only; a write is refused by the owner, `macos.rs:161-163`, `:283-288`) | the same `macos-keychain-file:` + canonical path (`macos.rs:517`) | the same macOS identity |
//! | Linux Secret Service | `linux-secret-service` (`linux.rs:259-262`) | the owner's `linux:` service and account, exact (no folding) |
//! | an injected backend | its `exclusion_identity`, else `instance:` + the address of its `Arc` (the owner's fallback, `store.rs:251-255`) | its own `credential_identity` |
//!
//! What is therefore one slot: a Windows target spelled in two cases, or reached through two
//! namespaces whose prefix and account give the same target; on macOS two namespaces with the same
//! folded service and account in the same keychain file; the user default and an explicit file that
//! are the same keychain file. What is therefore two slots: the same account in two Windows
//! prefixes; two keychain files (including a user default that has moved to another file between
//! two constructions); a Windows slot and a macOS slot (they never meet in one process).
//!
//! Two separately opened injected backends with no `exclusion_identity` are two slots even if they
//! front one external store: a host that does that must give its backend an `exclusion_identity`,
//! as the owner's `Backend` documentation requires.
//!
//! [`KeySlot::new`] returns a handle that is bound to the handle's OWN pinned store and joined to
//! the existing record when the key is already registered, else registers a new record. Two
//! handles for the same native credential therefore hold the same `Arc<Shared>`: there is one busy
//! flag, one closed flag and one copy. Their `Binding`s are separate: each runs on its own store,
//! account and access. A handle is two `Arc`s and clones freely (a clone is the same handle, with
//! the same binding).
//!
//! A second `new` for a key that is already registered must pass the same [`KeySlotPolicy`]; a
//! different policy is [`OpenError::PolicyMismatch`] (the same slot cannot have two policies).
//!
//! Registry bound: at most [`MAX_SLOTS`] entries, each about a few hundred bytes. A key is made
//! only from a namespace the owner has validated and an account the owner's `check_account` has
//! validated; nothing a remote party or a user types reaches it, and a new key past the cap is
//! [`OpenError::Full`]. Entries are never removed (a slot lives for the process), and the cap is
//! what bounds them.
//!
//! ## The in-process copy and access
//!
//! The copy is shared coordination state, but it is never served on the strength of the record
//! alone. Each copy records the `Access` of the handle whose read, mint or write produced it, and
//! a handle is served the copy only when ALL of these hold: the policy has
//! [`KeySlotPolicy::cache_in_process`]; the slot is open and settled; the handle's own `Access` is
//! equal to the recorded one; the handle's own store has no guard active. Otherwise the handle
//! reads through its own store (and a successful read replaces the copy with one recorded under
//! its own access).
//!
//! `Access` includes a unique token for this verified binding as well as its codec and guard.
//! Only clones of that binding share its cached result. Distinct backend instances, backend
//! wrappers (including chunk decoding), namespaces and accounts must each read through their own
//! store, even when their physical exclusion identities match. A physical identity establishes
//! shared mutation exclusion, not equivalent read permissions or equivalent returned bytes.
//! Cache replacement still invalidates the previous binding's copy under the shared stamp.
//!
//! ## Binding is bounded
//!
//! `Store::pinned` is a synchronous call with no timeout of its own. [`KeySlot::new`] therefore
//! runs the resolution (`Store::pinned`, then `Store::mutation_target`) on a dedicated thread and
//! waits `resolve_wait` (`1 ms..=`[`MAX_WAIT`], the host's input, no default). On expiry it
//! returns [`OpenError::TimedOut`]; the thread is abandoned, never joined, and whatever it
//! produces afterwards is dropped (nothing is registered by it). At most [`MAX_RESOLVING`]
//! resolver threads exist at once in the process (more is [`OpenError::Busy`]), so a stuck OS call
//! cannot be turned into an unbounded number of threads by retrying. A failed or timed-out
//! `new` changes nothing; the host may call it again. No operation of a handle calls
//! `mutation_target` on the caller's thread: `Op::bound` runs on the worker.
//!
//! ## One sequence at a time, on a worker
//!
//! [`KeySlot::state`], [`KeySlot::persist`], [`KeySlot::holds`] and [`KeySlot::get_or_create`]
//! each run their whole sequence (binding check, copy check, store read, mint or write, copy
//! update) on a dedicated worker thread while the slot's `busy` flag is set. The flag is taken by
//! the calling thread before the worker starts and is cleared by the worker when the sequence ends,
//! or if the worker panics or cannot be started; a caller that stops waiting does not clear it. The
//! flag belongs to the record, so a sequence of one handle excludes every other handle's.
//!
//! The in-process state is one `Mutex` that is held for field reads and assignments only and never
//! across a store call.
//!
//! ## Waiting is always bounded, and a caller chooses how
//!
//! Every blocking method takes a [`Call`]:
//!
//! * `slot_wait`: how long to wait for another operation on the same slot to finish before giving
//!   up with `Busy`. `0` never waits.
//! * `result_wait`: how long to wait for this caller's own operation once started. On expiry a
//!   read gets [`SlotError::TimedOut`] and a WRITE gets [`PersistError::Indeterminate`]. The
//!   worker keeps running and still finishes its sequence, and the slot stays busy until it does.
//! * `interaction`: [`Interaction::NonInteractive`] runs the handle's pinned store as
//!   [`Store::non_interactive`], so nothing it does can raise an OS prompt; a locked or
//!   access-controlled item is `NeedsInteraction`. [`Interaction::Interactive`] may raise the OS
//!   prompt and is for an explicit user action or a start-up decision.
//!
//! A second caller does not join the first caller's result: it waits for the slot (up to
//! `slot_wait`) or gets `Busy`, then runs its own sequence.
//!
//! The owner's [`Store`] also bounds each of its own calls (5 s non-interactive and 90 s
//! interactive by default) and runs each on its own thread. So a worker here is blocked for at most
//! the sum of the store calls of its sequence (up to three for `get_or_create`: the read, the add
//! and the read of a winner), never indefinitely. Nothing in this module joins a thread.
//!
//! ## A write has four outcomes, and one of them is not a failure
//!
//! [`KeySlot::persist`] returns `Ok(())` or a [`PersistError`] that says what the caller may
//! conclude about the store:
//!
//! * `Busy`, `Closed`, `NeedsInteraction`, `NotAttempted`: no OS write was started by this call.
//!   The item is as it was (an EARLIER abandoned write of the same credential may still land; see
//!   `Indeterminate`).
//! * `Failed`: the OS or the owner's store reported an error for the write, including the
//!   handle's own backend or namespace refusing writes. The owner's backends write by
//!   add-then-modify and never delete as a fallback (`macos.rs::set_flow`), and take the
//!   per-target lock before the write (`locked.rs::set`), so the item holds what it held before.
//! * `Indeterminate`: the write was started and the caller cannot know whether it landed. Raised
//!   when the owner's store timed out the write (`Error::Timeout`, other than the backend's own
//!   lock wait, which is before the write), when this caller's `result_wait` ended, when the slot
//!   was closed while the caller waited, or when the worker was lost. The OS call cannot be
//!   cancelled and may still finish with success or failure after the caller has its answer.
//!   Nothing here may be treated as a failure to write: the item may hold the new value.
//!
//! An indeterminate write also marks the slot UNSETTLED. While unsettled the slot never answers
//! from its copy (the copy is cleared), a read refuses with `Busy` while the owner's store still
//! has a mutation of the credential inside the OS (`Store::mutation_in_flight` on the handle's
//! pinned store), and no read result is kept until the slot has seen the store with nothing in
//! flight. [`KeySlot::holds`] is how a caller settles: it reports which value the store holds, or
//! that a write is still in flight. There is no state on disk and no timer: unsettled is a flag of
//! this process, and a restarted process learns the store's content by reading it.
//!
//! ## Abandonment, shutdown and what the OS allows
//!
//! An OS credential prompt (the macOS Keychain access-control prompt) is shown by the OS and, as
//! far as the crate can tell, cannot be dismissed or cancelled programmatically; the owner's
//! [`Store`] says the same of a timed-out call. What this module can do is stop WAITING for it,
//! and it does: a caller leaves after its `Call` bounds; [`KeySlot::close`] and [`close_all`]
//! return at once.
//!
//! * `close` marks the logical slot closed for the rest of the process (one-way, every handle) and
//!   clears the copy. Every later operation is `Closed`, including one waiting for the slot. A
//!   worker still inside a store call is abandoned, not joined. A READ or a mint whose answer is
//!   published after `close` wins is replaced by `Closed` at the moment the caller takes it (the
//!   answer is taken under the same lock `close` sets the flag under), so no key is delivered
//!   after close. A WRITE that already returned success is still reported as success (the store
//!   changed); a write whose answer was not yet published when `close` won is `Indeterminate`.
//! * A prompt that is still open when the owner's [`Store`] gives up on its call keeps running in
//!   the OS; the owner keeps a mutation's target claimed until the OS returns (a retry gets
//!   `Busy` from `Error::InProgress`), but a READ that timed out is not tracked.
//!
//! ## Mint admission
//!
//! [`KeySlot::get_or_create`] reads through [`KeySlot::state`]'s sequence, generates only after
//! authoritative absence, then requests an atomic create-only write on this handle's pinned
//! store. Another creator wins safely: the losing caller reads the winner on its own store.
//! The slot busy flag excludes its sequences; Store's native target permit excludes every local
//! mutation of that physical credential; the backend's atomic add excludes other processes.
//!
//! Both mint and persist use Store's worker admission callback. Store first resolves its single
//! mutation backend and acquires the target permit, then its worker checks the slot's closed flag
//! under the same lock as close, directly before invoking the backend. If close wins admission,
//! no backend mutation runs. If admission wins, the operation is already in flight and may finish
//! after close; the lock is never held across I/O. Reads and mint answers taken after close are
//! suppressed, cached values are cleared, and no newly registered slot can reopen the process
//! after [`close_all`]. A previously admitted successful write is still reported as successful.
//!
//! ## Fail closed and ordering
//!
//! A refused admission never mints a key. A timeout or close after admission may leave a
//! mutation in flight and clears the copy; a mint only follows an authoritative `Ok(None)` read in the same
//! sequence; a copy update only follows a successful store result. A write that succeeded in the
//! store but whose copy update is refused (slot closed, or a newer entry exists) clears the copy
//! rather than leave an older value in it.
//!
//! The copy carries the version stamp of the sequence that wrote it. A sequence takes the next
//! stamp when it acquires the slot and may set the copy only if its stamp is newer than the
//! copy's; because the slot admits one sequence at a time the stamps already increase, and the
//! comparison is the second guard.
//!
//! [`KeySlot::last_known`] is the copy as it was last set, subject to the same access rule: it may
//! be older than the store, and `None` says nothing about the store. No decision that can create,
//! rotate or discard a key may be based on it.
//!
//! Lock order: a slot's state mutex is never held while any other lock is taken. The registry
//! mutex is held only to look up or insert a record in [`KeySlot::new`] and to copy the records out
//! in [`close_all`], which also permanently latches process shutdown. A poisoned lock is recovered: every assignment under it is whole.
//!
//! Threads: one resolver thread per running [`KeySlot::new`] (at most [`MAX_RESOLVING`] in the
//! process, abandoned on timeout, ends when the OS call returns); one worker per running sequence,
//! at most one per logical slot at a time. No timer and no file.

use crate::namespace::BlobCodec;
use crate::{Error, Interaction, Secret, Store};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Most logical slots one process can register. A real host has one or two.
pub const MAX_SLOTS: usize = 64;

/// Most resolver threads ([`KeySlot::new`]) alive in one process at a time, including any that
/// were abandoned after their caller's `resolve_wait` and are still inside an OS call.
pub const MAX_RESOLVING: usize = 4;

/// Longest `slot_wait`, `result_wait` or `resolve_wait` a caller may give.
pub const MAX_WAIT: Duration = Duration::from_secs(600);

/// How one slot behaves. No default: every field is the host's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySlotPolicy {
    /// Length of a key this slot mints, in random bytes. `16..=1024`, the owner's range.
    pub key_bytes: usize,
    /// Keep the last known good key in memory (zeroized on drop) and answer from it first, to a
    /// handle whose access equals the one the copy was read under. For a store whose reads can
    /// raise an OS prompt every time (a keychain access control list).
    pub cache_in_process: bool,
    /// An item that exists but holds no bytes: `true` is an error from the read (a store where an
    /// empty blob is damage, not absence); `false` treats it as absent in [`KeySlot::state`].
    pub empty_value_is_error: bool,
}

/// How one blocking call waits and whether it may raise an OS prompt. No default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    interaction: Interaction,
    slot_wait: Duration,
    result_wait: Duration,
}

impl Call {
    /// `slot_wait` is `0..=`[`MAX_WAIT`] (`0` = never wait for another operation: an immediate
    /// `Busy`); `result_wait` is `1 ms..=`[`MAX_WAIT`]. Anything else is [`Error::Invalid`].
    pub fn new(
        interaction: Interaction,
        slot_wait: Duration,
        result_wait: Duration,
    ) -> Result<Self, Error> {
        if slot_wait > MAX_WAIT {
            return Err(Error::Invalid {
                what: "slot wait",
                reason: "must be at most 600 s".into(),
            });
        }
        if result_wait < Duration::from_millis(1) || result_wait > MAX_WAIT {
            return Err(Error::Invalid {
                what: "result wait",
                reason: "must be 1 ms..=600 s".into(),
            });
        }
        Ok(Self {
            interaction,
            slot_wait,
            result_wait,
        })
    }
}

/// Why [`KeySlot::new`] produced no slot. None of them read, wrote or minted anything, and none
/// registered anything; the host may call `new` again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The account, the policy or `resolve_wait` is out of range. Nothing was attempted. Fixed
    /// wording naming the field.
    Invalid(String),
    /// The owner's `Store::pinned` (for the macOS user default: the default keychain cannot be
    /// resolved or its file cannot be named) or the identity of the pinned store failed, or the
    /// resolver thread could not be started or ended without an answer, or process shutdown
    /// has permanently closed slot registration. The owner's error text or fixed shutdown text.
    Unresolved(String),
    /// `resolve_wait` ended before the resolution returned. The resolver thread is abandoned and
    /// its late result is dropped.
    TimedOut,
    /// [`MAX_RESOLVING`] earlier resolutions are still running.
    Busy,
    /// A slot for this native credential is registered with a different [`KeySlotPolicy`].
    PolicyMismatch,
    /// [`MAX_SLOTS`] slots are registered and this is a new one.
    Full,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(text) | Self::Unresolved(text) => f.write_str(text),
            Self::TimedOut => f.write_str("credential key slot: resolving the store timed out"),
            Self::Busy => {
                f.write_str("credential key slot: earlier store resolutions are still running")
            }
            Self::PolicyMismatch => f.write_str(
                "credential key slot: this slot is already registered with a different policy",
            ),
            Self::Full => {
                f.write_str("credential key slot: too many slots registered in this process")
            }
        }
    }
}

impl std::error::Error for OpenError {}

impl From<OpenError> for String {
    fn from(err: OpenError) -> Self {
        err.to_string()
    }
}

/// Why a read or a mint did not produce an answer. None of them is absence of the credential, and
/// none of them minted a key or set the in-process copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotError {
    /// Another operation on this slot is running (an OS prompt may be open) and the caller's
    /// `slot_wait` ended; or the owner's store reports an earlier change still in progress; or an
    /// earlier write of this credential is unsettled and still inside the OS, so the store's
    /// content is not final.
    Busy,
    /// This caller's `result_wait`, or the owner store's own call timeout, ended first. The
    /// worker may still be running; the slot is busy until it ends.
    TimedOut,
    /// The item needs a user prompt and the call was non-interactive.
    NeedsInteraction,
    /// The slot was closed ([`KeySlot::close`], [`close_all`]) before or during the operation.
    Closed,
    /// Any other failure: fixed wording plus the owner's error text, never a value. Includes a
    /// handle whose own store no longer resolves to the slot it registered under.
    Failed(String),
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("credential key slot is busy with another operation"),
            Self::TimedOut => f.write_str("credential key slot operation timed out"),
            Self::NeedsInteraction => f.write_str(
                "credential key slot needs user interaction and the call was non-interactive",
            ),
            Self::Closed => f.write_str("credential key slot is closed"),
            Self::Failed(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for SlotError {}

impl From<SlotError> for String {
    fn from(err: SlotError) -> Self {
        err.to_string()
    }
}

/// Why a write did not return success, and what the caller may conclude about the store. Only
/// [`PersistError::Indeterminate`] leaves the store's content unknown; see the module
/// documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistError {
    /// Another operation holds the slot longer than `slot_wait`, or the owner's store reports an
    /// earlier change to this credential still running, or its per-target lock was not obtained.
    /// This call did not write.
    Busy,
    /// The slot was closed before this call reached the OS. This call did not write.
    Closed,
    /// The call was non-interactive and the item needs a prompt. This call did not write.
    NeedsInteraction,
    /// Refused before any OS call (the value is empty, the handle's own store no longer resolves to
    /// its slot, a worker could not be started). Fixed wording, never a value.
    NotAttempted(String),
    /// The OS or the owner's store reported an error for the write (including the handle's own
    /// backend or namespace refusing writes). The item holds what it held before. Fixed wording
    /// plus the owner's error text, never a value.
    Failed(String),
    /// The write was started and its outcome is unknown: it may have landed, may still land, or may
    /// fail later. Not a failure. Settle with [`KeySlot::holds`].
    Indeterminate,
}

impl PersistError {
    /// True only when the item may hold the new value or may still receive it.
    pub fn may_have_landed(&self) -> bool {
        matches!(self, Self::Indeterminate)
    }
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("credential key slot is busy with another operation"),
            Self::Closed => f.write_str("credential key slot is closed"),
            Self::NeedsInteraction => f.write_str(
                "credential key slot needs user interaction and the call was non-interactive",
            ),
            Self::NotAttempted(text) | Self::Failed(text) => f.write_str(text),
            Self::Indeterminate => f.write_str(
                "credential key write outcome is unknown: it may still complete in the store",
            ),
        }
    }
}

impl std::error::Error for PersistError {}

impl From<PersistError> for String {
    fn from(err: PersistError) -> Self {
        err.to_string()
    }
}

/// What the store holds, as [`KeySlot::holds`] read it, compared with the candidate inside the
/// crate by [`Secret::ct_eq`]. No value is returned, formatted or logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreHolds {
    /// The item holds exactly the candidate.
    Candidate,
    /// The item holds a different, non-empty value.
    Other,
    /// The item is authoritatively absent (or holds no bytes and the policy does not call that
    /// an error).
    Absent,
    /// A write of this credential is still inside the OS in this process, so the store's content is
    /// not final. Nothing was read. Ask again later.
    WriteInFlight,
}

/// The identity of one native credential on one pinned store (see the module documentation).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SlotKey {
    /// The owner's identity of the pinned store (`Backend::exclusion_identity`, else the
    /// backend's allocation).
    store: String,
    /// The backend's own identity of the credential (`Backend::credential_identity`).
    credential: String,
}

/// What a handle's binding can read, as far as the crate's access model goes (see the module
/// documentation, "The in-process copy and access"). Built in one place, from the handle's own
/// store.
#[derive(Clone)]
struct Access {
    /// Unique to this verified binding; physical exclusion identity does not establish equal reads.
    binding: Arc<()>,
    /// How the handle's namespace decodes a stored blob (`namespace.rs:184-190`).
    codec: BlobCodec,
    /// The handle's store has a guard active and denies every read (`store.rs:184-188`, `:401`).
    guarded: bool,
}

impl Access {
    fn of(store: &Store) -> Self {
        Self {
            binding: Arc::new(()),
            codec: store.namespace().blob_codec(),
            guarded: store.guard_active(),
        }
    }
}

impl PartialEq for Access {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.binding, &other.binding)
            && self.codec == other.codec
            && self.guarded == other.guarded
    }
}

impl Eq for Access {}

/// The in-process copy, with the stamp of the sequence that set it and the access it was read
/// under.
struct Cached {
    stamp: u64,
    access: Access,
    value: Secret,
}

struct Inner {
    /// An operation owns the slot (set by the caller before its worker starts, cleared by the
    /// worker when the sequence ends).
    busy: bool,
    /// One-way: set by close, never cleared.
    closed: bool,
    /// An earlier write ended indeterminate and no sequence has since seen the store with nothing
    /// in flight. The copy is not served and no read result is kept while this is set.
    unsettled: bool,
    /// The last stamp handed to a sequence.
    stamp: u64,
    /// The in-process copy.
    cache: Option<Cached>,
}

/// The coordination record of a logical slot. It holds NO binding: no operation of any handle
/// reads a store, a namespace or an account from it.
struct Shared {
    /// The registry key this record is registered under. A handle's operation runs only while the
    /// handle's own store and account resolve to it (`Op::bound`).
    key: SlotKey,
    policy: KeySlotPolicy,
    /// A clone of the first registering handle's store, kept so the allocation behind an
    /// `instance:` identity is never freed while the entry exists. Never read.
    _identity_anchor: Store,
    state: Mutex<Inner>,
    freed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One handle's own binding: its pinned store, its account and its access. Never shared with
/// another handle.
struct Binding {
    store: Store,
    account: String,
    access: Access,
}

struct Registry {
    closed: bool,
    slots: BTreeMap<SlotKey, Arc<Shared>>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    closed: false,
    slots: BTreeMap::new(),
});

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The copy as `access` may be served it, or `None`: the policy keeps a copy, the slot is open and
/// settled, the binding has no guard, and the copy was read under an equal access.
fn serve(shared: &Shared, access: &Access) -> Option<Secret> {
    if !shared.policy.cache_in_process || access.guarded {
        return None;
    }
    let inner = shared.lock();
    if inner.closed || inner.unsettled {
        return None;
    }
    inner
        .cache
        .as_ref()
        .filter(|entry| entry.access == *access)
        .map(|entry| entry.value.clone())
        .filter(|value| !value.is_empty())
}

/// Resolver threads alive right now (see [`MAX_RESOLVING`]).
static RESOLVING: AtomicUsize = AtomicUsize::new(0);

/// One of the [`MAX_RESOLVING`] places; released when the resolver thread ends (or was never
/// started).
struct ResolverPlace;

impl ResolverPlace {
    fn take() -> Option<Self> {
        RESOLVING
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_RESOLVING).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ResolverPlace {
    fn drop(&mut self) {
        RESOLVING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What the resolver thread hands back: the handle's pinned store and its registry key, read from
/// that same pinned store with the handle's account. Neither carries a credential value.
struct Resolved {
    store: Store,
    key: SlotKey,
}

/// The ONE resolution: pin the store with the owner's `Store::pinned`, then read the registry key
/// from that same pinned store and this handle's account. No credential is read.
fn resolve(base: &Store, account: &str) -> Result<Resolved, Error> {
    let store = base.pinned()?;
    let (store_identity, credential) = store.mutation_target(account)?;
    Ok(Resolved {
        store,
        key: SlotKey {
            store: store_identity,
            credential,
        },
    })
}

/// Why a call ended before its worker was started: nothing reached the OS.
enum Before {
    Busy,
    Closed,
    Spawn,
}

/// Why a call ended after its worker was started: the caller has no answer, and the worker may
/// have reached the OS.
enum After {
    TimedOut,
    Closed,
    Lost,
}

/// What a blocking method returns on the paths that have no answer from the worker.
trait Outcome {
    fn before(why: Before) -> Self;
    fn after(why: After) -> Self;
}

impl Outcome for SlotError {
    fn before(why: Before) -> Self {
        match why {
            Before::Busy => Self::Busy,
            Before::Closed => Self::Closed,
            Before::Spawn => Self::Failed("credential key slot: could not start worker".into()),
        }
    }
    fn after(why: After) -> Self {
        match why {
            After::TimedOut => Self::TimedOut,
            After::Closed => Self::Closed,
            After::Lost => {
                Self::Failed("credential key slot: worker ended without an answer".into())
            }
        }
    }
}

impl Outcome for PersistError {
    fn before(why: Before) -> Self {
        match why {
            Before::Busy => Self::Busy,
            Before::Closed => Self::Closed,
            Before::Spawn => {
                Self::NotAttempted("credential key slot: could not start worker".into())
            }
        }
    }
    /// A write that left the caller without an answer may have reached the OS.
    fn after(_why: After) -> Self {
        Self::Indeterminate
    }
}

/// Exclusive right to run one sequence on a slot. It is turned in through [`Finisher`], which is
/// what clears the busy flag.
struct Permit {
    shared: Arc<Shared>,
    stamp: u64,
}

/// Where a sequence's answer is left for its caller.
type Mailbox<T, E> = Arc<Mutex<Option<Result<T, E>>>>;

/// Lives on the worker. `complete` stores the answer and clears the busy flag under the slot's
/// state lock and then wakes every waiter, so the caller can never see the slot free without the
/// answer or the answer without the slot free. Dropping it unfinished (the worker panicked, or
/// could not be started) does the same with the method's "lost" answer.
struct Finisher<T, E: Outcome> {
    permit: Option<Permit>,
    mailbox: Mailbox<T, E>,
}

impl<T, E: Outcome> Finisher<T, E> {
    fn complete(&mut self, outcome: Result<T, E>) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        {
            let mut inner = permit.shared.lock();
            inner.busy = false;
            *self.mailbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        }
        permit.shared.freed.notify_all();
    }
}

impl<T, E: Outcome> Drop for Finisher<T, E> {
    fn drop(&mut self) {
        self.complete(Err(E::after(After::Lost)));
    }
}

fn acquire(shared: &Arc<Shared>, slot_wait: Duration) -> Result<Permit, Before> {
    let deadline = Instant::now().checked_add(slot_wait);
    let mut inner = shared.lock();
    loop {
        if inner.closed {
            return Err(Before::Closed);
        }
        if !inner.busy {
            inner.busy = true;
            inner.stamp = inner.stamp.saturating_add(1);
            return Ok(Permit {
                shared: shared.clone(),
                stamp: inner.stamp,
            });
        }
        let left = deadline
            .and_then(|at| at.checked_duration_since(Instant::now()))
            .filter(|left| !left.is_zero());
        let Some(left) = left else {
            return Err(Before::Busy);
        };
        inner = shared
            .freed
            .wait_timeout(inner, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// What a worker needs. Owned, so it can outlive the caller. The store, the account and the access
/// are the calling HANDLE's (`binding`); the record (`shared`) supplies coordination only.
struct Op {
    shared: Arc<Shared>,
    binding: Arc<Binding>,
    interaction: Interaction,
    stamp: u64,
}

fn map_store(what: &'static str, err: Error) -> SlotError {
    match err {
        Error::InteractionRequired => SlotError::NeedsInteraction,
        Error::InProgress { .. } => SlotError::Busy,
        Error::Timeout { .. } => SlotError::TimedOut,
        other => SlotError::Failed(format!("credential store {what} failed: {other}")),
    }
}

/// A timeout of a store call that changes the credential (`Store::run` names them "set" and "add";
/// the mint ends in "add"). The backend's own lock wait is op "lock" and is before any write, so it
/// is not one.
fn timed_out_mutation(err: &Error) -> bool {
    matches!(err, Error::Timeout { op, .. } if *op == "set" || *op == "add")
}

/// Fixed wording for a handle whose own binding no longer resolves to its slot.
const UNBOUND: &str = "credential key slot: this handle's store no longer resolves to its slot";

impl Op {
    fn policy(&self) -> KeySlotPolicy {
        self.shared.policy
    }

    /// The calling handle's own pinned store with this call's interaction mode. The same backend
    /// binding every time: only the mode differs.
    fn store(&self) -> Store {
        match self.interaction {
            Interaction::Interactive => self.binding.store.interactive(),
            Interaction::NonInteractive => self.binding.store.non_interactive(),
        }
    }

    /// Whether this handle's own pinned store and account still resolve to the key of the record
    /// the handle registered under (`Store::mutation_target`: no credential access). A handle for
    /// which this is false is not this slot and does nothing.
    fn bound(&self) -> bool {
        matches!(
            self.binding.store.mutation_target(&self.binding.account),
            Ok((store, credential))
                if store == self.shared.key.store && credential == self.shared.key.credential
        )
    }

    fn cached(&self) -> Option<Secret> {
        serve(&self.shared, &self.binding.access)
    }

    fn unsettled(&self) -> bool {
        self.shared.lock().unsettled
    }

    /// An earlier write ended indeterminate: stop serving the copy and keep no read until the store
    /// has been seen with nothing in flight.
    fn mark_unsettled(&self) {
        let mut inner = self.shared.lock();
        inner.cache = None;
        inner.unsettled = true;
    }

    /// The store's content for this slot is known as of this sequence (a read with nothing in
    /// flight, or a write that returned success). Clears `unsettled` and sets the copy from
    /// `value`, recorded under THIS handle's access. `written` is true when the store was just
    /// written with `value`, in which case a refused update clears the copy instead of leaving an
    /// older value in it.
    fn commit(&self, value: &Secret, written: bool) {
        let mut inner = self.shared.lock();
        if inner.closed {
            inner.cache = None;
            return;
        }
        inner.unsettled = false;
        if !self.policy().cache_in_process {
            return;
        }
        match &inner.cache {
            Some(entry) if entry.stamp >= self.stamp => {
                if written {
                    inner.cache = None;
                }
            }
            _ => {
                inner.cache = Some(Cached {
                    stamp: self.stamp,
                    access: self.binding.access.clone(),
                    value: value.clone(),
                })
            }
        }
    }

    /// The store is authoritatively empty for this slot: nothing is in flight, nothing is held.
    fn commit_absent(&self) {
        let mut inner = self.shared.lock();
        inner.cache = None;
        if !inner.closed {
            inner.unsettled = false;
        }
    }

    /// Whether a mutation of this credential is inside a backend call right now (including one
    /// whose caller timed out), asked of the handle's own pinned store: the key it looks up is the
    /// one `Store::run` claimed for a write, and the table is process-wide, so a write through any
    /// handle of the slot is seen.
    fn in_flight(&self) -> Result<bool, SlotError> {
        self.binding
            .store
            .mutation_in_flight(&self.binding.account)
            .map_err(|err| {
                SlotError::Failed(format!("credential key slot in-flight check failed: {err}"))
            })
    }

    fn read_from(&self, store: &Store) -> Result<Option<Secret>, SlotError> {
        match store.get(&self.binding.account) {
            Ok(Some(secret)) => {
                if self.policy().empty_value_is_error && secret.is_empty() {
                    return Err(SlotError::Failed("stored credential is empty".to_string()));
                }
                secret.expose_str().map_err(|err| {
                    SlotError::Failed(format!("stored credential is not UTF-8: {err}"))
                })?;
                Ok(Some(secret))
            }
            Ok(None) => Ok(None),
            Err(err) => Err(map_store("read", err)),
        }
    }

    /// The state read. The binding is verified first; the copy is served only under the access
    /// rule; otherwise the read runs on the handle's own pinned store.
    fn state_in(&self) -> Result<Option<Secret>, SlotError> {
        if !self.bound() {
            return Err(SlotError::Failed(UNBOUND.to_string()));
        }
        if let Some(existing) = self.cached() {
            return Ok(Some(existing));
        }
        // After an indeterminate write the store's content is not final while that write is still
        // inside the OS: refuse rather than answer with a value it may be about to replace.
        if self.unsettled() && self.in_flight()? {
            return Err(SlotError::Busy);
        }
        match self
            .read_from(&self.store())?
            .filter(|value| !value.is_empty())
        {
            Some(existing) => {
                self.commit(&existing, false);
                Ok(Some(existing))
            }
            None => Ok(None),
        }
    }

    /// Early mint checks. Final close admission is repeated on Store's mutation worker after
    /// acquiring its target permit, so a target wait or thread start cannot admit a closed slot.
    fn admit_mint(&self) -> Result<(), SlotError> {
        if !self.bound() {
            return Err(SlotError::Failed(UNBOUND.to_string()));
        }
        if self.in_flight()? {
            return Err(SlotError::Busy);
        }
        if self.closed() {
            return Err(SlotError::Closed);
        }
        Ok(())
    }

    fn get_or_create_in(&self) -> Result<Secret, SlotError> {
        if let Some(existing) = self.state_in()? {
            return Ok(existing);
        }
        // Authoritative absence. Generate first (no store call), then check early prerequisites;
        // Store repeats final close admission on its mutation worker.
        let fresh = Store::generate_key(self.policy().key_bytes).map_err(|err| {
            SlotError::Failed(format!(
                "credential key slot: generating a key failed: {err}"
            ))
        })?;
        self.admit_mint()?;
        let store = self.store();
        let created = store
            .put_admitted(
                &self.binding.account,
                &fresh,
                crate::WriteIntent::CreateOnly,
                self.admission(),
            )
            .map_err(|err| {
                if timed_out_mutation(&err) {
                    self.mark_unsettled();
                }
                map_store("mint", err)
            })?
            .ok_or(SlotError::Closed)?;
        let secret = if created {
            fresh
        } else {
            // Another writer created the item first (another process, or a write the slot could
            // not see): adopt it, read on this handle's own store.
            self.read_from(&store)?
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    SlotError::Failed(
                        "credential key slot: the key that won the add could not be read".into(),
                    )
                })?
        };
        secret
            .expose_str()
            .map_err(|err| SlotError::Failed(format!("stored credential is not UTF-8: {err}")))?;
        self.commit(&secret, false);
        Ok(secret)
    }

    fn holds_in(&self, candidate: &Secret) -> Result<StoreHolds, SlotError> {
        if !self.bound() {
            return Err(SlotError::Failed(UNBOUND.to_string()));
        }
        // Checked first, on the handle's pinned store (the table is process-wide, so a write of any
        // handle of this slot shows). With nothing in flight every earlier OS call has returned, so
        // the read below sees the final content; this slot admits no write of its own while this
        // sequence holds the busy flag.
        if self.in_flight()? {
            return Ok(StoreHolds::WriteInFlight);
        }
        match self
            .read_from(&self.store())?
            .filter(|value| !value.is_empty())
        {
            Some(held) => {
                let same = held.ct_eq(candidate);
                self.commit(&held, false);
                Ok(if same {
                    StoreHolds::Candidate
                } else {
                    StoreHolds::Other
                })
            }
            None => {
                self.commit_absent();
                Ok(StoreHolds::Absent)
            }
        }
    }

    fn persist_in(&self, value: &Secret) -> Result<(), PersistError> {
        if value.is_empty() {
            return Err(PersistError::NotAttempted(
                "credential key slot: refusing to write an empty value".into(),
            ));
        }
        if !self.bound() {
            return Err(PersistError::NotAttempted(UNBOUND.to_string()));
        }
        // Refuse early if closed; Store repeats final admission after its own target permit and
        // worker start. This handle's own backend and namespace enforce its write permissions.
        if self.closed() {
            return Err(PersistError::Closed);
        }
        match self.store().put_admitted(
            &self.binding.account,
            value,
            crate::WriteIntent::Replace,
            self.admission(),
        ) {
            Ok(None) => Err(PersistError::Closed),
            Ok(Some(_)) => {
                self.commit(value, true);
                Ok(())
            }
            Err(Error::InteractionRequired) => Err(PersistError::NeedsInteraction),
            Err(Error::InProgress { .. }) => Err(PersistError::Busy),
            // The backend could not take its per-target lock: it is before the write.
            Err(Error::Timeout { op: "lock", .. }) => Err(PersistError::Busy),
            Err(Error::Timeout { .. }) => {
                self.mark_unsettled();
                Err(PersistError::Indeterminate)
            }
            Err(other) => Err(PersistError::Failed(format!(
                "credential store write failed: {other}"
            ))),
        }
    }

    /// This callback is invoked only after Store owns the native mutation target, on its worker.
    /// Checking under the close lock linearizes admission with close without holding it over I/O.
    fn admission(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let shared = self.shared.clone();
        Arc::new(move || !shared.lock().closed)
    }

    fn closed(&self) -> bool {
        self.shared.lock().closed
    }
}

/// A process-wide cell holding the one [`KeySlot`] a host binds for a credential, for a `static`.
///
/// The first successful bind is kept for the process; a failed bind is NOT remembered, so a later
/// call binds again. The cell's lock is held across the bind (which [`KeySlot::new`] bounds by its
/// `resolve_wait`), so a second caller waits for that one binding rather than starting another. A
/// poisoned lock is recovered.
pub struct SharedKeySlot {
    slot: Mutex<Option<KeySlot>>,
}

impl SharedKeySlot {
    /// An empty cell, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// The bound slot, or the result of `bind` (kept on success). The host's `bind` builds its
    /// store and calls [`KeySlot::new`] with its account, policy and resolve wait, mapping its own
    /// errors to text.
    pub fn get_or_bind(
        &self,
        bind: impl FnOnce() -> Result<KeySlot, String>,
    ) -> Result<KeySlot, String> {
        let mut bound = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = bound.as_ref() {
            return Ok(slot.clone());
        }
        let slot = bind()?;
        *bound = Some(slot.clone());
        Ok(slot)
    }
}

impl Default for SharedKeySlot {
    fn default() -> Self {
        Self::new()
    }
}

/// A handle onto one native credential's key slot. See the module documentation. Handles to the
/// same credential share the coordination record (busy, closed, unsettled, stamp, copy) and
/// nothing else: each runs on its own pinned store, account and access. Cloning is cheap and
/// yields the same handle.
#[derive(Clone)]
pub struct KeySlot {
    shared: Arc<Shared>,
    binding: Arc<Binding>,
}

impl KeySlot {
    /// The handle for the native credential `store` addresses at `account`. `store` is the host's
    /// (namespace, guard, interaction mode); it is resolved ONCE here with `Store::pinned` and the
    /// pinned result is THIS handle's binding for its registry key, every operation and the
    /// in-flight lookup (see the module documentation). The handle joins the existing coordination
    /// record when that key is registered, else registers a new one; either way it keeps its own
    /// store, account and access, and takes none of them from the record. The resolution may need an
    /// OS call (no credential access, no prompt), so it runs on a thread and is waited for at most
    /// `resolve_wait` (`1 ms..=`[`MAX_WAIT`]). `account` is checked as the owner checks every
    /// account name; `policy.key_bytes` must be in `16..=1024`; a registered slot must have the same
    /// policy; at most [`MAX_SLOTS`] slots and [`MAX_RESOLVING`] resolver threads. Reads and writes
    /// nothing. Every failure is a typed [`OpenError`] and leaves nothing registered.
    pub fn new(
        store: Store,
        account: impl Into<String>,
        policy: KeySlotPolicy,
        resolve_wait: Duration,
    ) -> Result<Self, OpenError> {
        let account = account.into();
        crate::namespace::check_account(&account)
            .map_err(|err| OpenError::Invalid(format!("credential key slot account: {err}")))?;
        if !(16..=1024).contains(&policy.key_bytes) {
            return Err(OpenError::Invalid(
                "credential key slot key length must be 16..=1024 bytes".into(),
            ));
        }
        if resolve_wait < Duration::from_millis(1) || resolve_wait > MAX_WAIT {
            return Err(OpenError::Invalid(
                "credential key slot resolve wait must be 1 ms..=600 s".into(),
            ));
        }
        if registry().closed {
            return Err(OpenError::Unresolved(
                "credential key slot: process is closed".into(),
            ));
        }
        let place = ResolverPlace::take().ok_or(OpenError::Busy)?;
        let (tx, rx) = mpsc::channel();
        let target = account.clone();
        // If the thread cannot be started the closure, and the place in it, is dropped.
        std::thread::Builder::new()
            .name("rightkit-key-slot-resolve".to_string())
            .spawn(move || {
                let _place = place;
                let _ = tx.send(resolve(&store, &target).map_err(|err| err.to_string()));
            })
            .map_err(|_| {
                OpenError::Unresolved("credential key slot: could not start resolver".into())
            })?;
        let resolved = match rx.recv_timeout(resolve_wait) {
            Ok(Ok(resolved)) => resolved,
            Ok(Err(text)) => {
                return Err(OpenError::Unresolved(format!(
                    "credential key slot: cannot bind the store: {text}"
                )))
            }
            Err(RecvTimeoutError::Timeout) => return Err(OpenError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(OpenError::Unresolved(
                    "credential key slot: resolver ended without an answer".into(),
                ))
            }
        };
        // The key was computed by the resolver from THIS handle's pinned store and account: it is
        // the registry key this handle registers or joins under, and no other handle's store or
        // account entered it. Every operation re-verifies it (`Op::bound`).
        let Resolved { store, key } = resolved;
        let access = Access::of(&store);
        let mut registry = registry();
        // A resolver may have started before shutdown: recheck under the same lock that latches it.
        if registry.closed {
            return Err(OpenError::Unresolved(
                "credential key slot: process is closed".into(),
            ));
        }
        let slots = &mut registry.slots;
        let shared = match slots.get(&key) {
            Some(existing) => {
                if existing.policy != policy {
                    return Err(OpenError::PolicyMismatch);
                }
                existing.clone()
            }
            None => {
                if slots.len() >= MAX_SLOTS {
                    return Err(OpenError::Full);
                }
                let created = Arc::new(Shared {
                    key: key.clone(),
                    policy,
                    _identity_anchor: store.clone(),
                    state: Mutex::new(Inner {
                        busy: false,
                        closed: false,
                        unsettled: false,
                        stamp: 0,
                        cache: None,
                    }),
                    freed: Condvar::new(),
                });
                slots.insert(key, created.clone());
                created
            }
        };
        drop(registry);
        Ok(Self {
            shared,
            binding: Arc::new(Binding {
                store,
                account,
                access,
            }),
        })
    }

    /// This handle's pinned store: the binding every operation of THIS handle uses, in the mode the
    /// host gave it. For the host's OTHER accounts that must live in the same physical store as
    /// this key (for example a staging credential beside it): `slot.store().set(other, ..)`
    /// addresses the same keychain this handle was bound to, where a freshly opened store would
    /// address whatever the default is at that moment. It is a handle, not a credential: it holds
    /// no value. Calls made on it bypass the slot's busy flag, copy and unsettled rules.
    pub fn store(&self) -> Store {
        self.binding.store.clone()
    }

    /// The in-process copy as it was last set, if THIS handle may be served it (the policy keeps a
    /// copy, the slot is open and settled, the copy was read under an access equal to this
    /// handle's, and this handle's store has no guard active): never the store, never waiting for
    /// an operation (the lock it takes is never held across a store call). `None` otherwise, which
    /// says nothing about the store. It may be older than the store. Use it only to display or to
    /// skip optional work; never to decide to create, rotate, replace or discard a key.
    pub fn last_known(&self) -> Option<Secret> {
        serve(&self.shared, &self.binding.access)
    }

    /// Close this logical slot (every handle) for the rest of the process and clear the copy.
    /// Never waits, never joins: a worker still inside an OS call is abandoned. Safe on a UI or
    /// shutdown path.
    pub fn close(&self) {
        close_shared(&self.shared);
    }

    /// Run `f` as one sequence on a worker, under the slot's busy flag. `effect` is true for a
    /// write: its success is delivered even if the slot was closed meanwhile, because the store
    /// changed. A read or mint answer taken after `close` won is replaced by `Closed`: the answer
    /// is taken under the same lock `close` sets the flag under, so the two are ordered and no key
    /// is delivered after close.
    fn run<T: Send + 'static, E: Outcome + Send + 'static>(
        &self,
        call: Call,
        effect: bool,
        f: impl FnOnce(&Op) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let deadline = Instant::now().checked_add(call.result_wait);
        let permit = acquire(&self.shared, call.slot_wait).map_err(E::before)?;
        let op = Op {
            shared: self.shared.clone(),
            binding: self.binding.clone(),
            interaction: call.interaction,
            stamp: permit.stamp,
        };
        let shared = self.shared.clone();
        let mailbox: Mailbox<T, E> = Arc::new(Mutex::new(None));
        let mut finisher = Finisher {
            permit: Some(permit),
            mailbox: mailbox.clone(),
        };
        // If the thread cannot be started the closure is dropped with the finisher, which clears
        // the busy flag; nothing ran, so the answer is the "before" one.
        std::thread::Builder::new()
            .name("rightkit-key-slot".to_string())
            .spawn(move || {
                let outcome = f(&op);
                finisher.complete(outcome);
            })
            .map_err(|_| E::before(Before::Spawn))?;
        // Wait on the slot's own condvar, so the answer, a close and the deadline all end the wait.
        let mut inner = shared.lock();
        loop {
            if let Some(outcome) = mailbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                return match outcome {
                    Ok(_) if inner.closed && !effect => Err(E::before(Before::Closed)),
                    other => other,
                };
            }
            if inner.closed {
                return Err(E::after(After::Closed));
            }
            let left = deadline
                .and_then(|at| at.checked_duration_since(Instant::now()))
                .filter(|left| !left.is_zero());
            let Some(left) = left else {
                return Err(E::after(After::TimedOut));
            };
            inner = shared
                .freed
                .wait_timeout(inner, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Tri-state read for an open-time decision: `Ok(Some)` a usable non-empty key, `Ok(None)` the
    /// credential is authoritatively absent (or stored empty without
    /// [`KeySlotPolicy::empty_value_is_error`]), `Err` it could not be established (see
    /// [`SlotError`]). With [`KeySlotPolicy::cache_in_process`] a non-empty copy read under this
    /// handle's access answers first, and a key read from the store is kept. While an earlier write
    /// is unsettled and still inside the OS this is `Err(Busy)`. An `Err` changes nothing.
    pub fn state(&self, call: Call) -> Result<Option<Secret>, SlotError> {
        self.run(call, false, |op| op.state_in())
    }

    /// Write `value` (create or replace) on this handle's own store, then refresh the copy, as one
    /// sequence. Call only when the store has just reported the credential absent, or for a
    /// deliberate rotation. The result says what the caller may conclude (see [`PersistError`]):
    /// `Ok` the store holds `value`; `Busy`, `Closed`, `NeedsInteraction`, `NotAttempted` nothing
    /// was written by this call; `Failed` the store reported an error (including this handle's own
    /// backend or namespace refusing writes) and holds what it held; `Indeterminate` the write may
    /// still land, the copy is cleared, the slot is unsettled, and the caller must not act as if
    /// the write failed. A write that returned success is reported as success even if the slot was
    /// closed meanwhile.
    pub fn persist(&self, value: &str, call: Call) -> Result<(), PersistError> {
        let value = Secret::from_string(value);
        self.run(call, true, move |op| op.persist_in(&value))
    }

    /// Which value the store holds, compared with `candidate` inside the crate in constant time
    /// ([`Secret::ct_eq`]); neither value is returned, formatted or logged. It is a read on this
    /// handle's own store under the slot, bounded like every call, that bypasses the copy.
    /// `WriteInFlight` when a write of this credential (including one a caller gave up on, through
    /// any handle) is still inside the OS in this process: the content is not final, ask later.
    /// Otherwise the answer is final for every OS call this process has made, and the slot is
    /// settled. It cannot see a write another process or a process that has since exited may still
    /// be completing.
    pub fn holds(&self, candidate: &str, call: Call) -> Result<StoreHolds, SlotError> {
        if candidate.is_empty() {
            return Err(SlotError::Failed(
                "credential key slot: an empty candidate cannot be held".into(),
            ));
        }
        let candidate = Secret::from_string(candidate);
        self.run(call, false, move |op| op.holds_in(&candidate))
    }

    /// The stored key, or a freshly minted one. Reads first through the same step as
    /// [`KeySlot::state`]; only an authoritative absence mints, and the mint is admitted as the
    /// module documentation's "Mint admission" says (binding and in-flight checks, then final close
    /// admission on Store's mutation worker before its atomic add-if-absent, so a concurrent
    /// process adopts the winner). The copy check, the read, the mint and the copy fill are one
    /// sequence, so two callers at first run mint once and both get that key. A mint that is not
    /// UTF-8 is an error and leaves the copy as it was. A mint the owner's store timed out may
    /// still land: the copy is cleared and the slot is unsettled.
    pub fn get_or_create(&self, call: Call) -> Result<Secret, SlotError> {
        self.run(call, false, |op| op.get_or_create_in())
    }
}

fn close_shared(shared: &Shared) {
    {
        let mut inner = shared.lock();
        inner.closed = true;
        inner.cache = None;
    }
    shared.freed.notify_all();
}

/// Permanently close slot registration and every registered slot (application shutdown).
/// Resolving constructors cannot register afterwards. Never joins or waits on an operation;
/// the registry is copied out first, and each slot lock is held only for field assignments.
pub fn close_all() {
    let all: Vec<Arc<Shared>> = {
        let mut registry = registry();
        registry.closed = true;
        registry.slots.values().cloned().collect()
    };
    for shared in all {
        close_shared(&shared);
    }
}
