//! Owner side of the QA credential snapshot (the counterpart of `snapshot::owner`, which is the
//! runner side).
//!
//! An app's OS credentials can be readable only by the installed, signed app itself (a macOS
//! Keychain access control list), so the QA tool cannot read them from its own executable. The
//! tool therefore launches the INSTALLED app with the one switch [`SWITCH`]. In that mode the
//! process performs exactly ONE credential operation (a snapshot, a restore, or a completion) over
//! a QA control service, answers with metadata only, flushes and exits. It never starts the app:
//! the host's `main` calls [`dispatch`] first, before anything else runs. A snapshot, the restore
//! of its credentials and the completion that ends the restore are three separate launches; the
//! state that joins them lives in the snapshot store (below), never in the process.
//!
//! No supported snapshot-store isolation or signed independent runner has been established.
//! This module is an API intake, with no behavior-complete claim. Configure
//! [`ExpectedRunner::unconfigured`] until independently trusted runner release metadata exists.
//! Owner-required operations must never fall back to in-process credential reads or restores.
//!
//! # What gates this code
//!
//! This module is compiled only when the crate feature `credential-owner` is on. Nothing in the
//! source makes that feature impossible in a release build: a host must keep it out of release
//! artifacts with its release tooling (the release-switch registry), exactly as for the crate's
//! `control` feature. At run time all of these must hold, and each refusal is fixed text with no
//! value in it:
//!
//! * the argv is exactly [`SWITCH`] and nothing else;
//! * `RIGHTKIT_CONTROL_SERVICE` names a `qa-credentials-<id>` service the daemon host accepts;
//! * the QA control token file named by `RIGHTKIT_QA_CONTROL_TOKEN_FILE` is a private regular file
//!   of at most [`MAX_TOKEN_FILE_BYTES`] bytes holding a usable token;
//! * the connected caller process is PROVEN to be the runner the host expects (below), on every
//!   order, and the order carries the token;
//! * the snapshot store is isolated from the runner as far as this process can establish (below);
//!   where that cannot be established every order is refused;
//! * every credential slot the owner can touch resolves to a native credential identity, and no
//!   snapshot-store slot can be a declared live credential (below, "Slot identities"); a
//!   declaration that fails this is refused at construction and every order is refused;
//! * the snapshot directory the order names is exactly
//!   `<managed root>/snapshots/<app>/<snapshotId>`, a real directory chain private to the current
//!   user, and the runner's restore marker there agrees with the order field for field;
//! * no other owner process holds the app's claim (below, "Recovery exclusion"): a second order
//!   for the same app is answered `app_busy` and changes nothing;
//! * the order is not a version 1 order and the snapshot is not a version 1 snapshot (below,
//!   "Interrupted version 1 markers"): those are refused `legacy_v1_marker` and nothing is
//!   consumed or deleted.
//!
//! No debug-only branch, no `cfg(debug_assertions)`, no environment bypass: the environment names
//! the service and the token file, it never lowers a check.
//!
//! # Who may call: the proven runner
//!
//! The daemon proves the caller runs as the current user. The label the caller declares
//! (`right-qa`) is a claim, and the token file is a capability any same-user process that can read
//! it also holds. Neither is proof, so neither is accepted as proof. Every order, `health`
//! included, first passes [`CallContext::verify_peer_signed`] against the [`ExpectedRunner`] the
//! HOST supplied from its own build configuration:
//!
//! * macOS: the audit token of the connection (the kernel's record of that process instance,
//!   never a pid or a path) is validated with `SecCodeCheckValidity` against a Developer ID
//!   requirement naming the team and the code-signing identifier
//!   (`SigningIdentity::MacDeveloperId`, rightkit-daemon 0.9.1).
//! * Windows: the pipe client's process is identified by an open process handle and its start
//!   time; the image the OS reports for that process is verified with `WinVerifyTrust` for the
//!   expected signer common name, and then, on one handle that denies write, rename and delete,
//!   (1) the SHA-256 of the whole image is computed on that handle with a byte cap
//!   ([`MAX_RUNNER_IMAGE_BYTES`]) and must equal [`WindowsRunnerIdentity::image_sha256`], the
//!   digest of the qualified runner artifact, and (2) `verify_held_windows_publisher` checks the
//!   expected publisher EKU and the pinned parent CA and root CA digests, with revocation checking
//!   (rightkit-daemon 0.9.1). A publisher match without the image digest is refused. The peer is
//!   identified again afterwards and must be the same process image.
//! * Any other platform has no peer signature proof: every order is refused.
//!
//! Nothing the requester sends takes part: the request has no field for a path or an identity
//! (unknown fields are refused), and the binary whose signature is verified is the image of the
//! connected process, not a file the requester names. An [`ExpectedRunner`] that names no identity
//! for the running platform refuses every order with `runner_proof_unavailable`; a peer the OS
//! cannot identify or whose signature cannot be checked is `runner_unverifiable`; a peer that is
//! identified and is not the expected signed runner is `runner_rejected`. There is no bypass, no
//! development mode and no unsigned runner.
//!
//! # What an order can do, and what leaves the process
//!
//! A credential value is read by the owner process from a DECLARED item and written only into the
//! app's own credential store, in the one host-declared [`SnapshotStore`] service. The snapshot
//! file in the snapshot directory is a manifest: schema, app, snapshot id and, per declared item,
//! set index, account name and whether it was present. It holds no value, no digest of a value and
//! no key. Responses carry metadata only. A credential value is never written to stdout, stderr, a
//! log, an IPC response, a file, the environment or argv; errors are fixed text plus set and
//! account names and indices. `run` silences the panic hook for the life of the process: this
//! mode must print no panic payload.
//!
//! # Slot identities: a snapshot-store slot is never a live credential
//!
//! The owner deletes and overwrites slots of the snapshot-store service (`cur`, `pending`,
//! `g-<generation>-<n>`). Comparing service strings is not enough to keep those away from the
//! app's own credentials: Windows compares target names without case, the keyring-compatible form
//! is `<account>.<service>` and the slash form `<prefix>/<account>`, so two different
//! `(service, account)` pairs can name one Credential Manager entry, and on Windows a large value
//! is also stored as derived `<account>.rkchunk.<generation>.<index>` entries. The owner therefore
//! compares native identities, as the credentials crate derives them
//! ([`rightkit_credentials::macos_credential_identity`] and
//! [`rightkit_credentials::windows_credential_identity`], both folded as the crate folds them, on
//! the very namespaces the stores use) and does it under BOTH rules on every platform, because the
//! declaration is one host configuration:
//!
//! * the slots are every declared live item, `cur`, `pending`, and the whole family
//!   `g-<anything>`, which covers every present and future generation and item index;
//! * each slot also covers its derived chunk entries (`<account>.rkchunk.<anything>`);
//! * a family is resolved by asking the crate for the identity of the prefix plus a placeholder
//!   character and splitting at the placeholder, so the family is a head and a tail taken from
//!   the crate's own output; two families overlap when their heads are prefix-comparable and
//!   their tails suffix-comparable; an exact identity is in a family when it has the head, the
//!   tail and room for both. The variable part is treated as any text, which can only refuse more;
//! * [`OwnerHost::new`] refuses (and `Owner::admit` re-checks on every order, before the isolation
//!   gate and before any store access) unless every identity resolves
//!   (`slot_identity_unresolved`), the declared live slots are pairwise disjoint, the snapshot-store
//!   slots are pairwise disjoint, and the two groups are disjoint (`slot_identity_overlap`);
//! * each order also resolves the concrete generations it can touch (the request's, `cur`'s and
//!   every one in `pending`) for every item index up to [`MAX_ITEMS`] and refuses unless all are
//!   distinct, before its first write or delete;
//! * every write or delete in the snapshot store goes through `Shadow`, which derives the account
//!   from a closed slot type and re-checks its identity against every live identity first.
//!
//! # Where the snapshot store is allowed: isolation
//!
//! The runner is a process of the same user. Moving values into another credential-store service
//! protects them from the runner only if the OS refuses the runner access to that service, so the
//! snapshot store is used only where the process can establish that, and `snapshot_isolation`
//! runs on every order after the sender proof and the token:
//!
//! * macOS: REFUSED, always, with `isolation_not_established`, until the credentials crate offers
//!   a per-slot access-control proof (HANDOFF.md, "Crate-side API needed"). A signed identity of the
//!   running process is necessary and is still checked (`SecCodeCopySelf`, `SecCodeCheckValidity`
//!   against the declared [`MacAppIdentity`]), but it cannot certify the access control of an
//!   item: the crate's `Store::set` creates an absent item through the legacy add call, which takes
//!   no access object, and UPDATES a present item in place
//!   (`SecKeychainItemModifyAttributesAndData`), which keeps whatever access control that item
//!   already has. A slot that exists before this process first writes it (created or re-created by
//!   any same-user process, with an access control that names that process) would therefore keep
//!   its access control, and the signed identity of this process says nothing about it. The
//!   crate exposes no call that reads an item's access object or sets one, so this module can
//!   neither verify a pre-existing slot nor replace it provably: the gate fails closed
//!   (`mac_slot_acl`), and `Shadow::set` refuses on macOS as well, so that no value or record is
//!   written to a slot whose access control was not proven.
//! * Windows: refused, always, with `snapshot_store_unisolated`. Credential Manager entries are
//!   readable by any process of the same user; a value sealed with a key kept in that store is
//!   opened by whoever reads the key; DPAPI opens for the same user; Windows Hello binds use to
//!   the user's presence and not to the application, and prompts. A same-user process can
//!   normally open the app's process and read its memory. No mechanism this crate has binds a
//!   secret to the app and not to the user, so the snapshot store on Windows would be a copy the
//!   runner can read.
//! * Any other platform: refused with `snapshot_store_unisolated`.
//!
//! # Recovery exclusion: one owner order per app at a time
//!
//! The runner's exclusive `claim_marker` stops a second MARKER from being created; it does not
//! serialise two recoveries of an EXISTING marker, and the daemon's election lock is per service
//! name while the runner names a fresh `qa-credentials-<id>` service for every launch, so two
//! runners (or two owner processes) can run orders for one app at the same moment. The owner
//! therefore takes its own claim per app, `AppClaim`: an exclusive OS advisory lock
//! (`rightkit_fs::FileLock::try_acquire`, `snapshots/<app>/owner.lock`, opened without following a
//! link) held for the whole of the order's store work. The OS drops it when the process exits or
//! is killed, so there is no stale record to judge and no age or TTL to guess (the sibling
//! `owner.lock.owner` record, pid and start time, written only when `rightkit-fs` has its `json`
//! feature on, is diagnostic evidence and never decides).
//! A second order that cannot take the claim at once is answered with the fixed code `app_busy`
//! and writes, deletes and reads nothing. The claim is taken after the directory and marker checks
//! pass and those checks are repeated under it, so the marker state an order acts on cannot change
//! while it runs. Restore and completion are idempotent by snapshot id and manifest digest (the
//! `cur` record), so a runner that retries after `app_busy` or a lost reply converges; a snapshot
//! id is single use (a repeat is refused and changes nothing).
//!
//! # Interrupted version 1 markers
//!
//! A restore marker written by the version 1 protocol has `credentials.json` holding values (an
//! `items` array with `value` and `sha256`, no `schema` key) and no `cur` record in the snapshot
//! store. This owner never reads, restores, completes or deletes such a snapshot: a restore or
//! completion whose manifest file has no `schema` key, and any order whose wire schema is
//! `rightkit.qa.credential-owner/1`, is refused with `legacy_v1_marker` before the claim, before
//! any store is opened and before any sweep. The runner must finish a version 1 marker with its
//! version 1 path before any version 2 snapshot (RUNNER-CHANGES.md, section 6).
//!
//! # The three-launch protocol (crash safety)
//!
//! One process per order, so every guarantee is in the snapshot store. Its records (all in the
//! one [`SnapshotStore`] service): `cur` names the current generation, its manifest digest and
//! whether it is `ready` (restorable) or `done` (completed receipt); `pending` lists the
//! generations whose item slots may hold values; `g-<generation>-<n>` hold one declared item's
//! value each. The generation is the snapshot id. None of these is a credential the app reads.
//!
//! * snapshot: sweep stale generations; add the new generation to `pending`; write and read back
//!   its items; write the manifest file; then write `cur` (the commit); then sweep (this deletes
//!   the superseded generation). Until `cur` is written the previous snapshot stays restorable.
//! * restore: repeatable for the same snapshot id and manifest digest while `cur` is `ready`; it
//!   writes only the declared items and changes no record. When `cur` is `done` it answers
//!   `alreadyCompleted` and writes nothing.
//! * complete: the runner's order after it restored its file roots. Only if every declared item
//!   holds the snapshot's value (or is absent, as snapshotted) does it write `cur` as `done`
//!   (the commit that consumes the snapshot) and then delete the values. On `done` it answers
//!   `alreadyCompleted`.
//! * cleanup that fails is reported in the reply as `cleanup_incomplete` with counts and is
//!   retried at the end of every order and at the start of a snapshot; it never turns a committed
//!   operation into a failure and is never silent. A snapshot is refused with `cleanup_backlog`
//!   when `pending` is full of generations that cannot be removed.
//!
//! Every crash point and its recovery: HANDOFF.md, "Restore and snapshot protocol".
//!
//! # Lifecycle
//!
//! The process hosts one service and exits. One daemon host (stopped by drop), one `Owner` shared
//! with the daemon's request threads, one `Mutex`/`Condvar` pair for the phase, and one detached
//! watchdog thread that ends the process at [`PROCESS_LIMIT`] whatever else is blocked (a request
//! thread may be inside an OS credential call; nothing joins it). Until the operation starts the
//! wait is bounded by `IDLE_LIMIT`; the wait for the client to take the response is bounded by
//! `FLUSH_LIMIT`. Nothing outlives `run` and the watchdog bounds the process.

use rightkit_credentials::{Config, Namespace, Secret, Store, WindowsTarget};
#[cfg(any(windows, target_os = "macos"))]
use rightkit_daemon::SigningIdentity;
use rightkit_daemon::{
    host_with_options, valid_service_name, Allowlist, CallContext, Emit, Handler,
    HostIdentityError, HostIdentityReason, HostOptions, PeerPolicy, ServiceConfig, ServiceError,
    Suite,
};
#[cfg(windows)]
use rightkit_daemon::{
    verify_held_windows_publisher, HostProcess, SignatureError, WindowsPublisherPolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
#[cfg(windows)]
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The argv switch that selects this mode. It must be the only argument. The runner
/// (`snapshot::owner`) launches the installed app with it.
pub const SWITCH: &str = "--rightkit-qa-credentials";

/// Exit code of a refused argv.
pub const EXIT_USAGE: i32 = 2;
/// Exit code of the watchdog: the process outlived [`PROCESS_LIMIT`].
pub const EXIT_TIMEOUT: i32 = 3;

/// Wire schema of requests and responses (the runner's `OWNER_SCHEMA`). Version 2 adds the
/// completion order and the `alreadyCompleted` and `cleanup` response fields, and a restore no
/// longer consumes the snapshot; a runner that speaks version 1 is refused with `legacy_v1_marker`
/// after admission checks pass.
pub const SCHEMA: &str = "rightkit.qa.credential-owner/2";
/// Schema of the manifest file the owner writes into the snapshot directory. The file holds no
/// credential value; see the module documentation.
pub const MANIFEST_SCHEMA: &str = "rightkit.qa.credential-snapshot/2";
const MODE: &str = "credentials";
/// The shared QA runner names the service `qa-credentials-<fresh id>`.
const SERVICE_PREFIX: &str = "qa-credentials-";
/// The label the QA runner's daemon client uses. A claim, never proof.
const CALLER: &str = "right-qa";
const M_HEALTH: &str = "health";
const M_SNAPSHOT: &str = "qa_credentials_snapshot";
const M_RESTORE: &str = "qa_credentials_restore";
const M_COMPLETE: &str = "qa_credentials_complete";

const SNAPSHOTS_DIR: &str = "snapshots";
const MARKER_FILE: &str = "restore-marker.json";
/// Name of the manifest file. Kept from the earlier format so the runner's paths do not move; the
/// content is the manifest, not values.
const MANIFEST_FILE: &str = "credentials.json";
const MARKER_SCHEMA: u32 = 1;
const STATE_SNAPSHOTTING: &str = "snapshotting";
const STATE_RESTORING: &str = "restoring";

/// Largest restore marker the owner reads, in bytes (`validate_dir`).
pub const MAX_MARKER_BYTES: u64 = 64 * 1024;
/// Largest manifest the owner reads back or restores from, in bytes.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Largest control token file, in bytes (a 64-character token plus a line break needs 66).
pub const MAX_TOKEN_FILE_BYTES: u64 = 256;
/// Largest request frame the daemon accepts for this service, in bytes.
pub const MAX_REQUEST_FRAME: usize = 64 * 1024;
/// Longest `schema` text, in bytes.
const MAX_SCHEMA_BYTES: usize = 64;
/// Longest `snapshotDir` text, in bytes.
pub const MAX_PATH_BYTES: usize = 1024;
/// Most simultaneous connections (and Windows pipe instances) of the one-operation service.
const MAX_CONNECTIONS: usize = 4;

/// No operation arrived: leave instead of lingering as a credential-capable process.
const IDLE_LIMIT: Duration = Duration::from_secs(120);
/// After the operation, how long to wait for the client to take the response and hang up. Also the
/// daemon's per-frame write bound.
const FLUSH_LIMIT: Duration = Duration::from_secs(10);
/// The daemon offers no "response written" signal (inflight drops before the write), so the
/// process never exits sooner than this after the operation.
const FLUSH_MIN: Duration = Duration::from_millis(150);
/// A client that connects and says nothing is dropped after this.
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);
/// The whole process, from the start of `run`: idle wait, one operation and the flush. The
/// watchdog thread ends the process at this bound even if a request thread is blocked.
pub const PROCESS_LIMIT: Duration = Duration::from_secs(300);

/// Most declared sets, accounts per set, and items in all.
pub const MAX_SETS: usize = 16;
pub const MAX_ACCOUNTS_PER_SET: usize = 32;
pub const MAX_ITEMS: usize = 128;
/// Longest set, account, service or app name, in bytes.
const MAX_NAME_BYTES: usize = 256;

/// Refusal codes of the sender proof. Distinct on purpose; the text of each is fixed.
const REASON_UNAVAILABLE: &str = "runner_proof_unavailable";
const REASON_UNVERIFIABLE: &str = "runner_unverifiable";
const REASON_REJECTED: &str = "runner_rejected";

/// Refusal codes of the snapshot-store isolation gate (see the module documentation).
#[cfg_attr(target_os = "macos", allow(dead_code))]
const REASON_UNISOLATED: &str = "snapshot_store_unisolated";
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const REASON_ISOLATION: &str = "isolation_not_established";

/// Refusal code of a second order for an app whose claim another owner process holds (see the
/// module documentation, "Recovery exclusion"). Distinct from `busy`, which is a second order in
/// one process.
const REASON_APP_BUSY: &str = "app_busy";
/// Refusal code of a version 1 order or a version 1 snapshot (see the module documentation,
/// "Interrupted version 1 markers").
const REASON_LEGACY_V1: &str = "legacy_v1_marker";
/// The wire schema of the version 1 protocol, recognised only to be refused.
const LEGACY_SCHEMA_V1: &str = "rightkit.qa.credential-owner/1";
/// The per-app claim file, in `<managed root>/snapshots/<app>/`. Never deleted: removing a lock file
/// lets two processes lock two different files at one name.
const CLAIM_FILE: &str = "owner.lock";

/// Most bytes of the runner image hashed on Windows (the qualified runner is far smaller). An
/// image over this is not the qualified image and is refused as `runner_rejected`.
pub const MAX_RUNNER_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Refusal codes of the slot-identity gate (see the module documentation, "Slot identities"). Fixed
/// text; the same strings are the construction errors of [`OwnerHost::new`].
const REASON_SLOT_OVERLAP: &str = "slot_identity_overlap";
const REASON_SLOT_UNRESOLVED: &str = "slot_identity_unresolved";

/// Accounts of the control records in the [`SnapshotStore`] service. `cur` is the current
/// generation (id, manifest digest, `ready` or `done`); `pending` lists the generations whose item
/// slots may hold values. Both hold only ids, digests and counts, never a credential value.
const SHADOW_CURRENT: &str = "cur";
const SHADOW_PENDING: &str = "pending";
/// Prefix of every item account: `g-<generation>-<n>`.
const ITEM_PREFIX: &str = "g-";
/// What the credentials crate (`chunked.rs`, `chunk_account`) appends to an account for the derived
/// entries of a large value on Windows: `<account>.rkchunk.<generation>.<index>`. A copy of a
/// private crate constant; see HANDOFF.md, "Unproven".
const CHUNK_INFIX: &str = ".rkchunk.";
/// Stands for the open part of a slot family when its identity is resolved. A private-use
/// character: no case folding changes it and the crate's validation accepts it.
const FAMILY_VAR: char = '\u{E000}';
const CURRENT_VERSION: &str = "v1";
const CUR_READY: &str = "ready";
const CUR_DONE: &str = "done";
/// Most generations `pending` may list. A snapshot is refused with `cleanup_backlog` when stale
/// generations cannot be removed and the list is full, so the backlog never grows without bound.
const MAX_PENDING_GENERATIONS: usize = 4;
/// Longest control record the owner reads, in bytes.
const MAX_CONTROL_BYTES: usize = 1024;

/// The Windows credential target form of one declared set (the wire names are `slash` and
/// `keyring-compat`, as the runner's `CredRecord` carries them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerWindowsTarget {
    /// `<prefix>/<account>`; `windows_prefix` of the set is the prefix.
    Slash,
    /// The keyring-compat form; the set has no prefix.
    KeyringCompat,
}

impl OwnerWindowsTarget {
    fn wire(self) -> &'static str {
        match self {
            Self::Slash => "slash",
            Self::KeyringCompat => "keyring-compat",
        }
    }
}

/// One declared credential set, exactly as the app's QA configuration
/// (`[[snapshot.credentials]]`) lists it and as the app addresses it. The host owns every value.
#[derive(Debug, Clone, Copy)]
pub struct DeclaredSet {
    pub service: &'static str,
    pub windows_target: OwnerWindowsTarget,
    pub windows_prefix: Option<&'static str>,
    pub accounts: &'static [&'static str],
}

/// The one credential-store service the owner mode writes besides the declared sets: where the
/// snapshot's values are kept between the snapshot, restore and completion processes, never in a
/// file the runner can read. Declared by the host, with a name that differs from every declared
/// set and, resolved to native credential identities under the macOS and the Windows rule, with
/// no slot that can be a declared live slot ([`OwnerHost::new`], "Slot identities" in the module
/// documentation). The owner writes the control records `cur` and `pending` and, per generation, one
/// `g-<generation>-<n>` item per declared item into it (Windows target form: keyring-compat), and
/// removes them when the snapshot is completed, superseded or abandoned. The service name alone
/// isolates nothing: see the module documentation, "Where the snapshot store is allowed".
#[derive(Debug, Clone, Copy)]
pub struct SnapshotStore {
    pub service: &'static str,
    /// The installed app's own Developer ID identity, which the running process must validate
    /// against before it uses the store on macOS. Necessary and not sufficient: macOS refuses
    /// every order with `isolation_not_established` whatever is declared here until the credentials
    /// crate can prove a slot's access control (`mac_slot_acl`). `None` refuses in any case.
    /// Ignored on every other platform, where the store is refused.
    pub mac_app: Option<MacAppIdentity>,
}

/// The macOS code identity of the installed app that hosts the owner mode: a Developer ID
/// Application signature with exactly this team and code-signing identifier. It must not equal
/// the runner's identity ([`OwnerHost::new`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAppIdentity {
    /// Apple Developer team identifier: ten upper-case letters or digits.
    pub team_id: &'static str,
    /// The app executable's code-signing identifier.
    pub identifier: &'static str,
}

/// The macOS identity the runner must carry: a Developer ID Application signature with exactly
/// this team and this code-signing identifier. Supplied by the host from its build configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacRunnerIdentity {
    /// Apple Developer team identifier: ten upper-case letters or digits.
    pub team_id: &'static str,
    /// The runner executable's code-signing identifier.
    pub identifier: &'static str,
}

/// The Windows identity the runner must carry: the qualified runner image itself (its SHA-256),
/// carrying an Authenticode signature by this common name, whose verified chain has the pinned
/// parent CA and root CA and whose leaf carries the publisher EKU. A publisher match alone is not
/// accepted. Supplied by the host from its build configuration, from the runner's signed release
/// metadata; `None` in [`ExpectedRunner`] until RightKit provides and the host has verified it.
#[derive(Debug, Clone, Copy)]
pub struct WindowsRunnerIdentity {
    /// Exact signer common name.
    pub common_name: &'static str,
    /// Publisher-specific OID in the leaf's enhanced key usage extension (dotted decimal).
    pub identity_eku: &'static str,
    /// SHA-256 of the DER-encoded CA one level below the root.
    pub parent_ca_sha256: [u8; 32],
    /// SHA-256 of the DER-encoded trust root.
    pub root_ca_sha256: [u8; 32],
    /// SHA-256 over every byte of the qualified runner executable, the signed file as shipped
    /// (signature included), as the runner's signed release metadata records it. It is compared
    /// with the digest of the image held open for the check, at most
    /// [`MAX_RUNNER_IMAGE_BYTES`] bytes. Required, never all zero.
    pub image_sha256: [u8; 32],
}

/// The runner the host expects, per platform. It comes from the host's build configuration, never
/// from a request, the environment or a file the runner can write. A platform with no identity
/// refuses every order.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // each platform reads only its own identity
pub struct ExpectedRunner {
    mac: Option<MacRunnerIdentity>,
    windows: Option<WindowsRunnerIdentity>,
}

fn valid_oid(text: &str) -> bool {
    text.len() >= 3
        && text.len() <= 128
        && !text.starts_with('.')
        && !text.ends_with('.')
        && !text.contains("..")
        && text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && text != "1.3.6.1.5.5.7.3.3"
        && text != "2.5.29.37.0"
}

/// A macOS team (ten upper-case letters or digits) and code-signing identifier (ASCII letters,
/// digits, `.` and `-`, within the name bound): the values are spliced into a code requirement, so
/// nothing else is accepted.
fn valid_mac_identity(team_id: &str, identifier: &str) -> bool {
    team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && valid_name(identifier)
        && identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

impl ExpectedRunner {
    /// No identity is known for any platform: every order is refused with
    /// `runner_proof_unavailable`. This is the correct state until the host has an independently
    /// provided runner identity.
    pub const fn unconfigured() -> Self {
        Self {
            mac: None,
            windows: None,
        }
    }

    /// The identities the host expects. Refused: a macOS team that is not ten upper-case letters
    /// or digits, an identifier that is empty, longer than 256 bytes or holds anything but ASCII
    /// letters, digits, `.` and `-`; a Windows common name that is empty, over-long or holds a
    /// control character, an EKU that is not a dotted-decimal OID of at most 128 bytes (the
    /// generic code-signing and any-purpose OIDs are refused), a CA digest or the image digest
    /// that is all zero, or two equal CA digests.
    pub fn new(
        mac: Option<MacRunnerIdentity>,
        windows: Option<WindowsRunnerIdentity>,
    ) -> Result<Self, String> {
        if let Some(m) = &mac {
            if !valid_mac_identity(m.team_id, m.identifier) {
                return Err("the macOS runner identity is invalid".to_string());
            }
        }
        if let Some(w) = &windows {
            if !valid_name(w.common_name)
                || !valid_oid(w.identity_eku)
                || w.parent_ca_sha256 == [0u8; 32]
                || w.root_ca_sha256 == [0u8; 32]
                || w.image_sha256 == [0u8; 32]
                || w.parent_ca_sha256 == w.root_ca_sha256
            {
                return Err("the Windows runner identity is invalid".to_string());
            }
        }
        Ok(Self { mac, windows })
    }

    /// The policy for the running platform, or `None` when this platform has no expected identity.
    fn policy(&self) -> Option<PeerPolicy> {
        #[cfg(target_os = "macos")]
        {
            self.mac.map(|m| {
                PeerPolicy::signed_by(SigningIdentity::MacDeveloperId {
                    team_id: m.team_id.to_string(),
                    identifier: m.identifier.to_string(),
                })
            })
        }
        #[cfg(windows)]
        {
            self.windows.map(|w| {
                PeerPolicy::signed_by(SigningIdentity::WindowsCommonName(
                    w.common_name.to_string(),
                ))
            })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            None
        }
    }
}

/// Everything the host supplies. Built by [`OwnerHost::new`], which validates it.
#[derive(Clone)]
pub struct OwnerHost {
    app: &'static str,
    declared: &'static [DeclaredSet],
    snapshot_store: SnapshotStore,
    version: &'static str,
    runner: ExpectedRunner,
    /// Every native identity (exact and derived) of every declared live slot, resolved by
    /// [`OwnerHost::new`]; the guard of every snapshot-store write and delete compares against it.
    live_slots: Vec<Ident>,
}

fn valid_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_NAME_BYTES && !text.chars().any(char::is_control)
}

/// Bounded text without control characters (an empty text is allowed; the caller compares).
fn bounded_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control)
}

impl OwnerHost {
    /// `app` is the one path component the runner names the snapshot directory after
    /// (`snapshots/<app>/<snapshotId>`); `declared` the sets this app keeps, in the order the
    /// runner's configuration lists them; `snapshot_store` the one extra service the snapshot's
    /// values are kept in; `version` is reported by the service; `runner` the expected runner
    /// identity. Refused: an empty or oversized list, more than [`MAX_SETS`] sets,
    /// [`MAX_ACCOUNTS_PER_SET`] accounts per set or [`MAX_ITEMS`] items, an empty set, an empty or
    /// control-character name, a non-component `app`, a snapshot service equal to a declared
    /// service, a `Slash` set without a prefix or a `KeyringCompat` set with one, an invalid
    /// macOS app identity, a macOS runner identity equal to the macOS app identity, and any slot
    /// identity failure: `slot_identity_unresolved` when a slot cannot be resolved to its native
    /// identity, `slot_identity_overlap` when two declared live slots share a native credential or a
    /// snapshot-store slot (`cur`, `pending`, any `g-...` item, or a chunk entry of one) can be a
    /// declared live slot, under the macOS or the Windows rule (module documentation, "Slot
    /// identities"). Those two errors are exactly the code text, with nothing else in it.
    pub fn new(
        app: &'static str,
        declared: &'static [DeclaredSet],
        snapshot_store: SnapshotStore,
        version: &'static str,
        runner: ExpectedRunner,
    ) -> Result<Self, String> {
        if !valid_name(app)
            || !app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            || app == "."
            || app == ".."
        {
            return Err("the app name is not a single path component".to_string());
        }
        if !valid_name(version) {
            return Err("the version text is empty or invalid".to_string());
        }
        if !valid_name(snapshot_store.service) {
            return Err("the snapshot store service name is invalid".to_string());
        }
        if let Some(mac_app) = &snapshot_store.mac_app {
            if !valid_mac_identity(mac_app.team_id, mac_app.identifier) {
                return Err("the macOS app identity is invalid".to_string());
            }
            if runner
                .mac
                .is_some_and(|m| m.team_id == mac_app.team_id && m.identifier == mac_app.identifier)
            {
                return Err("the runner identity must not be the app identity".to_string());
            }
        }
        if declared.is_empty() || declared.len() > MAX_SETS {
            return Err("the declared credential sets are empty or too many".to_string());
        }
        let mut items = 0usize;
        for set in declared {
            let prefix_ok = match set.windows_target {
                OwnerWindowsTarget::Slash => set.windows_prefix.is_some_and(valid_name),
                OwnerWindowsTarget::KeyringCompat => set.windows_prefix.is_none(),
            };
            if !valid_name(set.service)
                || set.service == snapshot_store.service
                || !prefix_ok
                || set.accounts.is_empty()
                || set.accounts.len() > MAX_ACCOUNTS_PER_SET
                || !set.accounts.iter().all(|account| valid_name(account))
            {
                return Err("a declared credential set is invalid".to_string());
            }
            items = items.saturating_add(set.accounts.len());
        }
        if items > MAX_ITEMS {
            return Err("too many declared credential items".to_string());
        }
        let live_slots = resolve_slots(declared, &snapshot_store).map_err(str::to_string)?;
        Ok(Self {
            app,
            declared,
            snapshot_store,
            version,
            runner,
            live_slots,
        })
    }
}

/// The rightkit-qa credential record wire shape (camelCase), as the runner's marker and request
/// carry it. Unknown keys are ignored here; the four fields are compared exactly after the bounds
/// check.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredRecord {
    service: String,
    #[serde(default)]
    windows_target: Option<String>,
    #[serde(default)]
    windows_prefix: Option<String>,
    accounts: Vec<String>,
}

/// One item of the manifest: which declared item, and whether it held a value when snapshotted.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestItem {
    set: usize,
    account: String,
    present: bool,
}

/// The manifest file. It holds no credential value, no digest of one and no key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: String,
    app: String,
    snapshot_id: String,
    items: Vec<ManifestItem>,
}

/// The fields of the runner's `restore-marker.json` this owner checks.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    schema: u32,
    snapshot_id: String,
    app: String,
    state: String,
    snapshot_dir: String,
    credentials: Vec<CredRecord>,
    #[serde(default)]
    credentials_sha256: Option<String>,
}

/// Authenticated params, after the token is stripped. There is deliberately no field for a path or
/// an identity of the caller: unknown fields are refused.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema: String,
    snapshot_id: String,
    snapshot_dir: String,
    credentials: Vec<CredRecord>,
    #[serde(default)]
    credentials_sha256: Option<String>,
}

/// A refusal. The text is fixed wording plus set/account names; it never holds a value.
struct Refusal {
    code: &'static str,
    message: String,
}

fn refuse<T>(code: &'static str, message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        code,
        message: message.into(),
    })
}

// ---------------------------------------------------------------------------- bounded reads

/// The one way this module reads a file: ONE handle, opened without following a link, a regular
/// file with a single hard link (on Windows opened with read-only sharing, so nobody can write,
/// rename or delete it while it is held), checked to be private to the current user while it is
/// held (on Unix the path is also proven to still name that very file), and read through the same
/// handle with a byte cap: at most `cap` bytes, one byte more is a refusal. There is no metadata
/// check followed by a reopen.
fn read_private_capped(path: &Path, cap: u64) -> Result<Vec<u8>, ()> {
    let options = rightkit_fs::ReadOptions::new()
        .require_single_link(true)
        .share_read_only(true);
    let mut file = rightkit_fs::open_read_nofollow(path, &options).map_err(|_| ())?;
    rightkit_fs::private::verify_owner_only(path).map_err(|_| ())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata().map_err(|_| ())?;
        let named = fs::symlink_metadata(path).map_err(|_| ())?;
        if held.dev() != named.dev() || held.ino() != named.ino() {
            return Err(());
        }
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > cap {
        return Err(());
    }
    Ok(bytes)
}

/// The per-run QA control token: the file `RIGHTKIT_QA_CONTROL_TOKEN_FILE` names (an absolute path
/// without `.` or `..`), read with `read_private_capped`, exactly one token of lowercase hex.
fn read_token() -> Result<String, ()> {
    let raw = std::env::var_os(crate::control::token::ENV_AUTH_FILE).ok_or(())?;
    let path = PathBuf::from(raw);
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(());
    }
    let bytes = read_private_capped(&path, MAX_TOKEN_FILE_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ())?.trim();
    let well_formed = text.len() == crate::control::token::HEX_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    well_formed.then(|| text.to_string()).ok_or(())
}

// ---------------------------------------------------------------------------- sender proof

fn unverifiable() -> ServiceError {
    ServiceError::new(
        REASON_UNVERIFIABLE,
        "the caller process could not be identified or its signature could not be checked",
    )
}

fn rejected() -> ServiceError {
    ServiceError::new(
        REASON_REJECTED,
        "the caller process is not the expected signed runner",
    )
}

/// Map a daemon identity refusal to one of the two fixed refusals. The daemon's own text carries
/// pids and paths and is never forwarded.
fn classify(error: HostIdentityError) -> ServiceError {
    match error.reason {
        HostIdentityReason::Refused(_) | HostIdentityReason::ExecutableMismatch => rejected(),
        _ => unverifiable(),
    }
}

#[cfg(windows)]
fn same_windows_path(a: &Path, b: &Path) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => plain_path(a).eq_ignore_ascii_case(plain_path(b)),
        _ => false,
    }
}

/// SHA-256 of the whole file behind `held`, computed on that handle from offset 0 with a byte cap:
/// `Ok(Some(digest))`, `Ok(None)` when the file is longer than `cap` (nothing is hashed past it),
/// `Err` when it cannot be read. The handle is left at offset 0. One handle, no reopen, and the
/// buffer is a fixed 64 KiB whatever the file size.
#[cfg(windows)]
fn held_sha256_capped(held: &fs::File, cap: u64) -> Result<Option<[u8; 32]>, ()> {
    let mut file: &fs::File = held;
    file.seek(SeekFrom::Start(0)).map_err(|_| ())?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(()),
        };
        total = total.checked_add(read as u64).ok_or(())?;
        if total > cap {
            return Ok(None);
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| ())?;
    Ok(Some(hasher.finalize_bytes()))
}

/// Windows: the image digest, the chain, publisher EKU and revocation checks the daemon's path
/// check does not make, on one handle that denies write, rename and delete while it is held. The
/// held file must be the image the OS reports for the peer; its SHA-256, computed on that handle,
/// must be the qualified runner's ([`WindowsRunnerIdentity::image_sha256`]); and the peer,
/// identified again afterwards, must be the same process image. A different or over-long image is
/// `runner_rejected`; an unreadable one is `runner_unverifiable`.
#[cfg(windows)]
fn check_windows_publisher(
    cx: &CallContext,
    policy: &PeerPolicy,
    first: &HostProcess,
    expected: &WindowsRunnerIdentity,
) -> Result<(), ServiceError> {
    let options = rightkit_fs::ReadOptions::new().share_read_only(true);
    let held =
        rightkit_fs::open_read_nofollow(&first.executable, &options).map_err(|_| unverifiable())?;
    let held_path = rightkit_fs::final_path(&held).map_err(|_| unverifiable())?;
    if !same_windows_path(&held_path, &first.executable) {
        return Err(unverifiable());
    }
    match held_sha256_capped(&held, MAX_RUNNER_IMAGE_BYTES) {
        Ok(Some(digest)) if digest == expected.image_sha256 => {}
        Ok(_) => return Err(rejected()),
        Err(()) => return Err(unverifiable()),
    }
    let publisher = WindowsPublisherPolicy {
        common_name: expected.common_name,
        identity_eku: expected.identity_eku,
        parent_ca_sha256: expected.parent_ca_sha256,
        root_ca_sha256: expected.root_ca_sha256,
    };
    verify_held_windows_publisher(&held, &publisher).map_err(|error| match error {
        SignatureError::NotSigned(_) => rejected(),
        _ => unverifiable(),
    })?;
    let second = cx.verify_peer_signed(policy).map_err(classify)?;
    if second.pid != first.pid
        || !same_windows_path(&second.executable, &first.executable)
        || cx.peer_closed()
    {
        return Err(unverifiable());
    }
    Ok(())
}

/// The sender proof of one order. See the module documentation.
fn prove_runner(cx: &CallContext, expected: &ExpectedRunner) -> Result<(), ServiceError> {
    let Some(policy) = expected.policy() else {
        return Err(ServiceError::new(
            REASON_UNAVAILABLE,
            "no expected runner identity is configured for this platform",
        ));
    };
    #[cfg_attr(not(windows), allow(unused_variables))]
    let first = cx.verify_peer_signed(&policy).map_err(classify)?;
    #[cfg(windows)]
    {
        let Some(windows) = expected.windows.as_ref() else {
            return Err(ServiceError::new(
                REASON_UNAVAILABLE,
                "no expected runner identity is configured for this platform",
            ));
        };
        check_windows_publisher(cx, &policy, &first, windows)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------- isolation

/// macOS: validate the RUNNING process (`SecCodeCopySelf`) against the host-declared app identity
/// with the same Developer ID requirement the daemon applies to a peer. This establishes only that
/// this process is the signed app. It does NOT establish the access control of any keychain item:
/// it reads nothing from the keychain, and an item that already exists keeps the access control it
/// has whoever updates it (see `mac_slot_acl`). A necessary condition of the isolation gate, not a
/// sufficient one.
#[cfg(target_os = "macos")]
mod mac_self {
    use super::MacAppIdentity;
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    type OSStatus = i32;
    /// `kCFStringEncodingUTF8`.
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecCodeCopySelf(flags: u32, out: *mut *mut c_void) -> OSStatus;
        fn SecRequirementCreateWithString(
            text: *const c_void,
            flags: u32,
            out: *mut *mut c_void,
        ) -> OSStatus;
        fn SecCodeCheckValidity(
            code: *mut c_void,
            flags: u32,
            requirement: *mut c_void,
        ) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            is_external: u8,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    /// The Developer ID requirement of rightkit-daemon 0.9.1 (`mac_developer_id_requirement`).
    /// The team and identifier are validated by `valid_mac_identity` before they reach here.
    fn requirement_text(app: &MacAppIdentity) -> String {
        format!(
            "anchor apple generic and certificate leaf[subject.OU] = \"{}\" \
             and identifier \"{}\" \
             and certificate 1[field.1.2.840.113635.100.6.2.6] \
             and certificate leaf[field.1.2.840.113635.100.6.1.13]",
            app.team_id, app.identifier
        )
    }

    /// `Ok` only when the running process satisfies the requirement; every other outcome is `Err`.
    pub(super) fn verify_self(app: &MacAppIdentity) -> Result<(), ()> {
        let text = requirement_text(app);
        // SAFETY: every pointer passed is either a live buffer for the call's duration or an
        // out-pointer to a local; each Core Foundation object created here is released exactly
        // once on every path.
        unsafe {
            let cf_text = CFStringCreateWithBytes(
                null(),
                text.as_ptr(),
                text.len() as isize,
                CF_STRING_ENCODING_UTF8,
                0,
            );
            if cf_text.is_null() {
                return Err(());
            }
            let mut requirement: *mut c_void = null_mut();
            let status = SecRequirementCreateWithString(cf_text, 0, &mut requirement);
            CFRelease(cf_text);
            if status != 0 || requirement.is_null() {
                return Err(());
            }
            let mut code: *mut c_void = null_mut();
            let status = SecCodeCopySelf(0, &mut code);
            let valid =
                status == 0 && !code.is_null() && SecCodeCheckValidity(code, 0, requirement) == 0;
            if !code.is_null() {
                CFRelease(code as *const c_void);
            }
            CFRelease(requirement as *const c_void);
            if valid {
                Ok(())
            } else {
                Err(())
            }
        }
    }
}

/// macOS: the per-slot access-control proof the snapshot store needs and the credentials crate
/// (0.5.6) cannot give. The crate's macOS backend is the legacy file keychain
/// (`SecKeychainAddGenericPassword`, `SecKeychainItemModifyAttributesAndData`,
/// `SecKeychainItemDelete`); it has no call that reads an item's access object
/// (`SecKeychainItemCopyAccess`, `SecAccessCopyACLList`, `SecACLCopyContents`) and none that
/// creates an item with an explicit one (`SecKeychainItemCreateFromContent`). Both functions here
/// therefore refuse, and nothing in this module may be written to claim otherwise until the crate
/// provides the API specified in HANDOFF.md ("Crate-side API needed"), at which point they are the
/// only two places to change: `mac_slot_acl::before_write` becomes the crate's isolated write,
/// and `mac_slot_acl::established` the proof that every slot the order can write is isolated.
#[cfg(target_os = "macos")]
mod mac_slot_acl {
    /// Whether every snapshot-store slot an order can write has an access control that admits only
    /// this app. Always `false` today: there is no call to read one.
    pub(super) fn established() -> bool {
        false
    }

    /// Before a slot is written (created or updated). Always an error today, so a pre-existing slot
    /// with a foreign access control is never updated in place and no slot is created with an
    /// access control nobody read back.
    pub(super) fn before_write() -> Result<(), ()> {
        Err(())
    }
}

/// The isolation gate of the snapshot store: `Ok` only where the process can establish that the
/// runner cannot read what is kept in it. See the module documentation for what is established on
/// each platform and what is not. The pair is the fixed refusal code and its fixed text; no value.
/// Called for every order by `Owner::admit` and again by `open_shadow`, the only place the
/// snapshot store is opened.
fn snapshot_isolation(host: &OwnerHost) -> Result<(), (&'static str, &'static str)> {
    #[cfg(target_os = "macos")]
    {
        // The signed identity is necessary, never sufficient: the access control of a slot is
        // what keeps the runner out, and `mac_slot_acl::established` is the proof of that.
        let identity = host
            .snapshot_store
            .mac_app
            .as_ref()
            .is_some_and(|app| mac_self::verify_self(app).is_ok());
        if identity && mac_slot_acl::established() {
            Ok(())
        } else {
            Err((
                REASON_ISOLATION,
                "the snapshot store is not established as isolated from the runner",
            ))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = host;
        Err((
            REASON_UNISOLATED,
            "this platform has no snapshot store that is isolated from the runner",
        ))
    }
}

// ---------------------------------------------------------------------------- service

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Done(bool),
}

struct Owner {
    host: OwnerHost,
    token: String,
    phase: Mutex<Phase>,
    changed: Condvar,
}

impl Owner {
    fn set_phase(&self, next: Phase) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = next;
        self.changed.notify_all();
    }

    /// The sender proof first, then the published token check (missing token or a wrong token is
    /// `unauthorized`), then the slot-identity gate, then the snapshot-store isolation gate, then
    /// the token removed from the
    /// params. A refused order consumes nothing and learns nothing about the token. The isolation
    /// refusal is also written once to stderr as a fixed line, because the runner does not carry
    /// an owner's refusal code into its own message.
    fn admit(&self, cx: &CallContext, params: &Value) -> Result<Value, ServiceError> {
        prove_runner(cx, &self.host.runner)?;
        crate::control::token::authorize(Some(self.token.as_str()), params)
            .map_err(|r| ServiceError::new(crate::control::token::REFUSED_CODE, r.message()))?;
        // Pure computation, no store access: every order re-proves the slot identities before the
        // isolation gate and before any store is opened.
        if let Err(code) = resolve_slots(self.host.declared, &self.host.snapshot_store) {
            eprintln!("qa credential owner: slot identity refused");
            return Err(ServiceError::new(code, slot_refusal_text(code)));
        }
        if let Err((code, text)) = snapshot_isolation(&self.host) {
            eprintln!("qa credential owner: snapshot store isolation refused");
            return Err(ServiceError::new(code, text));
        }
        let mut stripped = params.clone();
        if let Some(map) = stripped.as_object_mut() {
            map.remove(crate::control::token::PARAM);
        }
        Ok(stripped)
    }

    /// Exactly one credential operation per process; success or failure ends the run.
    fn once(
        &self,
        operation: impl FnOnce() -> Result<Value, Refusal>,
    ) -> Result<Value, ServiceError> {
        {
            let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
            if *phase != Phase::Idle {
                return Err(ServiceError::new(
                    "busy",
                    "one credential operation per process",
                ));
            }
            *phase = Phase::Running;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
            .unwrap_or_else(|_| {
                Err(Refusal {
                    code: "failed",
                    message: "the credential operation failed".to_string(),
                })
            });
        self.set_phase(Phase::Done(outcome.is_ok()));
        outcome.map_err(|r| ServiceError::new(r.code, r.message))
    }
}

impl Handler for Owner {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        let params = self.admit(cx, params)?;
        match method {
            M_HEALTH => Ok(json!({"schema": SCHEMA, "mode": MODE})),
            M_SNAPSHOT => self.once(|| op_snapshot(&self.host, &params)),
            M_RESTORE => self.once(|| op_restore(&self.host, &params)),
            M_COMPLETE => self.once(|| op_complete(&self.host, &params)),
            _ => Err(ServiceError::new("unknown_method", "unknown method")),
        }
    }

    fn connection_closed(&self, _conn: u64) {
        self.changed.notify_all();
    }
}

fn fail(reason: &str) -> i32 {
    eprintln!("qa credential owner: {reason}");
    1
}

/// `None` means ordinary startup. Any invocation that names the switch (or something that starts
/// with it) exits here with `Some(code)` and never reaches the host's data root, update dispatch,
/// UI, telemetry, licence, cache or engine startup. Call it first thing in `main`.
///
/// `host` builds the host configuration and is called only after the argv is accepted, so an
/// ordinary start-up never builds it and a configuration error can only stop this mode.
pub fn dispatch(host: impl FnOnce() -> Result<OwnerHost, String>) -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if !args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with(SWITCH))
    {
        return None;
    }
    if args.len() != 1 || args[0] != SWITCH {
        eprintln!("qa credential owner: the switch takes no other argument");
        return Some(EXIT_USAGE);
    }
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    {
        match host() {
            Ok(host) => Some(run(host)),
            Err(e) => Some(fail(match e.as_str() {
                REASON_SLOT_OVERLAP => "the host configuration is invalid: slot_identity_overlap",
                REASON_SLOT_UNRESOLVED => {
                    "the host configuration is invalid: slot_identity_unresolved"
                }
                _ => "the host configuration is invalid",
            })),
        }
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = host;
        eprintln!("qa credential owner: unsupported platform");
        Some(EXIT_USAGE)
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn run(host: OwnerHost) -> i32 {
    // No panic text, ever: nothing in this mode may print a payload.
    std::panic::set_hook(Box::new(|_| {}));

    // The watchdog is the process bound: a request thread may be blocked inside an OS credential
    // call and nothing here joins it. The thread ends with the process.
    let started = Instant::now();
    if std::thread::Builder::new()
        .name("qa-owner-watchdog".into())
        .spawn(move || {
            std::thread::sleep(PROCESS_LIMIT);
            std::process::exit(EXIT_TIMEOUT);
        })
        .is_err()
    {
        return fail("the process watchdog could not start");
    }

    let service = match std::env::var("RIGHTKIT_CONTROL_SERVICE") {
        Ok(s) if valid_service_name(&s) && s.starts_with(SERVICE_PREFIX) => s,
        _ => return fail("the control service name is missing or not a qa-credentials name"),
    };
    let Ok(token) = read_token() else {
        return fail("the QA control token is missing or unusable");
    };
    // Default suite only; no override of the root is read.
    let suite = match Suite::per_user_default() {
        Ok(suite) => suite,
        Err(_) => return fail("the control suite root cannot be resolved"),
    };
    let version = host.version;
    let owner = Arc::new(Owner {
        host,
        token,
        phase: Mutex::new(Phase::Idle),
        changed: Condvar::new(),
    });
    let allow = Allowlist::new().allow(CALLER, [M_HEALTH, M_SNAPSHOT, M_RESTORE, M_COMPLETE]);
    let mut cfg = ServiceConfig::new(&service, "control:qa-credentials", 0, version, allow);
    cfg.frame_limit = MAX_REQUEST_FRAME;
    let options = HostOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .max_pipe_instances(MAX_CONNECTIONS as u32)
        .reserved_slots(0)
        .supervisor_pid(None)
        .handshake_timeout(Some(HANDSHAKE_LIMIT))
        .write_timeout(Some(FLUSH_LIMIT));
    let hosted = match host_with_options(&suite, cfg, owner.clone(), options) {
        Ok(hosted) => hosted,
        Err(_) => return fail("the control service could not be hosted"),
    };

    // Wait for the one operation (the watchdog bounds it), then for the client to take the
    // response and hang up.
    let mut phase = owner.phase.lock().unwrap_or_else(|e| e.into_inner());
    let ok = loop {
        let current = *phase;
        match current {
            Phase::Done(ok) => break ok,
            Phase::Idle if started.elapsed() > IDLE_LIMIT => {
                drop(phase);
                drop(hosted);
                return fail("no credential operation arrived");
            }
            _ => {}
        }
        phase = owner
            .changed
            .wait_timeout(phase, Duration::from_secs(1))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    };
    drop(phase);
    let done_at = Instant::now();
    loop {
        let waited = done_at.elapsed();
        if waited >= FLUSH_LIMIT || (waited >= FLUSH_MIN && hosted.status().connections == 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(hosted);
    if ok {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------- request checks

fn valid_snapshot_id(id: &str) -> bool {
    // The runner's `new_id`: lowercase hyphenated version-4 form, 36 characters.
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

fn valid_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Every text and list of the credential records is within the bounds the host's own limits give,
/// before any of it is compared.
fn records_bounded(sets: &[CredRecord]) -> bool {
    sets.len() <= MAX_SETS
        && sets.iter().all(|set| {
            bounded_text(&set.service, MAX_NAME_BYTES)
                && set
                    .windows_target
                    .as_deref()
                    .is_none_or(|t| bounded_text(t, 32))
                && set
                    .windows_prefix
                    .as_deref()
                    .is_none_or(|p| bounded_text(p, MAX_NAME_BYTES))
                && set.accounts.len() <= MAX_ACCOUNTS_PER_SET
                && set.accounts.iter().all(|a| bounded_text(a, MAX_NAME_BYTES))
        })
}

/// Exactly the declared sets in declared order, nothing else.
fn is_declared(host: &OwnerHost, sets: &[CredRecord]) -> bool {
    sets.len() == host.declared.len()
        && sets.iter().zip(host.declared.iter()).all(|(got, want)| {
            got.service == want.service
                && got.windows_target.as_deref() == Some(want.windows_target.wire())
                && got.windows_prefix.as_deref() == want.windows_prefix
                && got
                    .accounts
                    .iter()
                    .map(String::as_str)
                    .eq(want.accounts.iter().copied())
        })
}

/// `(set index, account)` of every declared item, in order.
fn expected_items(host: &OwnerHost) -> Vec<(usize, &'static str)> {
    host.declared
        .iter()
        .enumerate()
        .flat_map(|(set, d)| d.accounts.iter().map(move |account| (set, *account)))
        .collect()
}

/// `with_digest` is true for the restore and completion orders, which must carry the manifest
/// digest, and false for the snapshot order, which must not.
fn parse_request(host: &OwnerHost, params: &Value, with_digest: bool) -> Result<Request, Refusal> {
    // A version 1 order is never served and never touches a store, a claim or a file.
    if params.get("schema").and_then(Value::as_str) == Some(LEGACY_SCHEMA_V1) {
        return refuse(REASON_LEGACY_V1, legacy_refusal_text());
    }
    let req: Request = match serde_json::from_value(params.clone()) {
        Ok(req) => req,
        Err(_) => return refuse("bad_request", "malformed request"),
    };
    if !bounded_text(&req.schema, MAX_SCHEMA_BYTES)
        || !bounded_text(&req.snapshot_dir, MAX_PATH_BYTES)
        || !records_bounded(&req.credentials)
    {
        return refuse("bad_request", "a request field is oversized or invalid");
    }
    if req.schema != SCHEMA {
        return refuse("bad_request", "unsupported schema");
    }
    if !valid_snapshot_id(&req.snapshot_id) {
        return refuse("bad_request", "malformed snapshot id");
    }
    if !is_declared(host, &req.credentials) {
        return refuse(
            "bad_request",
            "the credential sets are not the declared sets",
        );
    }
    match (&req.credentials_sha256, with_digest) {
        (Some(sha), true) if valid_sha256(sha) => {}
        (None, false) => {}
        _ => {
            return refuse(
                "bad_request",
                "credentialsSha256 is required for restore and completion only",
            )
        }
    }
    Ok(req)
}

// ---------------------------------------------------------------------------- path checks

fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// `root` and every directory below it down to `leaf` is a real directory (no symlink, no
/// reparse point).
fn verify_real_chain(root: &Path, leaf: &Path) -> Result<(), Refusal> {
    let Ok(below) = leaf.strip_prefix(root) else {
        return refuse(
            "refused",
            "the snapshot directory is outside the managed root",
        );
    };
    let mut current: PathBuf = root.to_path_buf();
    let mut chain = vec![current.clone()];
    for part in below.components() {
        current.push(part);
        chain.push(current.clone());
    }
    for dir in chain {
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !is_link_or_reparse(&meta) => {}
            _ => return refuse("refused", "the snapshot path is not a real directory chain"),
        }
    }
    Ok(())
}

/// A Windows verbatim disk path (`\\?\D:\x`) without its `\\?\` prefix; any other path unchanged
/// (a verbatim UNC path keeps its prefix: it is not a managed-root form).
fn plain_path(path: &str) -> &str {
    match path.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => rest,
        _ => path,
    }
}

/// The snapshot directory the request names must be exactly
/// `<managed root>/snapshots/<app>/<snapshotId>`, real, and private to this user; the restore
/// marker (read through `read_private_capped`) must say the same thing in the expected state.
/// Returns that directory.
fn validate_dir(
    host: &OwnerHost,
    req: &Request,
    state: &str,
    expected_credentials_sha: Option<&str>,
) -> Result<PathBuf, Refusal> {
    let root = crate::storage::managed_root(None);
    if !root.is_absolute() {
        return refuse("refused", "the managed root is not an absolute path");
    }
    let app_dir = root.join(SNAPSHOTS_DIR).join(host.app);
    let snap_dir = app_dir.join(&req.snapshot_id);
    // The runner names the directory in the form its own path resolution produced: on Windows that
    // is the verbatim form (`\\?\D:\...`) of the same path this process builds as `D:\...`. The two
    // are compared as text once that one prefix is removed; nothing else is normalised.
    if snap_dir.to_str().map(plain_path) != Some(plain_path(req.snapshot_dir.as_str())) {
        return refuse("refused", "the snapshot directory is not the managed one");
    }
    verify_real_chain(&root, &snap_dir)?;
    // Resolve through the OS too: the real path must be the expected path under the real root.
    let canon = |p: &Path| fs::canonicalize(p);
    match (canon(&root), canon(&snap_dir)) {
        (Ok(real_root), Ok(real_snap))
            if real_snap
                == real_root
                    .join(SNAPSHOTS_DIR)
                    .join(host.app)
                    .join(&req.snapshot_id) => {}
        _ => {
            return refuse(
                "refused",
                "the snapshot directory does not resolve to itself",
            )
        }
    }
    for dir in [&app_dir, &snap_dir] {
        if rightkit_fs::private::verify_owner_only(dir).is_err() {
            return refuse(
                "refused",
                "a snapshot directory is not private to the current user",
            );
        }
    }

    let marker_path = app_dir.join(MARKER_FILE);
    let Ok(marker_bytes) = read_private_capped(&marker_path, MAX_MARKER_BYTES) else {
        return refuse("refused", "the restore marker cannot be read");
    };
    let marker: Marker = match serde_json::from_slice(&marker_bytes) {
        Ok(marker) => marker,
        Err(_) => return refuse("refused", "the restore marker is unreadable"),
    };
    if marker.schema != MARKER_SCHEMA
        || marker.app != host.app
        || marker.state != state
        || marker.snapshot_id != req.snapshot_id
        || marker.snapshot_dir != req.snapshot_dir
        || marker.credentials != req.credentials
        || marker.credentials_sha256.as_deref() != expected_credentials_sha
    {
        return refuse("refused", "the restore marker does not match the request");
    }
    Ok(snap_dir)
}

// ---------------------------------------------------------------------------- recovery exclusion

/// The per-app claim of one owner order. Held until dropped; the OS also drops it when the process
/// ends, so a killed owner leaves nothing to judge.
struct AppClaim {
    /// Held for its `Drop`, which releases the lock; never read.
    _lock: rightkit_fs::FileLock,
}

impl AppClaim {
    /// Take the claim of the app whose snapshot directory is `snap_dir`
    /// (`<managed root>/snapshots/<app>/<snapshotId>`, already verified by `validate_dir`): an
    /// exclusive lock on `owner.lock` in the app directory, tried once and never waited for. Held
    /// elsewhere is `app_busy`; any other failure is a refusal, never a granted claim.
    fn acquire(snap_dir: &Path) -> Result<Self, Refusal> {
        let Some(app_dir) = snap_dir.parent() else {
            return refuse("refused", "the snapshot directory has no app directory");
        };
        match rightkit_fs::FileLock::try_acquire(&app_dir.join(CLAIM_FILE)) {
            Ok(lock) => Ok(Self { _lock: lock }),
            Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => {
                refuse(
                    REASON_APP_BUSY,
                    "another credential owner order for this app is running",
                )
            }
            Err(_) => refuse("failed", "the per-app claim could not be taken"),
        }
    }
}

fn legacy_refusal_text() -> &'static str {
    "a version 1 snapshot or order is not served by this owner; the runner completes it with its version 1 path first"
}

/// Refuse a manifest file in the version 1 form (`credentials.json` holding values: an `items`
/// array and no `schema` key). Reads the file through `read_private_capped` and parses only the
/// optional `schema` field; the bytes are dropped at once (they stay in this process's heap until
/// then and are not zeroed; the runner can read the same file). A file that cannot be read or
/// parsed here is left to the later checks, which refuse it; nothing is consumed either way.
fn refuse_legacy_manifest(dir: &Path) -> Result<(), Refusal> {
    #[derive(Deserialize)]
    struct SchemaProbe {
        #[serde(default)]
        schema: Option<String>,
    }
    let Ok(bytes) = read_private_capped(&dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES) else {
        return Ok(());
    };
    match serde_json::from_slice::<SchemaProbe>(&bytes) {
        Ok(SchemaProbe { schema: None }) => refuse(REASON_LEGACY_V1, legacy_refusal_text()),
        _ => Ok(()),
    }
}

/// The directory and marker checks of an order, then the app's claim, then the same checks again
/// under it (the marker an order acts on cannot change while it holds the claim). `refuse_v1` is
/// set for restore and completion, which act on an existing manifest: a version 1 manifest is
/// refused before the claim is taken, so a refused version 1 order creates and changes nothing.
fn validate_and_claim(
    host: &OwnerHost,
    req: &Request,
    state: &str,
    expected_credentials_sha: Option<&str>,
    refuse_v1: bool,
) -> Result<(PathBuf, AppClaim), Refusal> {
    let dir = validate_dir(host, req, state, expected_credentials_sha)?;
    if refuse_v1 {
        refuse_legacy_manifest(&dir)?;
    }
    let claim = AppClaim::acquire(&dir)?;
    let dir = validate_dir(host, req, state, expected_credentials_sha)?;
    Ok((dir, claim))
}

// ---------------------------------------------------------------------------- store access

// ---------------------------------------------------------------------------- slot identities

/// One native credential identity as the credentials crate derives it, or a family of them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    /// One identity, folded as the crate folds it.
    Exact(String),
    /// Every identity that starts with `head`, ends with `tail` and has any text between them.
    Family { head: String, tail: String },
}

/// The two native naming rules of the credentials crate. Both are applied on every platform.
#[derive(Clone, Copy)]
enum Rule {
    /// Keychain service and account, case-folded (`macos_credential_identity`).
    MacKeychain,
    /// Credential Manager target name, case-folded (`windows_credential_identity`).
    WindowsCredMan,
}

const RULES: [Rule; 2] = [Rule::MacKeychain, Rule::WindowsCredMan];

/// The native identity of `(ns, account)` under `rule`: the crate's own function, nothing derived
/// here.
fn native_identity(rule: Rule, ns: &Namespace, account: &str) -> Result<String, ()> {
    match rule {
        Rule::MacKeychain => rightkit_credentials::macos_credential_identity(ns, account),
        Rule::WindowsCredMan => rightkit_credentials::windows_credential_identity(ns, account),
    }
    .map_err(|_| ())
}

/// The family of identities of every account that starts with `account_prefix`: the crate's
/// identity of the prefix plus `FAMILY_VAR`, split at that character. The prefix and the
/// namespace must not contain the placeholder, so it occurs exactly once.
fn family_ident(rule: Rule, ns: &Namespace, account_prefix: &str) -> Result<Ident, ()> {
    if account_prefix.contains(FAMILY_VAR) {
        return Err(());
    }
    let identity = native_identity(rule, ns, &format!("{account_prefix}{FAMILY_VAR}"))?;
    if identity.matches(FAMILY_VAR).count() != 1 {
        return Err(());
    }
    let (head, tail) = identity.split_once(FAMILY_VAR).ok_or(())?;
    Ok(Ident::Family {
        head: head.to_string(),
        tail: tail.to_string(),
    })
}

/// Everything one logical account can address, under both rules: its own identity and the derived
/// chunk entries of a large value.
fn account_idents(ns: &Namespace, account: &str) -> Result<Vec<Ident>, ()> {
    let mut out = Vec::with_capacity(RULES.len() * 2);
    for rule in RULES {
        out.push(Ident::Exact(native_identity(rule, ns, account)?));
        out.push(family_ident(rule, ns, &format!("{account}{CHUNK_INFIX}"))?);
    }
    Ok(out)
}

/// Everything the item accounts `g-<anything>` can address (every generation and item index, and
/// the chunk entries of each).
fn item_family_idents(ns: &Namespace) -> Result<Vec<Ident>, ()> {
    let mut out = Vec::with_capacity(RULES.len());
    for rule in RULES {
        out.push(family_ident(rule, ns, ITEM_PREFIX)?);
    }
    Ok(out)
}

/// Whether one string can be named by both identities.
fn idents_overlap(a: &Ident, b: &Ident) -> bool {
    match (a, b) {
        (Ident::Exact(x), Ident::Exact(y)) => x == y,
        (Ident::Exact(x), Ident::Family { head, tail })
        | (Ident::Family { head, tail }, Ident::Exact(x)) => {
            x.len() >= head.len().saturating_add(tail.len())
                && x.starts_with(head.as_str())
                && x.ends_with(tail.as_str())
        }
        (Ident::Family { head: h1, tail: t1 }, Ident::Family { head: h2, tail: t2 }) => {
            (h1.starts_with(h2.as_str()) || h2.starts_with(h1.as_str()))
                && (t1.ends_with(t2.as_str()) || t2.ends_with(t1.as_str()))
        }
    }
}

fn slots_overlap(a: &[Ident], b: &[Ident]) -> bool {
    a.iter().any(|x| b.iter().any(|y| idents_overlap(x, y)))
}

/// `Ok` only when every group of identities is disjoint from every other group in `groups`.
fn pairwise_disjoint(groups: &[Vec<Ident>]) -> bool {
    groups.iter().enumerate().all(|(i, a)| {
        groups
            .iter()
            .skip(i.saturating_add(1))
            .all(|b| !slots_overlap(a, b))
    })
}

/// The Namespace one declared set is opened with: used by the stores and by the identity proof, so
/// both see the same namespace.
fn declared_namespace(d: &DeclaredSet) -> Result<Namespace, ()> {
    let style = match d.windows_target {
        OwnerWindowsTarget::Slash => WindowsTarget::Slash {
            prefix: d.windows_prefix.map(str::to_string),
        },
        OwnerWindowsTarget::KeyringCompat => WindowsTarget::KeyringCompat,
    };
    Namespace::new(d.service)
        .and_then(|ns| ns.try_windows_target(style))
        .map_err(|_| ())
}

/// The Namespace of the snapshot store (keyring-compatible target form).
fn shadow_namespace(store: &SnapshotStore) -> Result<Namespace, ()> {
    Namespace::new(store.service)
        .and_then(|ns| ns.try_windows_target(WindowsTarget::KeyringCompat))
        .map_err(|_| ())
}

fn slot_refusal_text(code: &str) -> &'static str {
    if code == REASON_SLOT_OVERLAP {
        "a snapshot-store slot can address a declared credential, or two declared credentials share one native slot"
    } else {
        "a credential slot identity could not be resolved"
    }
}

/// The slot-identity proof (module documentation, "Slot identities"). No store is touched. On
/// success returns every identity, exact and derived, of every declared live slot. Bounded by the
/// declared limits (at most [`MAX_ITEMS`] live slots).
fn resolve_slots(
    declared: &[DeclaredSet],
    store: &SnapshotStore,
) -> Result<Vec<Ident>, &'static str> {
    let unresolved = |()| REASON_SLOT_UNRESOLVED;
    let shadow_ns = shadow_namespace(store).map_err(unresolved)?;
    let shadow: Vec<Vec<Ident>> = vec![
        account_idents(&shadow_ns, SHADOW_CURRENT).map_err(unresolved)?,
        account_idents(&shadow_ns, SHADOW_PENDING).map_err(unresolved)?,
        item_family_idents(&shadow_ns).map_err(unresolved)?,
    ];
    let mut live: Vec<Vec<Ident>> = Vec::new();
    for d in declared {
        let ns = declared_namespace(d).map_err(unresolved)?;
        for account in d.accounts {
            live.push(account_idents(&ns, account).map_err(unresolved)?);
        }
    }
    // Two declarations that alias one native slot would make a restore order-dependent; the
    // snapshot-store records and families must not share a slot with each other either.
    if !pairwise_disjoint(&live) || !pairwise_disjoint(&shadow) {
        return Err(REASON_SLOT_OVERLAP);
    }
    if shadow
        .iter()
        .any(|s| live.iter().any(|l| slots_overlap(s, l)))
    {
        return Err(REASON_SLOT_OVERLAP);
    }
    Ok(live.into_iter().flatten().collect())
}

/// A slot of the snapshot store: the only way this module names an account there.
enum ShadowSlot<'a> {
    Current,
    Pending,
    Item { generation: &'a str, index: usize },
}

impl ShadowSlot<'_> {
    /// The account, or `None` for an item whose generation is not a valid snapshot id or whose
    /// index is not below [`MAX_ITEMS`].
    fn account(&self) -> Option<String> {
        match self {
            Self::Current => Some(SHADOW_CURRENT.to_string()),
            Self::Pending => Some(SHADOW_PENDING.to_string()),
            Self::Item { generation, index }
                if valid_snapshot_id(generation) && *index < MAX_ITEMS =>
            {
                Some(slot_account(generation, *index))
            }
            Self::Item { .. } => None,
        }
    }
}

/// The snapshot-store account of one declared item's value in one generation.
fn slot_account(generation: &str, index: usize) -> String {
    format!("{ITEM_PREFIX}{generation}-{index}")
}

/// The snapshot store. EVERY get, set and delete of it goes through here; no other code holds the
/// `Store`. A write or delete first resolves the slot's native identities and refuses when they
/// overlap any declared live identity (defence in depth; `resolve_slots` already proved the
/// families disjoint). Errors carry nothing.
struct Shadow {
    store: Store,
    ns: Namespace,
    live: Vec<Ident>,
}

impl Shadow {
    fn get(&self, slot: &ShadowSlot) -> Result<Option<Secret>, ()> {
        let account = slot.account().ok_or(())?;
        self.store.get(&account).map_err(|_| ())
    }

    /// The account of `slot`, only when it cannot address any declared live slot.
    fn guarded(&self, slot: &ShadowSlot) -> Result<String, ()> {
        let account = slot.account().ok_or(())?;
        let own = account_idents(&self.ns, &account)?;
        if slots_overlap(&own, &self.live) {
            return Err(());
        }
        Ok(account)
    }

    fn set(&self, slot: &ShadowSlot, value: &Secret) -> Result<(), ()> {
        let account = self.guarded(slot)?;
        // `Store::set` updates a present item in place and keeps its access control. No slot is
        // written on macOS until its access control is proven (`mac_slot_acl`).
        #[cfg(target_os = "macos")]
        mac_slot_acl::before_write()?;
        self.store.set(&account, value).map_err(|_| ())
    }

    fn delete(&self, slot: &ShadowSlot) -> Result<(), ()> {
        let account = self.guarded(slot)?;
        self.store.delete(&account).map(|_| ()).map_err(|_| ())
    }

    /// Before an order's first write or delete: the concrete slots of every generation it can
    /// touch (`generations`), every item index below [`MAX_ITEMS`], and the two records resolve
    /// and are pairwise distinct under both rules, so no two generations or items share a slot.
    fn check_generations(&self, generations: &[String]) -> Result<(), Refusal> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut accounts: Vec<String> =
            vec![SHADOW_CURRENT.to_string(), SHADOW_PENDING.to_string()];
        let mut done: HashSet<&str> = HashSet::new();
        for generation in generations {
            if !valid_snapshot_id(generation) {
                return refuse(
                    REASON_SLOT_UNRESOLVED,
                    slot_refusal_text(REASON_SLOT_UNRESOLVED),
                );
            }
            if !done.insert(generation.as_str()) {
                continue;
            }
            for index in 0..MAX_ITEMS {
                accounts.push(slot_account(generation, index));
            }
        }
        for account in &accounts {
            for rule in RULES {
                let Ok(identity) = native_identity(rule, &self.ns, account) else {
                    return refuse(
                        REASON_SLOT_UNRESOLVED,
                        slot_refusal_text(REASON_SLOT_UNRESOLVED),
                    );
                };
                if !seen.insert(identity) {
                    return refuse(REASON_SLOT_OVERLAP, slot_refusal_text(REASON_SLOT_OVERLAP));
                }
            }
        }
        Ok(())
    }
}

/// The generations one order can touch: the request's, the current one and every one `pending`
/// lists. Reads only. A record that cannot be read adds nothing: the sweep reads the same records
/// and removes nothing when it cannot.
fn known_generations(shadow: &Shadow, request_generation: &str) -> Vec<String> {
    let mut generations = vec![request_generation.to_string()];
    if let Ok(CurrentRead::Valid(current)) = read_current(shadow) {
        generations.push(current.generation);
    }
    if let Ok(pending) = read_pending(shadow) {
        generations.extend(pending.into_iter().map(|(generation, _)| generation));
    }
    generations
}

// ---------------------------------------------------------------------------- store access

/// One non-interactive store per declared set: direct open/get/set/delete, no create, migrate
/// or cache helper. (`Config::new` is non-interactive and its guard denies; a keychain that
/// wants a prompt answers an error, never a dialog.) Each is opened on the namespace the identity
/// proof used.
fn open_stores(host: &OwnerHost) -> Result<Vec<Store>, Refusal> {
    let mut stores = Vec::with_capacity(host.declared.len());
    for d in host.declared.iter() {
        let Ok(ns) = declared_namespace(d) else {
            return refuse("failed", "a credential namespace is invalid");
        };
        stores.push(Store::open(Config::new(ns)));
    }
    Ok(stores)
}

/// The host-declared [`SnapshotStore`] service, opened on the namespace the identity proof used.
/// The only place the snapshot store is opened. It re-runs `snapshot_isolation` itself, so no
/// order path can reach the store without the gate, whatever `Owner::admit` did before it (on
/// Windows and every other platform this always refuses with `snapshot_store_unisolated`).
fn open_shadow(host: &OwnerHost) -> Result<Shadow, Refusal> {
    if let Err((code, text)) = snapshot_isolation(host) {
        return refuse(code, text);
    }
    let Ok(ns) = shadow_namespace(&host.snapshot_store) else {
        return refuse("failed", "the snapshot store namespace is invalid");
    };
    Ok(Shadow {
        store: Store::open(Config::new(ns.clone())),
        ns,
        live: host.live_slots.clone(),
    })
}

/// The `cur` record: which generation is current, the digest of its manifest file, and whether
/// it can still be restored (`done == false`) or has been completed (`done == true`, the receipt:
/// snapshot id and manifest digest, no value). A restore or a completion is accepted only for the
/// exact id and digest recorded here, so only a snapshot this app made, and not superseded since,
/// is acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Current {
    generation: String,
    manifest_sha256: String,
    done: bool,
}

impl Current {
    fn encode(&self) -> Secret {
        let state = if self.done { CUR_DONE } else { CUR_READY };
        Secret::from_bytes(
            format!(
                "{CURRENT_VERSION}\n{}\n{}\n{state}",
                self.generation, self.manifest_sha256
            )
            .into_bytes(),
        )
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_CONTROL_BYTES {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let mut parts = text.split('\n');
        let version = parts.next()?;
        let generation = parts.next()?;
        let manifest_sha256 = parts.next()?;
        let state = parts.next()?;
        if parts.next().is_some()
            || version != CURRENT_VERSION
            || !valid_snapshot_id(generation)
            || !valid_sha256(manifest_sha256)
        {
            return None;
        }
        let done = match state {
            CUR_READY => false,
            CUR_DONE => true,
            _ => return None,
        };
        Some(Self {
            generation: generation.to_string(),
            manifest_sha256: manifest_sha256.to_string(),
            done,
        })
    }
}

enum CurrentRead {
    Absent,
    Valid(Current),
    /// The record exists and is not a record this owner wrote.
    Corrupt,
}

/// A store read error is a refusal, never "absent".
fn read_current(shadow: &Shadow) -> Result<CurrentRead, Refusal> {
    match shadow.get(&ShadowSlot::Current) {
        Ok(None) => Ok(CurrentRead::Absent),
        Ok(Some(secret)) => Ok(match Current::parse(secret.expose_bytes()) {
            Some(current) => CurrentRead::Valid(current),
            None => CurrentRead::Corrupt,
        }),
        Err(_) => refuse("failed", "the snapshot record cannot be read"),
    }
}

/// Write `cur` and read it back. This single store write is the commit point of a snapshot and of a
/// completion: the store replaces an item's value in one call, so a crash leaves the old record or
/// the new one.
fn write_current(shadow: &Shadow, record: &Current) -> bool {
    let value = record.encode();
    shadow.set(&ShadowSlot::Current, &value).is_ok()
        && matches!(shadow.get(&ShadowSlot::Current), Ok(Some(now)) if now.ct_eq(&value))
}

/// `write_current`, or else the record already reads back as `record` (a write whose answer was
/// lost or whose read-back failed is not assumed to have failed).
fn commit_current(shadow: &Shadow, record: &Current) -> bool {
    write_current(shadow, record)
        || matches!(read_current(shadow), Ok(CurrentRead::Valid(now)) if now == *record)
}

/// The `current` record of this exact snapshot, or the refusal that says why there is none.
fn matching_current(shadow: &Shadow, req: &Request, want_sha: &str) -> Result<Current, Refusal> {
    match read_current(shadow)? {
        CurrentRead::Valid(current)
            if current.generation == req.snapshot_id && current.manifest_sha256 == want_sha =>
        {
            Ok(current)
        }
        CurrentRead::Valid(_) => refuse(
            "snapshot_mismatch",
            "the snapshot this app holds is not the one the request names",
        ),
        CurrentRead::Absent => refuse("no_snapshot", "this app holds no snapshot"),
        CurrentRead::Corrupt => refuse("state_unreadable", "the snapshot record is unreadable"),
    }
}

/// Generations whose item slots may hold values, each with the number of slots to try.
type Pending = Vec<(String, usize)>;

fn parse_pending(bytes: &[u8]) -> Option<Pending> {
    if bytes.len() > MAX_CONTROL_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut list: Pending = Vec::new();
    for line in text.split('\n') {
        let (generation, count_text) = line.split_once(':')?;
        let count: usize = count_text.parse().ok()?;
        if count.to_string() != count_text
            || count > MAX_ITEMS
            || !valid_snapshot_id(generation)
            || list.iter().any(|(known, _)| known == generation)
        {
            return None;
        }
        list.push((generation.to_string(), count));
    }
    (!list.is_empty() && list.len() <= MAX_PENDING_GENERATIONS).then_some(list)
}

/// A store read error, or a record this owner did not write, is a refusal: the owner would lose
/// track of values it holds. The documented recovery is removing the snapshot service's items.
fn read_pending(shadow: &Shadow) -> Result<Pending, Refusal> {
    match shadow.get(&ShadowSlot::Pending) {
        Ok(None) => Ok(Vec::new()),
        Ok(Some(secret)) => match parse_pending(secret.expose_bytes()) {
            Some(list) => Ok(list),
            None => refuse("state_unreadable", "the pending record is unreadable"),
        },
        Err(_) => refuse("failed", "the pending record cannot be read"),
    }
}

/// Write `pending` and read it back; an empty list removes the record.
fn write_pending(shadow: &Shadow, list: &[(String, usize)]) -> bool {
    if list.is_empty() {
        return shadow.delete(&ShadowSlot::Pending).is_ok();
    }
    let text = list
        .iter()
        .map(|(generation, count)| format!("{generation}:{count}"))
        .collect::<Vec<_>>()
        .join("\n");
    let value = Secret::from_bytes(text.into_bytes());
    shadow.set(&ShadowSlot::Pending, &value).is_ok()
        && matches!(shadow.get(&ShadowSlot::Pending), Ok(Some(now)) if now.ct_eq(&value))
}

/// What a cleanup left behind. Reported in every reply as fixed codes with counts.
#[derive(Default, Clone, Copy)]
struct Cleanup {
    /// Item deletions that failed in this sweep.
    items_not_removed: usize,
    /// Stale generations still listed after this sweep.
    generations_pending: usize,
    /// The records could not be read, so nothing was removed and the counts are not known.
    unknown: bool,
}

impl Cleanup {
    fn code(&self) -> &'static str {
        if self.unknown {
            "cleanup_unknown"
        } else if self.items_not_removed > 0 || self.generations_pending > 0 {
            "cleanup_incomplete"
        } else {
            "cleanup_ok"
        }
    }

    fn json(&self) -> Value {
        json!({
            "code": self.code(),
            "itemsNotRemoved": self.items_not_removed,
            "generationsPending": self.generations_pending,
        })
    }

    /// Fixed text and counts to append to a refusal; empty when the cleanup is complete.
    fn note(&self) -> String {
        match self.code() {
            "cleanup_ok" => String::new(),
            code => format!(
                "; {code} items={} generations={}",
                self.items_not_removed, self.generations_pending
            ),
        }
    }
}

/// Remove the item slots of every listed generation except the one that is current and
/// restorable, then rewrite `pending` without the generations whose slots are all gone. It decides
/// from the records as they are now (`cur` is read here, not passed in), so a failure or crash at
/// any point of an earlier order is repaired by the same rule. A generation that is current and
/// `ready` is never touched; a current generation that is `done` has its values removed and keeps
/// only the `cur` record as its receipt. Never fails the order it runs in.
fn sweep(shadow: &Shadow) -> Cleanup {
    let unknown = Cleanup {
        unknown: true,
        ..Cleanup::default()
    };
    let live = match read_current(shadow) {
        Ok(CurrentRead::Valid(current)) => Some(current),
        Ok(CurrentRead::Absent) | Ok(CurrentRead::Corrupt) => None,
        Err(_) => return unknown,
    };
    let Ok(pending) = read_pending(shadow) else {
        return unknown;
    };
    let listed = pending.len();
    let mut cleanup = Cleanup::default();
    let mut keep: Pending = Vec::with_capacity(listed);
    for (generation, count) in pending {
        if live
            .as_ref()
            .is_some_and(|current| !current.done && current.generation == generation)
        {
            keep.push((generation, count));
            continue;
        }
        let mut failed = 0usize;
        for index in 0..count {
            let slot = ShadowSlot::Item {
                generation: &generation,
                index,
            };
            if shadow.delete(&slot).is_err() {
                failed = failed.saturating_add(1);
            }
        }
        if failed > 0 {
            cleanup.items_not_removed = cleanup.items_not_removed.saturating_add(failed);
            cleanup.generations_pending = cleanup.generations_pending.saturating_add(1);
            keep.push((generation, count));
        }
    }
    if keep.len() != listed && !write_pending(shadow, &keep) {
        // The list still names generations whose slots are gone; the next sweep removes them.
        cleanup.generations_pending = cleanup
            .generations_pending
            .saturating_add(listed - keep.len());
    }
    cleanup
}

// ---------------------------------------------------------------------------- operations

fn metadata(
    operation: &str,
    req: &Request,
    count: usize,
    sha: &str,
    already_completed: bool,
    cleanup: &Cleanup,
) -> Value {
    json!({
        "schema": SCHEMA,
        "operation": operation,
        "snapshotId": req.snapshot_id,
        "itemCount": count,
        "credentialsSha256": sha,
        "verified": true,
        "alreadyCompleted": already_completed,
        "cleanup": cleanup.json(),
    })
}

/// Snapshot: keep each declared item's value in a NEW generation of the snapshot store, write the
/// value-free manifest, then switch `cur` to the new generation (the commit). The previous
/// snapshot, its values and its `cur` record are not touched before that switch; the superseded
/// generation is removed after it. A failure before the switch removes what this call wrote (the
/// sweep); a failure of that removal is reported with counts and retried by the next order.
fn op_snapshot(host: &OwnerHost, params: &Value) -> Result<Value, Refusal> {
    let req = parse_request(host, params, false)?;
    // The claim lives to the end of the order: the manifest, the snapshot store and `cur` are
    // written only by the one owner process that holds it.
    let (dir, _claim) = validate_and_claim(host, &req, STATE_SNAPSHOTTING, None, false)?;
    let path = dir.join(MANIFEST_FILE);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return refuse(
                "refused",
                "a credential manifest already exists in the snapshot",
            )
        }
    }
    // The snapshot store first: its isolation gate refuses before any other store is opened.
    let shadow = open_shadow(host)?;
    let stores = open_stores(host)?;
    let expected = expected_items(host);

    // Before the first write or delete of the order: every slot of every generation it can touch
    // is a distinct native slot.
    shadow.check_generations(&known_generations(&shadow, &req.snapshot_id))?;

    // Generations left by an earlier crash or failure go first. The usable one is not touched.
    let _ = sweep(&shadow);
    if let CurrentRead::Valid(current) = read_current(&shadow)? {
        if current.generation == req.snapshot_id {
            return refuse("refused", "a snapshot with this id is already held");
        }
    }
    let mut pending = read_pending(&shadow)?;
    pending.retain(|(generation, _)| *generation != req.snapshot_id);
    if pending.len() >= MAX_PENDING_GENERATIONS {
        return refuse(
            "cleanup_backlog",
            "stale snapshot generations cannot be removed; the snapshot was not started",
        );
    }
    // The new generation is listed BEFORE any of its values is written, so a crash at any later
    // point leaves a record the sweep can act on.
    pending.push((req.snapshot_id.clone(), expected.len()));
    if !write_pending(&shadow, &pending) {
        return refuse("failed", "the pending record cannot be written");
    }

    match commit_snapshot(&req, &path, &stores, &shadow, &expected, host) {
        Ok((count, sha)) => {
            let cleanup = sweep(&shadow);
            Ok(metadata("snapshot", &req, count, &sha, false, &cleanup))
        }
        Err(refusal) => {
            let _ = fs::remove_file(&path);
            let cleanup = sweep(&shadow);
            Err(Refusal {
                code: refusal.code,
                message: format!("{}{}", refusal.message, cleanup.note()),
            })
        }
    }
}

/// The writing half of a snapshot: items, manifest, then the commit of `cur`.
fn commit_snapshot(
    req: &Request,
    path: &Path,
    stores: &[Store],
    shadow: &Shadow,
    expected: &[(usize, &'static str)],
    host: &OwnerHost,
) -> Result<(usize, String), Refusal> {
    let items = snapshot_items(stores, shadow, &req.snapshot_id, expected)?;
    let count = items.len();
    let manifest = Manifest {
        schema: MANIFEST_SCHEMA.to_string(),
        app: host.app.to_string(),
        snapshot_id: req.snapshot_id.clone(),
        items,
    };
    let Ok(bytes) = serde_json::to_vec_pretty(&manifest) else {
        return refuse("failed", "the credential manifest cannot be encoded");
    };
    let sha = rightkit_fs::digest::sha256_bytes(&bytes);

    // Exclusive create, owner-only from creation.
    let written = match rightkit_fs::private::create_private_new(path) {
        Ok(mut file) => file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .is_ok(),
        Err(_) => false,
    };
    let proven = written
        && read_private_capped(path, MAX_MANIFEST_BYTES)
            .map(|back| rightkit_fs::digest::sha256_bytes(&back) == sha)
            .unwrap_or(false);
    if !proven {
        return refuse(
            "failed",
            "the credential manifest did not verify after writing",
        );
    }
    // The switch of `cur` is the commit point: from here this snapshot can be restored and the
    // previous one cannot.
    let record = Current {
        generation: req.snapshot_id.clone(),
        manifest_sha256: sha.clone(),
        done: false,
    };
    if !commit_current(shadow, &record) {
        return refuse("failed", "the snapshot record cannot be written");
    }
    Ok((count, sha))
}

/// Read every declared item and keep present values in the snapshot store under `generation`
/// (read back). A read failure is an error, never "absent".
fn snapshot_items(
    stores: &[Store],
    shadow: &Shadow,
    generation: &str,
    expected: &[(usize, &'static str)],
) -> Result<Vec<ManifestItem>, Refusal> {
    let mut items = Vec::with_capacity(expected.len());
    for (index, (set, account)) in expected.iter().enumerate() {
        // Reads the declared (live) item only; the writes and deletes below address `slot`, a
        // snapshot-store slot.
        let store = &stores[*set];
        let slot = ShadowSlot::Item { generation, index };
        match store.get(account) {
            Ok(Some(secret)) => {
                if secret.is_empty() {
                    return refuse(
                        "failed",
                        format!("credential set {set} account {account} is stored empty"),
                    );
                }
                if shadow.set(&slot, &secret).is_err() {
                    return refuse(
                        "failed",
                        format!("cannot keep credential set {set} account {account}"),
                    );
                }
                match shadow.get(&slot) {
                    Ok(Some(kept)) if kept.ct_eq(&secret) => {}
                    _ => {
                        return refuse(
                            "failed",
                            format!("credential set {set} account {account} was not kept intact"),
                        )
                    }
                }
                items.push(ManifestItem {
                    set: *set,
                    account: (*account).to_string(),
                    present: true,
                });
            }
            Ok(None) => {
                if shadow.delete(&slot).is_err() {
                    return refuse(
                        "failed",
                        format!("cannot clear the kept slot of set {set} account {account}"),
                    );
                }
                items.push(ManifestItem {
                    set: *set,
                    account: (*account).to_string(),
                    present: false,
                });
            }
            Err(_) => {
                return refuse(
                    "failed",
                    format!("cannot read credential set {set} account {account}"),
                )
            }
        }
    }
    Ok(items)
}

/// One validated restore step: set and get-verify a present value, delete and verify absence.
enum Step {
    Present(Secret),
    Absent,
}

type Plan = Vec<(usize, &'static str, Step)>;

/// Read the manifest the request's digest names and every kept value of the current generation,
/// and refuse unless they are exactly this app's snapshot. Nothing is mutated.
fn load_plan(
    host: &OwnerHost,
    dir: &Path,
    req: &Request,
    want_sha: &str,
    shadow: &Shadow,
    current: &Current,
) -> Result<Plan, Refusal> {
    let path = dir.join(MANIFEST_FILE);
    let Ok(bytes) = read_private_capped(&path, MAX_MANIFEST_BYTES) else {
        return refuse("refused", "the credential manifest cannot be read");
    };
    if rightkit_fs::digest::sha256_bytes(&bytes) != want_sha {
        return refuse(
            "refused",
            "the credential manifest does not match its digest",
        );
    }
    // Typed parse: every key is required, unknown and duplicate keys are refused.
    let file: Manifest = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(_) => return refuse("refused", "the credential manifest is malformed"),
    };
    if file.schema != MANIFEST_SCHEMA || file.app != host.app || file.snapshot_id != req.snapshot_id
    {
        return refuse(
            "refused",
            "the credential manifest is not this app's snapshot",
        );
    }
    let expected = expected_items(host);
    if file.items.len() != expected.len() {
        return refuse(
            "refused",
            "the credential manifest does not hold the declared items",
        );
    }
    let mut plan: Plan = Vec::with_capacity(expected.len());
    for (index, (item, (set, account))) in file.items.iter().zip(expected.iter()).enumerate() {
        if item.set != *set || item.account != *account {
            return refuse(
                "refused",
                "the credential manifest items are not the declared items",
            );
        }
        if item.present {
            let slot = ShadowSlot::Item {
                generation: &current.generation,
                index,
            };
            match shadow.get(&slot) {
                Ok(Some(secret)) if !secret.is_empty() => {
                    plan.push((*set, *account, Step::Present(secret)));
                }
                _ => {
                    return refuse(
                        "refused",
                        format!("the kept value of set {set} account {account} is missing"),
                    )
                }
            }
        } else {
            plan.push((*set, *account, Step::Absent));
        }
    }
    Ok(plan)
}

/// Restore of the credentials: only the snapshot this app holds (`cur` names this snapshot id and
/// this exact manifest) is restored, every value is loaded before the first mutation, and NOTHING
/// is consumed: the same restore can be ordered again, and converges, until the runner sends the
/// completion order after it has restored its file roots. For a snapshot that is already completed
/// it answers `alreadyCompleted` and writes nothing.
fn op_restore(host: &OwnerHost, params: &Value) -> Result<Value, Refusal> {
    let req = parse_request(host, params, true)?;
    let want_sha = req.credentials_sha256.clone().unwrap_or_default();
    let (dir, _claim) = validate_and_claim(host, &req, STATE_RESTORING, Some(&want_sha), true)?;
    let shadow = open_shadow(host)?;
    // Before the first write or delete of the order (the sweep included).
    shadow.check_generations(&known_generations(&shadow, &req.snapshot_id))?;
    let current = matching_current(&shadow, &req, &want_sha)?;
    if current.done {
        let cleanup = sweep(&shadow);
        let count = expected_items(host).len();
        return Ok(metadata("restore", &req, count, &want_sha, true, &cleanup));
    }
    let plan = load_plan(host, &dir, &req, &want_sha, &shadow, &current)?;
    let stores = open_stores(host)?;

    // Mutations. A failure stops at once with a redacted error; `cur` is unchanged, the values
    // are kept, and the restore can be run again.
    for (set, account, step) in &plan {
        let store = &stores[*set];
        match step {
            Step::Present(secret) => {
                if store.set(account, secret).is_err() {
                    return refuse(
                        "failed",
                        format!("cannot restore credential set {set} account {account}"),
                    );
                }
                match store.get(account) {
                    Ok(Some(now)) if now.ct_eq(secret) => {}
                    _ => {
                        return refuse(
                            "failed",
                            format!("credential set {set} account {account} does not hold the snapshot value after the restore"),
                        )
                    }
                }
            }
            Step::Absent => {
                if store.delete(account).is_err() {
                    return refuse(
                        "failed",
                        format!("cannot remove credential set {set} account {account}"),
                    );
                }
                match store.get(account) {
                    Ok(None) => {}
                    _ => {
                        return refuse(
                            "failed",
                            format!(
                            "credential set {set} account {account} still exists after the restore"
                        ),
                        )
                    }
                }
            }
        }
    }
    let cleanup = sweep(&shadow);
    Ok(metadata(
        "restore",
        &req,
        plan.len(),
        &want_sha,
        false,
        &cleanup,
    ))
}

/// Completion: the runner's order after it restored its file roots. Only when every declared item
/// holds the snapshot's value (or is absent, as snapshotted) is `cur` written as `done`, which
/// consumes the snapshot (a restore no longer writes anything) and leaves the receipt; the kept
/// values are removed after that. A repeated order for a completed snapshot answers
/// `alreadyCompleted`. If the declared items do not match, nothing is consumed and the refusal
/// `restore_incomplete` tells the runner to repeat the restore.
fn op_complete(host: &OwnerHost, params: &Value) -> Result<Value, Refusal> {
    let req = parse_request(host, params, true)?;
    let want_sha = req.credentials_sha256.clone().unwrap_or_default();
    let (dir, _claim) = validate_and_claim(host, &req, STATE_RESTORING, Some(&want_sha), true)?;
    let shadow = open_shadow(host)?;
    // Before the first write or delete of the order (the sweep included).
    shadow.check_generations(&known_generations(&shadow, &req.snapshot_id))?;
    let current = matching_current(&shadow, &req, &want_sha)?;
    if current.done {
        let cleanup = sweep(&shadow);
        let count = expected_items(host).len();
        return Ok(metadata("complete", &req, count, &want_sha, true, &cleanup));
    }
    let plan = load_plan(host, &dir, &req, &want_sha, &shadow, &current)?;
    let stores = open_stores(host)?;
    for (set, account, step) in &plan {
        let store = &stores[*set];
        let holds = match step {
            Step::Present(secret) => {
                matches!(store.get(account), Ok(Some(now)) if now.ct_eq(secret))
            }
            Step::Absent => matches!(store.get(account), Ok(None)),
        };
        if !holds {
            return refuse(
                "restore_incomplete",
                format!("credential set {set} account {account} does not hold the snapshot state; repeat the restore"),
            );
        }
    }
    let receipt = Current {
        done: true,
        ..current
    };
    if !commit_current(&shadow, &receipt) {
        return refuse(
            "failed",
            "the completion could not be recorded; the restore can be repeated",
        );
    }
    let cleanup = sweep(&shadow);
    Ok(metadata(
        "complete",
        &req,
        plan.len(),
        &want_sha,
        false,
        &cleanup,
    ))
}
