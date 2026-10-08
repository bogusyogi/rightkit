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
//!   releases. Clients re-read discovery and resubmit.
//! * **Authentication** - no TCP and no bearer tokens. macOS: a Unix socket in
//!   a 0700 directory, mode 0600, peer uid checked with `getpeereid`. Windows:
//!   a named pipe whose DACL grants only the current user's SID, remote clients
//!   rejected, first-instance flag set against name squatting.
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
mod record;
mod server;
mod suite;
mod transport;

pub use client::{Client, RetryPolicy, DEFAULT_DEADLINE};
pub use discovery::{
    adopt_or_spawn, discover_apps_in, discover_macos, discover_windows, parse_plist_string,
    parse_reg_query, plan_host, poll_until, publish_takeover, record_process_matches,
    resolve_install_dir, resolve_rank, should_request_takeover, spawn_detached,
    takeover_obliges_yield, wait_for_claim_winner, AdoptOrSpawn, HostPlan, InstalledApp,
    SpawnClaim, SpawnClaimError, UninstallEntry,
};
// Deprecated items stay exported for existing callers; their deprecation is reported at the
// caller's own use site, not on this re-export.
#[allow(deprecated)]
pub use discovery::{discover_apps, KNOWN_APPS};
pub use error::{
    ServiceError, ERROR_CONNECTION_CLOSED, ERROR_HOST_HANDOFF, ERROR_NOT_ALLOWED, ERROR_TIMEOUT,
    ERROR_UNAVAILABLE,
};
pub use record::{
    clear_takeover_if_owner, read_live_record, read_record, read_takeover, record_path,
    takeover_path, write_atomic, write_takeover, Allowlist, ServiceRecord, TakeoverRequest,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, RANK_UNKNOWN,
};
pub use server::{
    host, CallContext, Emit, Handler, HostError, Hosted, ServiceConfig, ServiceEvent,
    DEFAULT_FRAME_LIMIT,
};
pub use suite::{Suite, SuiteError, ENV_SUITE_ROOT, SHARED_DIR_NAME, SUITE_DIR_NAME};
pub use transport::{endpoint_for, open_stream, pid_alive, probe_endpoint, Stream};
