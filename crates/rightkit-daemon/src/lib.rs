#![allow(clippy::result_large_err)] // errors carry structured diagnostic context by design, as in rightkit-agent-orchestration
//! rightkit-daemon: the one discovery and authentication convention for local
//! Right Suite services (the shared speech service, the in-app control server,
//! `right-ctl`).
//!
//! * **Discovery** - `<suite>/runtime/<service>/service.json`, written
//!   atomically after the endpoint is listening, removed on clean exit. A
//!   record whose endpoint does not answer is stale and ignored.
//! * **Ranking and takeover** - one host per service per suite. A would-be host
//!   that outranks the incumbent (lower rank wins) writes `takeover.json`; the
//!   incumbent stops admitting work, finishes or hands off in-flight requests
//!   with the retryable [`ERROR_HOST_HANDOFF`] at their next boundary, and
//!   releases. Clients re-read discovery and resubmit. The incumbent looks for a
//!   request from another process once a second (a stat of `takeover.json`, no
//!   thread polls faster), so a takeover starts up to about a second after the
//!   request is written.
//! * **Authentication** - no TCP and no bearer tokens. Unix: a socket in a 0700
//!   directory, mode 0600, and the kernel-reported peer uid (`getpeereid` on macOS
//!   and the BSDs, `SO_PEERCRED` on Linux) checked on every accept and by every
//!   client (`verify_peer_is_current_user`); a platform that cannot check refuses
//!   every peer. Windows: a named pipe whose DACL grants only the current user's SID
//!   (`owner_only_security_attributes` on Windows), remote clients rejected, first-instance
//!   flag set against name squatting, a bounded instance count
//!   ([`HostOptions::max_pipe_instances`]), the client's process-token SID checked on
//!   every accept, and clients opening with `SECURITY_IDENTIFICATION` and checking
//!   the server's SID. See the README threat model.
//! * **Verifying the host** - the transport proves a host runs as the current user, not which
//!   program it is. [`Client::connect_expecting`] takes a [`HostExpectation`] (the executable the
//!   host may be, for example a sibling inside the client's own app bundle), asks the kernel who
//!   is on the listening end of the connected socket (macOS `LOCAL_PEERPID` and `proc_pidpath`,
//!   Linux `SO_PEERCRED` and `/proc/<pid>/exe`), and refuses anyone else with a typed
//!   [`HostIdentityError`] before the `hello` or any request is written. [`Client::connect`] stays
//!   unauthenticated-host. [`ClientConfig::ignore_env_overrides`] makes the suite root come only
//!   from the account database, so a launcher's environment cannot redirect a release client.
//!   See the README, "Verifying the host".
//! * **Per-app request allowlist** - each client names its app in `hello`; an
//!   app may call only the methods listed for it (the generalisation of
//!   HeardRight's `SERVICE_ALLOWED_SCHEMAS`). The app label is an allowlist key,
//!   not a security boundary against code already running as the same user.
//! * **Fail-closed roots** - [`Suite::resolve`] errors when no absolute
//!   app-data root can be determined; it never falls back to the working
//!   directory. [`Suite::per_run_temp`] is the explicit opt-in alternative.
//!
//! Wire format: bounded JSON lines (`rightkit-sidecar`), the same frames
//! as the `rightkit-job-queue` engine contract:
//! `hello`, `request{id,method,params}`, `event{id,event}`, `response{id,ok,result|error}`.

mod attach;
pub mod chunk;
mod client;
pub mod contract;
mod discovery;
mod error;
pub mod event_log;
mod fail_limit;
mod host_identity;
mod incumbent;
pub mod install;
pub mod lifecycle;
/// Loopback-only HTTP host with a bearer token (`remote-loopback` feature). The remote HTTPS host
/// is the `rightkit-daemon-remote` crate.
#[cfg(feature = "remote-loopback")]
pub mod loopback;
pub mod metrics;
mod peer;
mod peer_signature;
pub mod private;
mod record;
mod registration;
pub mod reload;
mod server;
mod service;
mod shutdown;
pub mod stop;
pub mod supervisor;
mod takeover_watch;
pub mod ticker;
mod transport;
pub mod wake;

pub use attach::{attach_or_fallback, Attached};
pub use client::{Client, ClientConfig, ClientPool, RetryPolicy, DEFAULT_DEADLINE};
pub use discovery::{
    adopt_or_spawn, discover_apps_in, discover_apps_in_with, discover_macos, discover_macos_with,
    discover_windows, discover_windows_with, parse_plist_string, parse_reg_query, plan_host,
    poll_until, publish_takeover, record_process_matches, resolve_install_dir, resolve_rank,
    should_request_takeover, spawn_detached, takeover_obliges_yield, try_adopt_or_spawn,
    try_plan_host, wait_for_claim_winner, AdoptOrSpawn, Completeness, DiscoverOptions, HostPlan,
    InstalledApp, SpawnClaim, SpawnClaimError, UninstallEntry,
};
pub use private::{
    exit_when_stdin_closes, watch_reader_eof, watch_stdin_eof, PrivateChild, PrivateSecret,
    SecretError, SECRET_BYTES, SECRET_HEX_LEN,
};
// Deprecated items stay exported for existing callers; their deprecation is reported at the
// caller's own use site, not on this re-export.
#[allow(deprecated)]
pub use discovery::{discover_apps, KNOWN_APPS};
pub use error::{
    ConnectError, ServiceError, ERROR_BAD_CHUNK, ERROR_CONNECTION_CLOSED, ERROR_FORBIDDEN,
    ERROR_HOST_HANDOFF, ERROR_HOST_IDENTITY, ERROR_NOT_ALLOWED, ERROR_RATE_LIMITED,
    ERROR_REQUEST_TOO_LARGE, ERROR_RESPONSE_TOO_LARGE, ERROR_REVISION_CONFLICT,
    ERROR_SUITE_UNRESOLVED, ERROR_TIMEOUT, ERROR_UNAUTHENTICATED, ERROR_UNAVAILABLE,
};
pub use fail_limit::FailLimiter;
pub use host_identity::{HostExpectation, HostIdentityError, HostIdentityReason, HostProcess};
pub use incumbent::{stop_incumbent, IncumbentError, IncumbentStop};
pub use lifecycle::{
    DaemonLifecycle, LifecycleError, Provenance, ERROR_NOT_OWNED, ERROR_RESTART_EXHAUSTED,
    ERROR_SESSION_RELEASED, ERROR_STALE_GENERATION,
};
#[cfg(unix)]
pub use peer::verify_peer_is_current_user;
#[cfg(windows)]
pub use peer::{
    current_user_sid, owner_only_security_attributes, owner_only_security_attributes_with,
    pipe_security_attributes, verify_pipe_peer_is_current_user, verify_pipe_peer_is_sid,
    verify_pipe_server_is_current_user, OwnerOnlySecurityAttributes,
};
pub use peer::{
    AclEntry, IntegrityLabel, PeerError, PeerFailureKind, PeerIdentity, PeerRefusal,
    ACCESS_GENERIC_ALL, ACCESS_PIPE_OWNER_RW,
};
pub use peer_signature::{
    verify_authenticode, verify_peer, AuthenticodeSigner, PeerPolicy, SignatureError,
    SigningIdentity, LICENSE_SNAPSHOT_METHOD,
};
pub use record::{
    clear_takeover_if_owner, read_live_record, read_record, read_takeover, record_path,
    takeover_path, try_read_live_record, try_read_record, try_read_takeover, write_atomic,
    write_takeover, Allowlist, ServiceRecord, TakeoverRequest, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    RANK_UNKNOWN,
};
pub use registration::{
    MethodDescriptor, Registration, RegistrationError, ServiceIdentity, TransportProfile,
};
pub use server::{
    host, host_with_options, valid_service_name, CallContext, Emit, Handler, HostError,
    HostOptions, Hosted, ServiceConfig, ServiceEvent, ServiceStatus, DEFAULT_FRAME_LIMIT,
    DEFAULT_HANDSHAKE_TIMEOUT, DEFAULT_MAX_CONNECTIONS, DEFAULT_RESERVED_SLOTS,
    DEFAULT_WRITE_TIMEOUT, MAX_SERVICE_NAME_LEN,
};
pub use service::{BuildError, Service, ServiceBuilder};
pub use shutdown::{OrderedShutdown, PhaseOutcome, PhaseReport, ShutdownReport};
pub use transport::{
    endpoint_candidates, endpoint_for, find_live_endpoint, open_stream, open_stream_expecting,
    pid_alive, probe_endpoint, try_endpoint_for, Accepted, Conn, EndpointError, Listener, Stream,
    DEFAULT_MAX_PIPE_INSTANCES, MAX_UNIX_SOCKET_PATH,
};

// Run mode, the suite root, app roots and root cutover moved to `rightkit-roots` 0.1.0
// (rightkit-daemon 0.8.0). `Suite` and `SuiteError` stay exported here for good: the daemon's own
// API takes them. Every other moved item stays exported under its old path for one release and is
// deprecated, naming the new path. (`#[deprecated]` on a `pub use` is ignored by rustc, so each is
// a deprecated alias, constant or function of the same name; they are the same types.)
/// Moved to `rightkit_roots::CutoverParticipant`. A trait cannot be aliased or deprecated through a
/// re-export; implement it from `rightkit_roots`.
pub use rightkit_roots::CutoverParticipant;
pub use rightkit_roots::{Suite, SuiteError};

#[deprecated(since = "0.8.0", note = "use `rightkit_roots::mode`")]
pub mod mode {
    pub use rightkit_roots::mode::*;
}

#[deprecated(since = "0.8.0", note = "use `rightkit_roots::valid_app_name`")]
pub fn valid_app_name(s: &str) -> bool {
    rightkit_roots::valid_app_name(s)
}
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::AppRoot`")]
pub type AppRoot = rightkit_roots::AppRoot;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::AppRootError`")]
pub type AppRootError = rightkit_roots::AppRootError;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::LinkNotCreated`")]
pub type LinkNotCreated = rightkit_roots::LinkNotCreated;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigrateOptions`")]
pub type MigrateOptions = rightkit_roots::MigrateOptions;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigratedLink`")]
pub type MigratedLink = rightkit_roots::MigratedLink;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigrationOutcome`")]
pub type MigrationOutcome = rightkit_roots::MigrationOutcome;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigrationReport`")]
pub type MigrationReport = rightkit_roots::MigrationReport;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigrationScope`")]
pub type MigrationScope = rightkit_roots::MigrationScope;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigrationState`")]
pub type MigrationState = rightkit_roots::MigrationState;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MigrationStatus`")]
pub type MigrationStatus = rightkit_roots::MigrationStatus;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::OmittedLink`")]
pub type OmittedLink = rightkit_roots::OmittedLink;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::ReparseKind`")]
pub type ReparseKind = rightkit_roots::ReparseKind;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::SkippedReparsePoint`")]
pub type SkippedReparsePoint = rightkit_roots::SkippedReparsePoint;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::SkippedSpecial`")]
pub type SkippedSpecial = rightkit_roots::SkippedSpecial;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::SpecialKind`")]
pub type SpecialKind = rightkit_roots::SpecialKind;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::WriterLock`")]
pub type WriterLock = rightkit_roots::WriterLock;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::CutoverContext`")]
pub type CutoverContext = rightkit_roots::CutoverContext;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::CutoverError`")]
pub type CutoverError = rightkit_roots::CutoverError;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::CutoverPhase`")]
pub type CutoverPhase = rightkit_roots::CutoverPhase;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::CutoverReport`")]
pub type CutoverReport = rightkit_roots::CutoverReport;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::CutoverStep`")]
pub type CutoverStep = rightkit_roots::CutoverStep;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::FsMove`")]
pub type FsMove = rightkit_roots::FsMove;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::ParticipantError`")]
pub type ParticipantError = rightkit_roots::ParticipantError;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::RecoverOutcome`")]
pub type RecoverOutcome = rightkit_roots::RecoverOutcome;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::RetireMode`")]
pub type RetireMode = rightkit_roots::RetireMode;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::Retired`")]
pub type Retired = rightkit_roots::Retired;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::RootCutover`")]
pub type RootCutover = rightkit_roots::RootCutover;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::RootSelection`")]
pub type RootSelection = rightkit_roots::RootSelection;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::RootStatus`")]
pub type RootStatus = rightkit_roots::RootStatus;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::Settlement`")]
pub type Settlement = rightkit_roots::Settlement;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MAX_APP_NAME_LEN`")]
pub const MAX_APP_NAME_LEN: usize = rightkit_roots::MAX_APP_NAME_LEN;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MIGRATION_MARKER`")]
pub const MIGRATION_MARKER: &str = rightkit_roots::MIGRATION_MARKER;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::MIGRATION_STATE`")]
pub const MIGRATION_STATE: &str = rightkit_roots::MIGRATION_STATE;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::ENV_SUITE_ROOT`")]
pub const ENV_SUITE_ROOT: &str = rightkit_roots::ENV_SUITE_ROOT;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::SHARED_DIR_NAME`")]
pub const SHARED_DIR_NAME: &str = rightkit_roots::SHARED_DIR_NAME;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::SUITE_DIR_NAME`")]
pub const SUITE_DIR_NAME: &str = rightkit_roots::SUITE_DIR_NAME;
#[deprecated(since = "0.8.0", note = "use `rightkit_roots::CUTOVER_RECORD`")]
pub const CUTOVER_RECORD: &str = rightkit_roots::CUTOVER_RECORD;
