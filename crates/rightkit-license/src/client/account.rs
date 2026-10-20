//! The native account-session client (feature `client`).
//!
//! The passwordless account session behind "sign in to manage devices across licences": request
//! an e-mailed sign-in link, complete it, keep one session token, list, rename and remove the
//! account's devices, sign out, and delete the account. The service paths and payloads are the
//! ones HeardRight's `account_session.rs` uses today, kept as compatibility vectors in
//! `tests/client_account.rs`; nothing here invents an endpoint:
//!
//! | operation | request |
//! | --- | --- |
//! | [`request_sign_in`](AccountSessionClient::request_sign_in) | `POST /v1/shop/auth/magic-link/request` `{email}` |
//! | [`complete_sign_in`](AccountSessionClient::complete_sign_in) | `POST /v1/shop/auth/magic-link/consume` `{token}` |
//! | [`refresh`](AccountSessionClient::refresh) | `GET /v1/shop/auth/me` |
//! | [`sign_out`](AccountSessionClient::sign_out) | `POST /v1/shop/auth/logout` `{}` |
//! | [`list_devices`](AccountSessionClient::list_devices) | `GET /v1/shop/account/devices` |
//! | [`rename_device`](AccountSessionClient::rename_device) | `PATCH /v1/shop/account/devices/{id}` `{label}` |
//! | [`remove_device`](AccountSessionClient::remove_device) | `DELETE /v1/shop/account/devices/{id}` |
//! | [`delete_account`](AccountSessionClient::delete_account) | `DELETE /v1/shop/account` |
//!
//! The link-code pair ([`request_link`](AccountSessionClient::request_link),
//! [`redeem_link`](AccountSessionClient::redeem_link)) is a licence operation, because it proves
//! ownership with, and stores, this device's lease; it runs on the [`LicenseClient`] the account
//! client was built with and uses the same one lifecycle, lock and record as everything else.
//!
//! ## What stays native
//! The session token is stored in one credential item ([`account_store`](super::account_store)),
//! sent only as an `Authorization` header registered as a secret, and never returned, emitted,
//! logged or serialised. [`AccountStatusView`] and [`AccountDeviceView`] are fixed allowlists
//! (the e-mail is display data the person typed; a device's licence key is reduced to its last
//! four characters); no token-bearing type is `Serialize`. The server's own text is never
//! forwarded; every refusal is a typed [`AccountError`] or [`AccountDeletion`].
//!
//! ## Concurrency
//! Local state is read and written under one per-app lock (`<license dir>/account/account.lock`,
//! polled to a deadline like the licence locks) and every write runs on a worker that keeps the
//! lock until the credential store call has really finished. The lock is NOT held across a server
//! call. An operation reads the item (a revision and what it holds), calls the server, and writes
//! its result only if a second read is still the same item; otherwise the result is discarded
//! ([`AccountError::Superseded`]) and, for a freshly issued session, logged out at the server. So
//! a refresh or a sign-in that finishes after a sign-out can never bring the session back, and a
//! sign-out that begins during one wins. Sign-out is local-first: the tombstone is written before
//! the courtesy server call and no server call is made if the tombstone cannot be written.
//!
//! ## What it does not do
//! No background task, no polling and no automatic refresh: the host calls `refresh`. Account
//! status is account display, not an entitlement: nothing here gates a feature, and signing out
//! never touches the licence record (removing this device's seat is a licence operation).

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_credentials::{Error as CredError, Store};
use rightkit_http::{Client, ClientConfig as HttpConfig, HttpError, Method, Request};
use serde::Serialize;
use serde_json::{json, Map, Value};
use zeroize::{Zeroize, Zeroizing};

use super::account_store::{encode_active, encode_tombstone, Item, Session, State};
use super::bridge::LinkRequestOutcome;
use super::lock::{acquire_polled, run_fenced, Fenced, OpGuard};
use super::ops::{plausible_email, suffix};
use super::persist::settle;
use super::time::{parse_rfc3339, to_ms};
use super::types::{LicenseClientConfig, SecretString};
use super::view::{LicenseError, LicenseView};
use super::LicenseClient;
use crate::MAX_TOKEN_BYTES;

/// Default credential account of the session item in the app's credential namespace.
pub const DEFAULT_SESSION_ACCOUNT: &str = "account-session";

/// The session lifetime the service uses (one year), for the display estimate in
/// [`AccountStatusView::expires_at`] only. The service issues no expiry claim; a dead session is
/// found, and cleared, when an authenticated call answers 401.
pub const ACCOUNT_SESSION_TTL_SECS: i64 = 365 * 24 * 60 * 60;

const MAGIC_LINK_REQUEST: &str = "/v1/shop/auth/magic-link/request";
const MAGIC_LINK_CONSUME: &str = "/v1/shop/auth/magic-link/consume";
const AUTH_ME: &str = "/v1/shop/auth/me";
const AUTH_LOGOUT: &str = "/v1/shop/auth/logout";
const ACCOUNT_DEVICES: &str = "/v1/shop/account/devices";
const ACCOUNT_DELETE: &str = "/v1/shop/account";

/// A device row id is a server-generated identifier placed in a URL path.
const MAX_DEVICE_ID_BYTES: usize = 128;
/// A device label is short text.
const MAX_LABEL_BYTES: usize = 256;
/// Display fields taken from a device row are bounded to this many characters.
const MAX_DISPLAY_CHARS: usize = 128;

// ---- configuration ----------------------------------------------------------------------

/// Static inputs of a configured account client. The origin, the wire profile (the paths above) and
/// the HTTP policy come from the licence configuration the client is built beside, so the account
/// and licence halves of an app can never point at different services.
#[derive(Clone)]
pub struct AccountClientConfig {
    origin: String,
    http: HttpConfig,
    store: Store,
    account: String,
    dir: PathBuf,
    mutation_wait: Duration,
    status_wait: Duration,
    persist_wait: Duration,
}

impl fmt::Debug for AccountClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountClientConfig")
            .field("origin", &self.origin)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl AccountClientConfig {
    /// Bind to the origin, HTTP policy, waits and licence directory of `license`. `store` is the
    /// credential store of the session item (its own namespace, for example HeardRight's existing
    /// `app.heardright.next.account-session`, with [`with_account`](Self::with_account) set to its
    /// existing user name `session` to read an older session in place). The account lock lives in
    /// `<license dir>/account`, and the store's rotation lock directory is set to the licence's, so
    /// the fence that tells a reader a timed-out write is still running works for the session item
    /// as it does for the licence.
    pub fn new(license: &LicenseClientConfig, store: Store) -> Self {
        Self {
            origin: license.origin(),
            http: license.http.clone(),
            store: store.with_rotation_lock_dir(license.license_dir.join("rotation")),
            account: DEFAULT_SESSION_ACCOUNT.to_string(),
            dir: license.license_dir.join("account"),
            mutation_wait: license.mutation_wait,
            status_wait: license.status_wait,
            persist_wait: license.persist_wait,
        }
    }

    /// The credential account of the session item (default [`DEFAULT_SESSION_ACCOUNT`]).
    pub fn with_account(mut self, account: impl Into<String>) -> Self {
        self.account = account.into();
        self
    }

    /// The whole-request deadline of one server call.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.http.request_timeout = timeout;
        self.http.response_timeout = Some(timeout);
        self
    }

    /// How long a mutation waits for the account lock, and a status read waits for it.
    pub fn with_waits(mut self, mutation: Duration, status: Duration) -> Self {
        self.mutation_wait = mutation;
        self.status_wait = status;
        self
    }

    /// How long a write waits for its fenced worker before it reports itself unresolved. The
    /// worker keeps the lock until the credential store call has returned, whatever this wait.
    pub fn with_persist_wait(mut self, wait: Duration) -> Self {
        self.persist_wait = wait;
        self
    }
}

// ---- views ------------------------------------------------------------------------------

/// The account session as a renderer may see it. No token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatusView {
    pub signed_in: bool,
    /// The e-mail the session was issued to, for display.
    pub email: Option<String>,
    /// A display estimate (issue or last confirmation plus one year), Unix ms. Never authority.
    pub expires_at: Option<i64>,
    /// Display only: the lock was held, so this is the last view this process saw.
    pub busy: bool,
    /// The last session write did not finish or the item cannot be read; the signed-in state is
    /// not established.
    pub unresolved: bool,
}

impl AccountStatusView {
    fn signed_out() -> Self {
        Self {
            signed_in: false,
            email: None,
            expires_at: None,
            busy: false,
            unresolved: false,
        }
    }

    fn unresolved() -> Self {
        Self {
            unresolved: true,
            ..Self::signed_out()
        }
    }

    fn of(session: &Session) -> Self {
        Self {
            signed_in: true,
            email: session.email.clone(),
            expires_at: Some(to_ms(
                session.issued_at.saturating_add(ACCOUNT_SESSION_TTL_SECS),
            )),
            busy: false,
            unresolved: false,
        }
    }

    fn of_item(item: &Item) -> Self {
        item.session().map_or_else(Self::signed_out, Self::of)
    }
}

/// One device on the account, from `GET /v1/shop/account/devices`. `id` is the server's row id: the
/// value to pass back to rename or remove (it is not a hash and not this device's identity). The
/// licence key the service lists with each row is reduced to its last four characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDeviceView {
    pub id: String,
    pub license_suffix: Option<String>,
    pub app_key: String,
    pub platform: Option<String>,
    pub pool: Option<String>,
    pub label: Option<String>,
    pub state: String,
    /// Unix ms; `None` when the service sent none or none that parses.
    pub last_seen_at: Option<i64>,
    /// Unix ms; `None` when the service sent none or none that parses.
    pub activated_at: Option<i64>,
    pub generation: u64,
}

// ---- errors -----------------------------------------------------------------------------

/// Why the account service declined, mapped natively from the HTTP status. Its message is never
/// forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountRejectReason {
    /// HTTP 429.
    RateLimited,
    /// The sign-in link or code is empty, malformed, or the service says (409) it is invalid,
    /// expired or already used.
    LinkInvalid,
    /// HTTP 404 for a device row.
    DeviceNotFound,
    /// The input was refused before anything was sent.
    InvalidInput,
    Other,
}

/// Why an account operation did not complete. `Display` is fixed text: no server body, no echoed
/// input. Nothing here carries a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountError {
    /// A build with no account service (see [`AccountSessionClient::disabled`]). No network.
    Unconfigured,
    /// The account lock is held; try again.
    Busy,
    /// An earlier session write did not finish; try again.
    Unresolved,
    /// The service could not be reached, or answered 5xx.
    Network,
    /// No session is stored.
    NotSignedIn,
    /// The service answered 401: the session is dead and the local record was cleared.
    SessionExpired,
    /// The session changed (a sign-out or a new sign-in) while this operation's server call was
    /// running; its result was discarded.
    Superseded,
    /// The service declined, or the input was refused before it was sent.
    Rejected { reason: AccountRejectReason },
    /// The session could not be read or saved on this device.
    Store,
    /// The service answered 2xx with something that is not a usable answer.
    BadResponse,
}

/// `AccountError` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountErrorCode {
    Unconfigured,
    Busy,
    Unresolved,
    Network,
    NotSignedIn,
    SessionExpired,
    Superseded,
    Rejected,
    Store,
    BadResponse,
}

/// The serialized object an `invoke` rejects with (never a string).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCommandError {
    pub code: AccountErrorCode,
    /// Only when `code` is `rejected`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<AccountRejectReason>,
}

impl AccountError {
    /// The wire code.
    pub fn code(&self) -> AccountErrorCode {
        match self {
            Self::Unconfigured => AccountErrorCode::Unconfigured,
            Self::Busy => AccountErrorCode::Busy,
            Self::Unresolved => AccountErrorCode::Unresolved,
            Self::Network => AccountErrorCode::Network,
            Self::NotSignedIn => AccountErrorCode::NotSignedIn,
            Self::SessionExpired => AccountErrorCode::SessionExpired,
            Self::Superseded => AccountErrorCode::Superseded,
            Self::Rejected { .. } => AccountErrorCode::Rejected,
            Self::Store => AccountErrorCode::Store,
            Self::BadResponse => AccountErrorCode::BadResponse,
        }
    }

    /// The reason, only for a rejection.
    pub fn reason(&self) -> Option<AccountRejectReason> {
        match self {
            Self::Rejected { reason } => Some(*reason),
            _ => None,
        }
    }

    /// The object a command rejects with.
    pub fn to_command_error(&self) -> AccountCommandError {
        AccountCommandError {
            code: self.code(),
            reason: self.reason(),
        }
    }

    fn rejected(reason: AccountRejectReason) -> Self {
        Self::Rejected { reason }
    }
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unconfigured => "Accounts are not available in this build.",
            Self::Busy => "Another account operation is in progress. Try again in a moment.",
            Self::Unresolved => "The last account change did not finish. Try again.",
            Self::Network => {
                "Could not reach the account service. Check your connection and try again."
            }
            Self::NotSignedIn => "Sign in to manage devices on your account.",
            Self::SessionExpired => "Your session expired. Sign in again.",
            Self::Superseded => "The account changed while this request was running. Try again.",
            Self::Rejected { reason } => match reason {
                AccountRejectReason::RateLimited => "Too many attempts. Try again shortly.",
                AccountRejectReason::LinkInvalid => {
                    "This sign-in link is invalid, expired, or already used."
                }
                AccountRejectReason::DeviceNotFound => "That device is no longer on your account.",
                AccountRejectReason::InvalidInput => "That input is not valid.",
                AccountRejectReason::Other => "The account service declined the request.",
            },
            Self::Store => "The account session could not be saved on this device.",
            Self::BadResponse => "The account service sent an unexpected answer.",
        })
    }
}

impl std::error::Error for AccountError {}

/// Proof that the product's own confirmation step ran. Account deletion is destructive and
/// irreversible; build this only after the person confirmed it. It has no `Default`, so a call
/// cannot omit it by accident.
#[derive(Debug, Clone, Copy)]
pub struct DeleteAccountConfirmation(());

impl DeleteAccountConfirmation {
    /// The person confirmed deleting the account in the product's own flow.
    pub fn confirmed_by_user() -> Self {
        Self(())
    }
}

/// Why the service refused to delete the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeletionRefusal {
    /// HTTP 409: the account has a non-cancelled subscription. The caller shows its own copy and
    /// the person cancels first; nothing was deleted and the session is kept.
    ActiveSubscription,
    /// HTTP 429.
    RateLimited,
    /// Any other 4xx. Nothing was deleted.
    Other,
}

/// How an account deletion ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum AccountDeletion {
    /// HTTP 200: the account is deleted. `local_session_cleared` says whether the local session
    /// was tombstoned too; when it is `false` the session item is a dead token that the next
    /// authenticated call (401) clears.
    #[serde(rename_all = "camelCase")]
    Deleted { local_session_cleared: bool },
    /// The service refused, in a way that proves nothing was deleted. The session is kept.
    #[serde(rename_all = "camelCase")]
    Refused { reason: DeletionRefusal },
    /// The request may have reached the service and no usable answer came back (a timeout, a
    /// dropped connection, a 5xx): the account may or may not be deleted. The session is kept;
    /// [`AccountSessionClient::refresh`] answers `SessionExpired` once the account is gone.
    Indeterminate,
}

// ---- transport --------------------------------------------------------------------------

/// A response: the status and a body that can hold a session token, zeroed on drop.
struct Reply {
    status: u16,
    body: Zeroizing<Vec<u8>>,
}

/// Why no answer came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendFailure {
    /// Nothing can have reached the service.
    NotSent,
    /// The request may have reached the service.
    Uncertain,
}

fn classify(error: &HttpError) -> SendFailure {
    match error {
        HttpError::Transport {
            connect_phase: true,
            ..
        }
        | HttpError::InvalidRequest(_)
        | HttpError::AddressDenied { .. }
        | HttpError::HttpsRequired { .. } => SendFailure::NotSent,
        _ => SendFailure::Uncertain,
    }
}

/// An authenticated call's failure: a typed account error, or no answer.
enum Failure {
    Account(AccountError),
    Send(SendFailure),
}

impl From<AccountError> for Failure {
    fn from(error: AccountError) -> Self {
        Self::Account(error)
    }
}

impl From<SendFailure> for Failure {
    fn from(failure: SendFailure) -> Self {
        Self::Send(failure)
    }
}

impl Failure {
    fn into_account(self) -> AccountError {
        match self {
            Self::Account(error) => error,
            Self::Send(_) => AccountError::Network,
        }
    }
}

// ---- the client -------------------------------------------------------------------------

/// The native account-session client. Cheap to clone (a handle).
#[derive(Clone)]
pub struct AccountSessionClient {
    core: Option<Arc<AccountCore>>,
    license: LicenseClient,
}

impl fmt::Debug for AccountSessionClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSessionClient")
            .field("configured", &self.core.is_some())
            .finish_non_exhaustive()
    }
}

impl AccountSessionClient {
    /// Build the client. `license` is the app's licence client, which the link-code operations
    /// run on. Builds the HTTP client; touches neither the network nor the store.
    pub fn new(config: AccountClientConfig, license: LicenseClient) -> Result<Self, AccountError> {
        let client = Client::try_new(config.http.clone()).map_err(|_| AccountError::Network)?;
        Ok(Self {
            core: Some(Arc::new(AccountCore {
                lock_path: config.dir.join("account.lock"),
                client,
                cfg: config,
                unresolved: AtomicBool::new(false),
                last: Mutex::new(None),
            })),
            license,
        })
    }

    /// An app with no account service: `status` is signed out and every account operation is
    /// [`AccountError::Unconfigured`] with no network. The link-code operations still run on
    /// `license`.
    pub fn disabled(license: LicenseClient) -> Self {
        Self {
            core: None,
            license,
        }
    }

    /// Whether the account service is disabled in this build.
    pub fn is_disabled(&self) -> bool {
        self.core.is_none()
    }

    fn configured(&self) -> Result<&Arc<AccountCore>, AccountError> {
        self.core.as_ref().ok_or(AccountError::Unconfigured)
    }

    /// The session as stored. No network. Takes the account lock with the status wait; while it
    /// is held the answer is the last view this process saw, marked `busy` (display only).
    pub fn status(&self) -> AccountStatusView {
        match &self.core {
            Some(core) => core.status(),
            None => AccountStatusView::signed_out(),
        }
    }

    /// Confirm the session with the service (`GET /v1/shop/auth/me`) and refresh the stored e-mail
    /// and issue time. A 401 clears the local session and is [`AccountError::SessionExpired`].
    /// With no session it answers signed out and sends nothing.
    pub fn refresh(&self) -> Result<AccountStatusView, AccountError> {
        self.configured()?.refresh()
    }

    /// Ask the service to e-mail a sign-in link. The service always answers 200 whether or not
    /// the address has an account, so success says nothing about the address.
    pub fn request_sign_in(&self, email: &str) -> Result<(), AccountError> {
        self.configured()?.request_sign_in(email)
    }

    /// Complete the sign-in with the token from the e-mailed link. The code is zeroed when this
    /// returns. The session is stored only if nothing changed the stored session meanwhile.
    pub fn complete_sign_in(&self, code: SecretString) -> Result<AccountStatusView, AccountError> {
        self.configured()?.complete_sign_in(code)
    }

    /// Sign out: write the tombstone, then (best effort, outside the lock) tell the service. A
    /// tombstone that cannot be written is an error and sends nothing. Never touches the licence.
    pub fn sign_out(&self) -> Result<AccountStatusView, AccountError> {
        self.configured()?.sign_out()
    }

    /// The account's devices.
    pub fn list_devices(&self) -> Result<Vec<AccountDeviceView>, AccountError> {
        self.configured()?.list_devices()
    }

    /// Rename a device row. `device` is an [`AccountDeviceView::id`].
    pub fn rename_device(&self, device: &str, label: &str) -> Result<(), AccountError> {
        self.configured()?.rename_device(device, label)
    }

    /// Remove a device row, which frees its seat. `device` is an [`AccountDeviceView::id`].
    pub fn remove_device(&self, device: &str) -> Result<(), AccountError> {
        self.configured()?.remove_device(device)
    }

    /// Delete the account. See [`AccountDeletion`]: a refusal (an active subscription) and an
    /// answer that cannot be told apart from "may have happened" are results, not errors, and the
    /// local session is cleared only when the service said 200.
    pub fn delete_account(
        &self,
        confirmation: DeleteAccountConfirmation,
    ) -> Result<AccountDeletion, AccountError> {
        self.configured()?.delete_account(confirmation)
    }

    /// Ask for a device-link code. See [`LicenseClient::link_request`].
    pub fn request_link(&self) -> Result<LinkRequestOutcome, LicenseError> {
        self.license.link_request()
    }

    /// Redeem a device-link code on this device. See [`LicenseClient::link_redeem`].
    pub fn redeem_link(
        &self,
        code: &str,
        device_label: Option<&str>,
    ) -> Result<LicenseView, LicenseError> {
        self.license.link_redeem(code, device_label)
    }
}

struct AccountCore {
    cfg: AccountClientConfig,
    client: Client,
    lock_path: PathBuf,
    /// A session write did not settle in this process.
    unresolved: AtomicBool,
    last: Mutex<Option<AccountStatusView>>,
}

fn now() -> i64 {
    crate::system_now_unix().unwrap_or(0)
}

fn rejected_status(status: u16) -> AccountError {
    match status {
        429 => AccountError::rejected(AccountRejectReason::RateLimited),
        400..=499 => AccountError::rejected(AccountRejectReason::Other),
        _ => AccountError::Network,
    }
}

fn json_object(reply: &Reply) -> Result<Map<String, Value>, AccountError> {
    match serde_json::from_slice::<Value>(&reply.body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(AccountError::BadResponse),
    }
}

fn text(map: &Map<String, Value>, name: &str) -> Option<String> {
    map.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A sign-in code is the opaque token of an e-mailed link: bounded text without control
/// characters. Anything else is refused unsent.
fn plausible_code(code: &str) -> bool {
    !code.is_empty() && code.len() as u64 <= MAX_TOKEN_BYTES && !code.chars().any(char::is_control)
}

/// A server-generated row id placed in a URL path: plain identifier characters only, so no
/// separator, dot segment, query or encoded escape can reach the path.
fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_DEVICE_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_label(label: &str) -> bool {
    label.len() <= MAX_LABEL_BYTES && !label.chars().any(char::is_control)
}

/// A closed-vocabulary word of a device row (platform, pool, state, app key): short, plain
/// characters; anything else is dropped.
fn word(map: &Map<String, Value>, name: &str) -> Option<String> {
    text(map, name).filter(|value| {
        value.len() <= 32
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

fn time_ms(map: &Map<String, Value>, name: &str) -> Option<i64> {
    text(map, name)
        .as_deref()
        .and_then(parse_rfc3339)
        .map(to_ms)
}

fn device_view(value: &Value) -> Result<AccountDeviceView, AccountError> {
    let row = value.as_object().ok_or(AccountError::BadResponse)?;
    let id = text(row, "id")
        .filter(|id| valid_device_id(id))
        .ok_or(AccountError::BadResponse)?;
    let label = text(row, "label").map(|label| {
        label
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_DISPLAY_CHARS)
            .collect::<String>()
    });
    Ok(AccountDeviceView {
        id,
        license_suffix: text(row, "licenseKey").as_deref().and_then(suffix),
        app_key: word(row, "appKey").unwrap_or_default(),
        platform: word(row, "platform"),
        pool: word(row, "pool"),
        label: label.filter(|label| !label.is_empty()),
        state: word(row, "state").unwrap_or_else(|| "unknown".to_string()),
        last_seen_at: time_ms(row, "lastSeenAt"),
        activated_at: time_ms(row, "activatedAt"),
        generation: row.get("generation").and_then(Value::as_u64).unwrap_or(0),
    })
}

impl AccountCore {
    // ---- plumbing ---------------------------------------------------------------------

    fn lock(&self, wait: Duration) -> Result<OpGuard, AccountError> {
        acquire_polled(&self.lock_path, wait).map_err(|error| match error {
            LicenseError::Busy => AccountError::Busy,
            _ => AccountError::Store,
        })
    }

    /// Read the session item. The caller holds the account lock.
    fn read(&self) -> Result<Item, AccountError> {
        match self.cfg.store.get(&self.cfg.account) {
            Ok(None) => Ok(Item::absent()),
            Ok(Some(secret)) if secret.is_empty() => Ok(Item::absent()),
            Ok(Some(secret)) => Ok(Item::from_secret(&secret)),
            Err(_) => Err(AccountError::Store),
        }
    }

    /// Write `secret` on a worker that owns `guard` until the credential store call has really
    /// finished. The guard comes back when the worker finished inside the wait.
    fn write(
        &self,
        guard: OpGuard,
        secret: rightkit_credentials::Secret,
    ) -> Result<OpGuard, AccountError> {
        let (store, account) = (self.cfg.store.clone(), self.cfg.account.clone());
        let fenced = run_fenced(guard, self.cfg.persist_wait, move || {
            let result = store.set(&account, &secret);
            settle(&store, &account);
            result
        });
        match fenced {
            Fenced::Done(Ok(()), guard) => {
                self.unresolved.store(false, Ordering::SeqCst);
                Ok(guard)
            }
            Fenced::Done(Err(error), guard) => {
                drop(guard);
                if matches!(
                    error,
                    CredError::Timeout { .. } | CredError::InProgress { .. }
                ) {
                    self.unresolved.store(true, Ordering::SeqCst);
                    Err(AccountError::Unresolved)
                } else {
                    Err(AccountError::Store)
                }
            }
            Fenced::TimedOut | Fenced::Failed => {
                self.unresolved.store(true, Ordering::SeqCst);
                Err(AccountError::Unresolved)
            }
        }
    }

    /// One HTTPS call to the compiled-in origin. `secrets` are registered so the HTTP layer
    /// scrubs them from its errors.
    fn call(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<&Value>,
        secrets: &[&str],
    ) -> Result<Reply, SendFailure> {
        let mut request = Request::new(method, format!("{}{path}", self.cfg.origin))
            .header("Accept", "application/json");
        if let Some(token) = bearer {
            let value = Zeroizing::new(format!("Bearer {token}"));
            request = request.secret_header("Authorization", value.as_str());
        }
        if let Some(body) = body {
            let encoded =
                Zeroizing::new(serde_json::to_vec(body).map_err(|_| SendFailure::NotSent)?);
            request = request
                .header("Content-Type", "application/json")
                .body(encoded.to_vec());
        }
        for secret in secrets {
            request = request.redact_value(*secret);
        }
        let result = self.client.send_any(&request);
        request.body.zeroize();
        match result {
            Ok(response) => Ok(Reply {
                status: response.status,
                body: Zeroizing::new(response.body),
            }),
            Err(error) => Err(classify(&error)),
        }
    }

    /// Best effort: tell the service a session token is finished. Never reports anything.
    fn logout_quietly(&self, token: &str) {
        let _ = self.call(
            Method::Post,
            AUTH_LOGOUT,
            Some(token),
            Some(&json!({})),
            &[],
        );
    }

    /// Write `session` as the stored session, but only if the stored item is still `baseline`.
    fn store_if_unchanged(&self, baseline: &Item, session: &Session) -> Result<(), AccountError> {
        let guard = self.lock(self.cfg.mutation_wait)?;
        let current = self.read()?;
        if !baseline.is_same_as(&current) {
            return Err(AccountError::Superseded);
        }
        let secret = encode_active(session).map_err(|_| AccountError::Store)?;
        self.write(guard, secret).map(drop)
    }

    /// A 401 said the session read as `baseline` is dead: tombstone it, unless the stored item
    /// has changed since (a newer session is not the dead one). `false` only when it changed. A
    /// failure to clear is not reported: the token is dead either way and the next call finds that
    /// out again.
    fn clear_if_same(&self, baseline: &Item) -> bool {
        let Ok(guard) = self.lock(self.cfg.mutation_wait) else {
            return true;
        };
        let Ok(current) = self.read() else {
            return true;
        };
        if !baseline.is_same_as(&current) {
            return false;
        }
        if let Ok(secret) = encode_tombstone() {
            let _ = self.write(guard, secret);
        }
        true
    }

    /// The authenticated call: read the session (a short lock hold), call without the lock, and
    /// on 401 clear that session. `body` is JSON.
    fn authed(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Reply, Failure> {
        let baseline = {
            let _guard = self.lock(self.cfg.mutation_wait)?;
            self.read()?
        };
        let Some(session) = baseline.session() else {
            return Err(AccountError::NotSignedIn.into());
        };
        let reply = self.call(method, path, Some(session.token.as_str()), body, &[])?;
        if reply.status == 401 {
            let error = if self.clear_if_same(&baseline) {
                AccountError::SessionExpired
            } else {
                // A newer session replaced the one the 401 was about: it stands.
                AccountError::Superseded
            };
            return Err(Failure::Account(error));
        }
        Ok(reply)
    }

    fn remember(&self, view: &AccountStatusView) {
        if !view.busy && !view.unresolved {
            if let Ok(mut last) = self.last.lock() {
                *last = Some(view.clone());
            }
        }
    }

    fn busy_view(&self) -> AccountStatusView {
        let last = self.last.lock().ok().and_then(|last| last.clone());
        AccountStatusView {
            busy: true,
            ..last.unwrap_or_else(AccountStatusView::signed_out)
        }
    }

    // ---- operations -------------------------------------------------------------------

    fn status(&self) -> AccountStatusView {
        let guard = match self.lock(self.cfg.status_wait) {
            Ok(guard) => guard,
            Err(AccountError::Busy) => return self.busy_view(),
            Err(_) => return AccountStatusView::unresolved(),
        };
        // A write that held the lock has settled by now: what is stored is what is stored.
        let view = match self.read() {
            Ok(item) => {
                self.unresolved.store(false, Ordering::SeqCst);
                AccountStatusView::of_item(&item)
            }
            Err(_) => AccountStatusView::unresolved(),
        };
        drop(guard);
        self.remember(&view);
        view
    }

    fn request_sign_in(&self, email: &str) -> Result<(), AccountError> {
        let email = email.trim();
        if !plausible_email(email) {
            return Err(AccountError::rejected(AccountRejectReason::InvalidInput));
        }
        let reply = self
            .call(
                Method::Post,
                MAGIC_LINK_REQUEST,
                None,
                Some(&json!({ "email": email })),
                &[],
            )
            .map_err(|_| AccountError::Network)?;
        match reply.status {
            // Always 200, whether or not the address has an account; the body is `{ok}`.
            200 => json_object(&reply).map(|_| ()),
            status => Err(rejected_status(status)),
        }
    }

    fn complete_sign_in(&self, code: SecretString) -> Result<AccountStatusView, AccountError> {
        let trimmed = code.expose().trim();
        if !plausible_code(trimmed) {
            return Err(AccountError::rejected(AccountRejectReason::LinkInvalid));
        }
        // The item is read BEFORE the one-time code is spent: a store that cannot be read burns
        // nothing, and the revision read here is what the result is written against.
        let baseline = {
            let _guard = self.lock(self.cfg.mutation_wait)?;
            self.read()?
        };
        let reply = self
            .call(
                Method::Post,
                MAGIC_LINK_CONSUME,
                None,
                Some(&json!({ "token": trimmed })),
                &[trimmed],
            )
            .map_err(|_| AccountError::Network)?;
        drop(code);
        match reply.status {
            200 => {}
            409 => {
                return Err(AccountError::rejected(AccountRejectReason::LinkInvalid));
            }
            status => return Err(rejected_status(status)),
        }
        let top = json_object(&reply)?;
        let token = text(&top, "token")
            .filter(|token| token.len() as u64 <= MAX_TOKEN_BYTES)
            .ok_or(AccountError::BadResponse)?;
        let email = top
            .get("customer")
            .and_then(Value::as_object)
            .and_then(|customer| text(customer, "email"))
            .ok_or(AccountError::BadResponse)?;
        let session = Session {
            token: Zeroizing::new(token),
            email: Some(email),
            issued_at: now(),
        };
        if let Err(error) = self.store_if_unchanged(&baseline, &session) {
            // The code is spent. When the session is certainly not kept, do not leave it alive at
            // the service. When the write may still land (`Unresolved`), logging out could strand
            // a stored session: it is left, and the next call that gets a 401 clears it.
            if error != AccountError::Unresolved {
                self.logout_quietly(&session.token);
            }
            return Err(error);
        }
        let view = AccountStatusView::of(&session);
        self.remember(&view);
        Ok(view)
    }

    fn refresh(&self) -> Result<AccountStatusView, AccountError> {
        let baseline = {
            let _guard = self.lock(self.cfg.mutation_wait)?;
            self.read()?
        };
        let Some(session) = baseline.session() else {
            return Ok(AccountStatusView::signed_out());
        };
        let reply = self
            .call(
                Method::Get,
                AUTH_ME,
                Some(session.token.as_str()),
                None,
                &[],
            )
            .map_err(|_| AccountError::Network)?;
        match reply.status {
            200 => {}
            401 => {
                return Err(if self.clear_if_same(&baseline) {
                    AccountError::SessionExpired
                } else {
                    // A newer session replaced the one the 401 was about: it stands.
                    AccountError::Superseded
                });
            }
            status => return Err(rejected_status(status)),
        }
        // `/auth/me` answers the customer: the e-mail is at the top level.
        let email = text(&json_object(&reply)?, "email").ok_or(AccountError::BadResponse)?;
        let updated = Session {
            token: session.token.clone(),
            email: Some(email),
            issued_at: now(),
        };
        self.store_if_unchanged(&baseline, &updated)?;
        let view = AccountStatusView::of(&updated);
        self.remember(&view);
        Ok(view)
    }

    fn sign_out(&self) -> Result<AccountStatusView, AccountError> {
        let guard = self.lock(self.cfg.mutation_wait)?;
        let item = self.read()?;
        let token = item.session().map(|session| session.token.clone());
        // An unreadable item is cleared too; an absent or already signed-out one needs nothing.
        let needs_tombstone = token.is_some() || matches!(item.state, State::Corrupt);
        if needs_tombstone {
            let secret = encode_tombstone().map_err(|_| AccountError::Store)?;
            // Local first and authoritative: if this fails nothing was changed and the service
            // is not told, so the two never disagree about a session that is still here.
            self.write(guard, secret).map(drop)?;
        } else {
            drop(guard);
        }
        if let Some(token) = token {
            self.logout_quietly(&token);
        }
        let view = AccountStatusView::signed_out();
        self.remember(&view);
        Ok(view)
    }

    fn list_devices(&self) -> Result<Vec<AccountDeviceView>, AccountError> {
        let reply = self
            .authed(Method::Get, ACCOUNT_DEVICES, None)
            .map_err(Failure::into_account)?;
        if reply.status != 200 {
            return Err(rejected_status(reply.status));
        }
        let top = json_object(&reply)?;
        let items = top
            .get("items")
            .and_then(Value::as_array)
            .ok_or(AccountError::BadResponse)?;
        items.iter().map(device_view).collect()
    }

    fn rename_device(&self, device: &str, label: &str) -> Result<(), AccountError> {
        if !valid_device_id(device) || !valid_label(label) {
            return Err(AccountError::rejected(AccountRejectReason::InvalidInput));
        }
        let reply = self
            .authed(
                Method::Patch,
                &format!("{ACCOUNT_DEVICES}/{device}"),
                Some(&json!({ "label": label })),
            )
            .map_err(Failure::into_account)?;
        device_answer(&reply)
    }

    fn remove_device(&self, device: &str) -> Result<(), AccountError> {
        if !valid_device_id(device) {
            return Err(AccountError::rejected(AccountRejectReason::InvalidInput));
        }
        let reply = self
            .authed(Method::Delete, &format!("{ACCOUNT_DEVICES}/{device}"), None)
            .map_err(Failure::into_account)?;
        device_answer(&reply)
    }

    fn delete_account(
        &self,
        _confirmation: DeleteAccountConfirmation,
    ) -> Result<AccountDeletion, AccountError> {
        let reply = match self.authed(Method::Delete, ACCOUNT_DELETE, None) {
            Ok(reply) => reply,
            Err(Failure::Account(error)) => return Err(error),
            // Nothing can have reached the service: the account was not deleted.
            Err(Failure::Send(SendFailure::NotSent)) => return Err(AccountError::Network),
            // The request may have been delivered: neither outcome is proven.
            Err(Failure::Send(SendFailure::Uncertain)) => {
                return Ok(AccountDeletion::Indeterminate)
            }
        };
        Ok(match reply.status {
            200 => {
                // The account this session authenticates no longer exists: clear the local
                // session, like a sign-out. There is no server session left to log out of.
                let cleared = self.clear_local_after_deletion();
                self.remember(&AccountStatusView::signed_out());
                AccountDeletion::Deleted {
                    local_session_cleared: cleared,
                }
            }
            409 => AccountDeletion::Refused {
                reason: DeletionRefusal::ActiveSubscription,
            },
            429 => AccountDeletion::Refused {
                reason: DeletionRefusal::RateLimited,
            },
            400..=499 => AccountDeletion::Refused {
                reason: DeletionRefusal::Other,
            },
            // A 5xx, a redirect or an unexpected 2xx: not proof either way.
            _ => AccountDeletion::Indeterminate,
        })
    }

    fn clear_local_after_deletion(&self) -> bool {
        let Ok(guard) = self.lock(self.cfg.mutation_wait) else {
            return false;
        };
        let Ok(secret) = encode_tombstone() else {
            return false;
        };
        self.write(guard, secret).is_ok()
    }
}

/// The answer of a rename or a remove.
fn device_answer(reply: &Reply) -> Result<(), AccountError> {
    match reply.status {
        200 => Ok(()),
        404 => Err(AccountError::rejected(AccountRejectReason::DeviceNotFound)),
        status => Err(rejected_status(status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            body: Zeroizing::new(body.as_bytes().to_vec()),
        }
    }

    #[test]
    fn device_ids_placed_in_a_path_are_plain_identifiers() {
        for good in ["3f2c9a10-aaaa-bbbb-cccc-0123456789ab", "dev_1", "A"] {
            assert!(valid_device_id(good), "{good}");
        }
        let long = "a".repeat(MAX_DEVICE_ID_BYTES + 1);
        for bad in [
            "",
            "../account",
            "a/b",
            "a b",
            "a?x=1",
            "a#b",
            "a%2fb",
            "a.b",
            "a\\b",
            "id\n",
            long.as_str(),
        ] {
            assert!(!valid_device_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn labels_and_codes_are_bounded_plain_text() {
        assert!(valid_label("Work Mac") && valid_label(""));
        assert!(!valid_label("a\u{7}b"));
        assert!(!valid_label(&"x".repeat(MAX_LABEL_BYTES + 1)));
        assert!(plausible_code("abc.DEF-123"));
        assert!(!plausible_code("") && !plausible_code("a\nb"));
        assert!(!plausible_code(&"x".repeat(MAX_TOKEN_BYTES as usize + 1)));
    }

    #[test]
    fn a_device_row_is_reduced_to_an_allowlist() {
        let row = json!({
            "id": "row-1",
            "licenseKey": "KEY-SENTINEL-9876",
            "appKey": "heardright",
            "platform": "macos",
            "pool": "computer",
            "label": "  Work\u{7} Mac ",
            "state": "active",
            "lastSeenAt": "2026-10-16T00:00:00Z",
            "activatedAt": "not a time",
            "generation": 4,
            "someFutureField": "x"
        });
        let view = device_view(&row).unwrap();
        assert_eq!(view.id, "row-1");
        assert_eq!(view.license_suffix.as_deref(), Some("9876"));
        assert_eq!(view.app_key, "heardright");
        assert_eq!(view.label.as_deref(), Some("Work Mac"));
        assert_eq!(view.last_seen_at, Some(1_792_108_800_000));
        assert_eq!(view.activated_at, None);
        assert_eq!(view.generation, 4);
        // No field of the serialized view carries the licence key.
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("SENTINEL"), "{json}");
    }

    #[test]
    fn an_unusable_device_id_fails_the_whole_listing_closed() {
        assert_eq!(
            device_view(&json!({"id": "../x"})).unwrap_err(),
            AccountError::BadResponse
        );
        assert_eq!(
            device_view(&json!({})).unwrap_err(),
            AccountError::BadResponse
        );
        assert_eq!(
            device_view(&json!("row")).unwrap_err(),
            AccountError::BadResponse
        );
        // Unknown words are dropped, not forwarded.
        let view =
            device_view(&json!({"id": "r", "state": "<script>", "platform": "x y"})).unwrap();
        assert_eq!(view.state, "unknown");
        assert_eq!(view.platform, None);
    }

    #[test]
    fn statuses_map_to_fixed_reasons() {
        assert_eq!(
            rejected_status(429),
            AccountError::rejected(AccountRejectReason::RateLimited)
        );
        assert_eq!(
            rejected_status(403),
            AccountError::rejected(AccountRejectReason::Other)
        );
        assert_eq!(rejected_status(500), AccountError::Network);
        assert_eq!(rejected_status(302), AccountError::Network);
        assert_eq!(
            device_answer(&reply(404, "SERVER TEXT")).unwrap_err(),
            AccountError::rejected(AccountRejectReason::DeviceNotFound)
        );
        assert_eq!(device_answer(&reply(200, "")), Ok(()));
    }

    #[test]
    fn errors_have_fixed_text_and_wire_codes() {
        let all = [
            AccountError::Unconfigured,
            AccountError::Busy,
            AccountError::Unresolved,
            AccountError::Network,
            AccountError::NotSignedIn,
            AccountError::SessionExpired,
            AccountError::Superseded,
            AccountError::rejected(AccountRejectReason::LinkInvalid),
            AccountError::Store,
            AccountError::BadResponse,
        ];
        for error in all {
            assert!(!error.to_string().is_empty());
            let wire = serde_json::to_value(error.to_command_error()).unwrap();
            assert!(wire.get("code").is_some());
        }
        let wire = serde_json::to_value(
            AccountError::rejected(AccountRejectReason::DeviceNotFound).to_command_error(),
        )
        .unwrap();
        assert_eq!(
            wire,
            json!({"code": "rejected", "reason": "deviceNotFound"})
        );
    }

    #[test]
    fn a_deletion_outcome_serializes_with_its_tag() {
        assert_eq!(
            serde_json::to_value(AccountDeletion::Refused {
                reason: DeletionRefusal::ActiveSubscription
            })
            .unwrap(),
            json!({"outcome": "refused", "reason": "activeSubscription"})
        );
        assert_eq!(
            serde_json::to_value(AccountDeletion::Deleted {
                local_session_cleared: true
            })
            .unwrap(),
            json!({"outcome": "deleted", "localSessionCleared": true})
        );
        assert_eq!(
            serde_json::to_value(AccountDeletion::Indeterminate).unwrap(),
            json!({"outcome": "indeterminate"})
        );
    }

    #[test]
    fn only_a_connect_phase_failure_proves_nothing_was_sent() {
        assert_eq!(
            classify(&HttpError::Transport {
                message: "x".into(),
                connect_phase: true,
                phase: rightkit_http::Phase::Connect,
            }),
            SendFailure::NotSent
        );
        assert_eq!(
            classify(&HttpError::Transport {
                message: "x".into(),
                connect_phase: false,
                phase: rightkit_http::Phase::Headers,
            }),
            SendFailure::Uncertain
        );
        assert_eq!(
            classify(&HttpError::Timeout {
                message: "x".into(),
                phase: rightkit_http::Phase::Body,
            }),
            SendFailure::Uncertain
        );
        assert_eq!(classify(&HttpError::Cancelled), SendFailure::Uncertain);
    }
}
