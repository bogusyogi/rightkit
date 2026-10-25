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

mod app_root;
/// Approval workflow mechanics: request binding, verified decisions, one execution claim,
/// audit before effect (PK07-4).
pub mod approval;
pub mod chunk;
mod client;
pub mod contract;
mod discovery;
mod error;
mod fail_limit;
mod host_identity;
pub mod lifecycle;
#[cfg(feature = "link")]
pub mod link;
/// Loopback-only HTTP host with a bearer token (`remote-loopback` feature, not `remote`).
#[cfg(feature = "remote-loopback")]
pub mod loopback;
pub mod mode;
mod peer;
pub mod private;
mod record;
mod registration;
#[cfg(feature = "relay")]
pub mod relay;
/// Daemon host (`remote` feature) and device client (`link` feature); see the module docs.
#[cfg(any(feature = "remote", feature = "link"))]
pub mod remote;
mod root_cutover;
mod server;
mod service;
pub mod stop;
mod suite;
pub mod supervisor;
mod takeover_watch;
pub mod ticker;
mod transport;

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
pub use app_root::{
    valid_app_name, AppRoot, AppRootError, LinkNotCreated, MigrateOptions, MigratedLink,
    MigrationOutcome, MigrationReport, MigrationScope, MigrationState, MigrationStatus,
    OmittedLink, ReparseKind, SkippedReparsePoint, SkippedSpecial, SpecialKind, WriterLock,
    MAX_APP_NAME_LEN, MIGRATION_MARKER, MIGRATION_STATE,
};
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
pub use lifecycle::{
    DaemonLifecycle, LifecycleError, Provenance, ERROR_NOT_OWNED, ERROR_RESTART_EXHAUSTED,
    ERROR_SESSION_RELEASED, ERROR_STALE_GENERATION,
};
#[cfg(unix)]
pub use peer::verify_peer_is_current_user;
#[cfg(windows)]
pub use peer::{
    current_user_sid, owner_only_security_attributes, verify_pipe_peer_is_current_user,
    verify_pipe_server_is_current_user, OwnerOnlySecurityAttributes,
};
pub use peer::{PeerError, PeerIdentity};
pub use record::{
    clear_takeover_if_owner, read_live_record, read_record, read_takeover, record_path,
    takeover_path, try_read_live_record, try_read_record, try_read_takeover, write_atomic,
    write_takeover, Allowlist, ServiceRecord, TakeoverRequest, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    RANK_UNKNOWN,
};
pub use registration::{
    MethodDescriptor, Registration, RegistrationError, ServiceIdentity, TransportProfile,
};
pub use root_cutover::{
    CutoverContext, CutoverError, CutoverParticipant, CutoverPhase, CutoverReport, CutoverStep,
    FsMove, ParticipantError, RecoverOutcome, RetireMode, Retired, RootCutover, RootSelection,
    RootStatus, Settlement, CUTOVER_RECORD,
};
pub use server::{
    host, host_with_options, CallContext, Emit, Handler, HostError, HostOptions, Hosted,
    ServiceConfig, ServiceEvent, ServiceStatus, DEFAULT_FRAME_LIMIT, DEFAULT_HANDSHAKE_TIMEOUT,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_RESERVED_SLOTS, DEFAULT_WRITE_TIMEOUT,
};
pub use service::{BuildError, Service, ServiceBuilder};
pub use suite::{
    refuse_build_tool_real_data, refuse_build_tool_real_data_for_mode, valid_service_name, Suite,
    SuiteError, ENV_SUITE_ROOT, MAX_SERVICE_NAME_LEN, SHARED_DIR_NAME, SUITE_DIR_NAME,
};
pub use transport::{
    endpoint_candidates, endpoint_for, find_live_endpoint, open_stream, open_stream_expecting,
    pid_alive, probe_endpoint, try_endpoint_for, Accepted, Conn, EndpointError, Listener, Stream,
    DEFAULT_MAX_PIPE_INSTANCES, MAX_UNIX_SOCKET_PATH,
};
