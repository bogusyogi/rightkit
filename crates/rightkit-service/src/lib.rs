//! rightkit-service: the one discovery and authentication convention for local
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
//!   client ([`verify_peer_is_current_user`]); a platform that cannot check refuses
//!   every peer. Windows: a named pipe whose DACL grants only the current user's SID
//!   ([`owner_only_security_attributes`]), remote clients rejected, first-instance
//!   flag set against name squatting, a bounded instance count
//!   ([`HostOptions::max_pipe_instances`]), the client's process-token SID checked on
//!   every accept, and clients opening with `SECURITY_IDENTIFICATION` and checking
//!   the server's SID. See the README threat model.
//! * **Per-app request allowlist** - each client names its app in `hello`; an
//!   app may call only the methods listed for it (the generalisation of
//!   HeardRight's `SERVICE_ALLOWED_SCHEMAS`). The app label is an allowlist key,
//!   not a security boundary against code already running as the same user.
//! * **Fail-closed roots** - [`Suite::resolve`] errors when no absolute
//!   app-data root can be determined; it never falls back to the working
//!   directory. [`Suite::per_run_temp`] is the explicit opt-in alternative.
//!
//! Wire format: bounded JSON lines (`rightkit-framed-sidecar`), the same frames
//! as the `rightkit-jobs` engine contract:
//! `hello`, `request{id,method,params}`, `event{id,event}`, `response{id,ok,result|error}`.

mod client;
mod discovery;
mod error;
mod peer;
mod record;
mod server;
mod suite;
mod takeover_watch;
mod transport;

pub use client::{Client, RetryPolicy, DEFAULT_DEADLINE};
pub use discovery::{
    adopt_or_spawn, discover_apps_in, discover_apps_in_with, discover_macos, discover_macos_with,
    discover_windows, discover_windows_with, parse_plist_string, parse_reg_query, plan_host,
    poll_until, publish_takeover, record_process_matches, resolve_install_dir, resolve_rank,
    should_request_takeover, spawn_detached, takeover_obliges_yield, wait_for_claim_winner,
    AdoptOrSpawn, Completeness, DiscoverOptions, HostPlan, InstalledApp, SpawnClaim,
    SpawnClaimError, UninstallEntry,
};
// Deprecated items stay exported for existing callers; their deprecation is reported at the
// caller's own use site, not on this re-export.
#[allow(deprecated)]
pub use discovery::{discover_apps, KNOWN_APPS};
pub use error::{
    ServiceError, ERROR_CONNECTION_CLOSED, ERROR_HOST_HANDOFF, ERROR_NOT_ALLOWED, ERROR_TIMEOUT,
    ERROR_UNAVAILABLE,
};
#[cfg(unix)]
pub use peer::verify_peer_is_current_user;
#[cfg(windows)]
pub use peer::{
    owner_only_security_attributes, verify_pipe_peer_is_current_user,
    verify_pipe_server_is_current_user, OwnerOnlySecurityAttributes,
};
pub use peer::{PeerError, PeerIdentity};
pub use record::{
    clear_takeover_if_owner, read_live_record, read_record, read_takeover, record_path,
    takeover_path, write_atomic, write_takeover, Allowlist, ServiceRecord, TakeoverRequest,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, RANK_UNKNOWN,
};
pub use server::{
    host, host_with_options, CallContext, Emit, Handler, HostError, HostOptions, Hosted,
    ServiceConfig, ServiceEvent, ServiceStatus, DEFAULT_FRAME_LIMIT,
};
pub use suite::{
    valid_service_name, Suite, SuiteError, ENV_SUITE_ROOT, MAX_SERVICE_NAME_LEN, SHARED_DIR_NAME,
    SUITE_DIR_NAME,
};
pub use transport::{
    endpoint_candidates, endpoint_for, find_live_endpoint, open_stream, pid_alive, probe_endpoint,
    try_endpoint_for, EndpointError, Stream, DEFAULT_MAX_PIPE_INSTANCES, MAX_UNIX_SOCKET_PATH,
};
