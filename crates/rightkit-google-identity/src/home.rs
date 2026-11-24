//! One app's Google connection: where the grant lives, how a sign-in is run, and what a caller may
//! learn about a failure.
//!
//! The library is `rightkit-google-apis`; [`Home`] decides where the grant is kept and maps every
//! failure to a [`Failure`] with a fixed code and fixed text. The app supplies its names, its
//! client id and secret, its state folder and the few sentences that name the app ([`Texts`]).
//!
//! * **Sign-in** ([`Home::sign_in`]): the PKCE loopback flow in the user's default browser, never a
//!   webview (`oauth::run_flow`).
//! * **The desktop** keeps the refresh token the sign-in returned in the OS credential store
//!   ([`Home::pending_grant`]), under the app's own service name, until the owner carries it to the
//!   duty host.
//! * **The duty host** keeps it in `oauth::sealed_file_store` (AES-256-GCM, a 0700 folder). The
//!   32-byte key is a credential the service manager provides
//!   (`$CREDENTIALS_DIRECTORY/<key credential>`), never a file beside the data: a credentials
//!   folder that contains or sits inside the store folder is refused.
//! * **No token leaves this module.** Every failure a caller sees is a [`Failure`]; no library
//!   error text, response body, path or token reaches it.

use rightkit_google_apis::google::{Google, GoogleError};
use rightkit_google_apis::oauth;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// The longest refresh token accepted on the way into the store.
const GRANT_MAX: usize = 4096;

/// A failure a caller may show: a stable code and fixed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
        }
    }
}

/// The sentences that name the app. Everything else a [`Failure`] says is the same for every app.
#[derive(Clone, Copy, Debug)]
pub struct Texts {
    /// `google_sign_in_again`: the grant is missing, revoked or expired.
    pub sign_in_again: &'static str,
    /// `google_not_configured`: the build has no client id or secret.
    pub not_configured: &'static str,
    /// `google_no_refresh_token`: Google returned no refresh token.
    pub no_refresh_token: &'static str,
    /// `google_key_missing`: the service manager has not provided the store key.
    pub key_missing: &'static str,
    /// `google_credential_store_unavailable`: the OS credential store is guarded or unavailable.
    pub desktop_unavailable: &'static str,
}

/// What the key credential is, for a status command; never its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// No `$CREDENTIALS_DIRECTORY`, or no credential in it.
    Missing,
    /// There, but not exactly 32 bytes.
    Invalid,
    /// The credentials folder contains the store folder or sits inside it.
    BesideData,
    /// 32 bytes, in a folder apart from the data.
    Ready,
}

/// A sign-in waiting to be carried to the duty host. It prints as `[redacted]`.
pub struct PendingGrant {
    email: String,
    refresh_token: String,
}

impl PendingGrant {
    pub fn email(&self) -> &str {
        &self.email
    }

    /// The token, for the carry step only (`Home::install_grant` on the receiving side). Never log it.
    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl std::fmt::Debug for PendingGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingGrant { [redacted] }")
    }
}

/// One app's Google connection. Cheap to build: make one per call.
#[derive(Clone, Debug)]
pub struct Home {
    /// The OAuth client id as the build embedded it (`option_env!`); trimmed and checked here.
    pub client_id: Option<&'static str>,
    /// The OAuth client secret as the build embedded it.
    pub client_secret: Option<&'static str>,
    /// The grant's name inside the sealed store (and the resolver's lock). Not a secret.
    pub account: &'static str,
    /// The credential the service manager provides: exactly 32 bytes.
    pub key_credential: &'static str,
    /// The sealed store's folder, or `None` when the state root cannot be resolved.
    pub store_root: Option<PathBuf>,
    /// The OS credential store's service name on the computer that signed in.
    pub desktop_service: &'static str,
    /// The one item kept under that service.
    pub desktop_pending: &'static str,
    /// The name of the one runtime thread (first caller in the process wins).
    pub thread_name: &'static str,
    pub texts: Texts,
}

fn embedded(value: Option<&'static str>) -> Option<&'static str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn store_unavailable() -> Failure {
    Failure::new(
        "google_store_unavailable",
        "the Google sign-in store could not be opened on this computer",
    )
}

fn rate_limited() -> Failure {
    Failure::new(
        "google_rate_limited",
        "Google is limiting requests; try again later",
    )
}

fn unavailable() -> Failure {
    Failure::new(
        "google_unavailable",
        "Google could not be reached or answered with an error; try again later",
    )
}

fn bad_response() -> Failure {
    Failure::new(
        "google_bad_response",
        "Google's answer, or the saved sign-in, could not be read; if this repeats, sign in again",
    )
}

fn status_failure(status: u16) -> Failure {
    Failure::new(
        "google_http",
        format!("Google refused the request (HTTP {status})"),
    )
}

fn pending_unreadable() -> Failure {
    Failure::new(
        "google_credential_store_unavailable",
        "the saved Google sign-in on this computer could not be read",
    )
}

/// What a failed sign-in says: fixed text, chosen by the library's own words and then dropped.
fn sign_in_failure(texts: &Texts, text: &str) -> Failure {
    let lower = text.to_ascii_lowercase();
    if lower.contains("no refresh token") {
        Failure::new("google_no_refresh_token", texts.no_refresh_token)
    } else if lower.contains("state mismatch") {
        Failure::new(
            "google_sign_in_aborted",
            "the sign-in was stopped because its answer did not match the request",
        )
    } else {
        Failure::new(
            "google_sign_in_failed",
            "the Google sign-in did not complete (it waits five minutes for the browser)",
        )
    }
}

/// One process-wide runtime with one worker. The library's HTTP client is a process-wide static
/// that keeps pooled connections; a runtime per call would leave them tied to a dead runtime.
fn runtime(thread_name: &'static str) -> Result<&'static tokio::runtime::Runtime, Failure> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name(thread_name)
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| {
            Failure::new(
                "google_unavailable",
                "the Google runtime could not be started on this computer",
            )
        })
}

fn credentials_directory() -> Option<PathBuf> {
    std::env::var_os("CREDENTIALS_DIRECTORY")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

impl Home {
    // ---- failures ------------------------------------------------------------------------------

    /// The sign-in a Google call needs again: the grant is missing, revoked or expired.
    pub fn sign_in_again(&self) -> Failure {
        Failure::new("google_sign_in_again", self.texts.sign_in_again)
    }

    /// A library failure as a `Failure`. The library's text is dropped: it can carry a response body.
    pub fn from_google(&self, error: &GoogleError) -> Failure {
        match error {
            GoogleError::Auth(_) => self.sign_in_again(),
            GoogleError::RateLimited(_) | GoogleError::RateLimitedAfter { .. } => rate_limited(),
            GoogleError::Transient(_) | GoogleError::Uncertain(_) => unavailable(),
            GoogleError::Http { status, .. } => status_failure(*status),
            GoogleError::Parse(_) => bad_response(),
        }
    }

    /// A library failure that only arrives as text (`gmail::run_delta_sync` returns
    /// `Result<_, String>`): classified by `GoogleError`'s own `Display` prefixes and then dropped.
    pub fn from_text(&self, text: &str) -> Failure {
        let lower = text.trim_start().to_ascii_lowercase();
        if lower.starts_with("auth failed") {
            self.sign_in_again()
        } else if lower.starts_with("rate limited") {
            rate_limited()
        } else if lower.starts_with("parse error") {
            bad_response()
        } else if let Some(rest) = lower.strip_prefix("http ") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<u16>() {
                Ok(status) => status_failure(status),
                Err(_) => unavailable(),
            }
        } else {
            unavailable()
        }
    }

    // ---- the client id and secret ---------------------------------------------------------------

    /// The OAuth client id and secret this build was compiled with; `google_not_configured` when
    /// either was missing from the build environment.
    pub fn client_credentials(&self) -> Result<(&'static str, &'static str), Failure> {
        match (embedded(self.client_id), embedded(self.client_secret)) {
            (Some(id), Some(secret)) => Ok((id, secret)),
            _ => Err(Failure::new(
                "google_not_configured",
                self.texts.not_configured,
            )),
        }
    }

    // ---- one runtime for every call -------------------------------------------------------------

    /// Run one async Google job to its end or `limit`, whichever comes first. Call it from a plain
    /// thread, never from inside a tokio runtime.
    pub fn run<T>(
        &self,
        limit: Duration,
        job: impl Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        runtime(self.thread_name)?.block_on(async {
            match tokio::time::timeout(limit, job).await {
                Ok(result) => result,
                Err(_) => Err(Failure::new(
                    "google_timed_out",
                    "Google did not answer in time; try again later",
                )),
            }
        })
    }

    // ---- sign-in, on a desktop ------------------------------------------------------------------

    /// Run the sign-in in the default browser and keep the refresh token in the OS credential store
    /// (`save_pending`). Returns the account's email address, which is the only thing a caller may
    /// print.
    pub fn sign_in(&self) -> Result<String, Failure> {
        let (id, secret) = self.client_credentials()?;
        let flow = self.run(Duration::from_secs(330), async move {
            Ok::<_, Failure>(oauth::run_flow(id.to_string(), secret.to_string()).await)
        })?;
        let granted = flow.map_err(|text| sign_in_failure(&self.texts, &text))?;
        self.save_pending(&granted.email, &granted.refresh_token)?;
        Ok(granted.email)
    }

    // ---- the grant kept on the computer that signed in ------------------------------------------

    fn desktop_unavailable(&self) -> Failure {
        Failure::new(
            "google_credential_store_unavailable",
            self.texts.desktop_unavailable,
        )
    }

    fn desktop_box(&self) -> Result<rightkit_credentials::Store, Failure> {
        let namespace = rightkit_credentials::Namespace::new(self.desktop_service.to_string())
            .map_err(|_| self.desktop_unavailable())?;
        let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
        if !store.is_durable() {
            return Err(self.desktop_unavailable());
        }
        Ok(store)
    }

    /// Keep a sign-in's refresh token in the OS credential store, replacing an older one.
    fn save_pending(&self, email: &str, refresh_token: &str) -> Result<(), Failure> {
        let store = self.desktop_box()?;
        let document = serde_json::json!({"v": 1, "email": email, "refreshToken": refresh_token});
        store
            .set_str(self.desktop_pending, &document.to_string())
            .map_err(|_| pending_unreadable())
    }

    /// The sign-in waiting on this computer, if any.
    pub fn pending_grant(&self) -> Result<Option<PendingGrant>, Failure> {
        let store = self.desktop_box()?;
        let secret = match store.get(self.desktop_pending) {
            Ok(Some(secret)) => secret,
            Ok(None) => return Ok(None),
            Err(_) => return Err(pending_unreadable()),
        };
        let text = secret.expose_str().map_err(|_| pending_unreadable())?;
        let document: serde_json::Value =
            serde_json::from_str(text).map_err(|_| pending_unreadable())?;
        let email = document["email"].as_str().unwrap_or_default().to_string();
        let refresh_token = document["refreshToken"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if refresh_token.is_empty() {
            return Err(pending_unreadable());
        }
        Ok(Some(PendingGrant {
            email,
            refresh_token,
        }))
    }

    /// The email of the sign-in waiting on this computer.
    pub fn pending_email(&self) -> Result<Option<String>, Failure> {
        Ok(self.pending_grant()?.map(|grant| grant.email))
    }

    /// Delete the sign-in kept on this computer. Nothing to delete is not an error.
    pub fn forget_pending(&self) -> Result<(), Failure> {
        if self.pending_email()?.is_none() {
            return Ok(());
        }
        self.desktop_box()?
            .delete(self.desktop_pending)
            .map_err(|_| pending_unreadable())
            .map(|_| ())
    }

    // ---- the sealed store, on the duty host -----------------------------------------------------

    fn store_root(&self) -> Result<&Path, Failure> {
        self.store_root.as_deref().ok_or_else(store_unavailable)
    }

    /// The 32-byte store key from the credential the service manager provides.
    fn store_key(&self, root: &Path) -> Result<[u8; 32], Failure> {
        let missing = || Failure::new("google_key_missing", self.texts.key_missing);
        let dir = credentials_directory().ok_or_else(missing)?;
        if root.starts_with(&dir) || dir.starts_with(root) {
            return Err(Failure::new(
                "google_key_beside_data",
                "the Google store key must not be kept in or beside the store's own folder",
            ));
        }
        let file = std::fs::File::open(dir.join(self.key_credential)).map_err(|_| missing())?;
        let mut bytes = Vec::with_capacity(33);
        file.take(33)
            .read_to_end(&mut bytes)
            .map_err(|_| missing())?;
        let key = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            Failure::new(
                "google_key_invalid",
                "the Google store key credential is not exactly 32 bytes",
            )
        });
        bytes.fill(0);
        key
    }

    /// The state of the key credential, for a status command.
    pub fn key_state(&self) -> KeyState {
        let Ok(root) = self.store_root() else {
            return KeyState::Missing;
        };
        match self.store_key(root) {
            Ok(_) => KeyState::Ready,
            Err(failure) if failure.code == "google_key_invalid" => KeyState::Invalid,
            Err(failure) if failure.code == "google_key_beside_data" => KeyState::BesideData,
            Err(_) => KeyState::Missing,
        }
    }

    /// Whether the sealed store's folder holds anything yet.
    pub fn store_has_entries(&self) -> bool {
        self.store_root()
            .ok()
            .and_then(|root| std::fs::read_dir(root).ok())
            .is_some_and(|mut entries| entries.next().is_some())
    }

    /// A client over the sealed store: the grant is read, refreshed and written back there, and never
    /// passes through the app's own fields. A missing or revoked grant is `google_sign_in_again` on
    /// the first call, and nothing retries.
    pub fn client(&self) -> Result<Google, Failure> {
        let (id, secret) = self.client_credentials()?;
        let root = self.store_root()?;
        let key = self.store_key(root)?;
        let store = oauth::sealed_file_store(root, key).map_err(|_| store_unavailable())?;
        Google::with_store(id, secret, self.account, store)
            .map_err(|error| self.from_google(&error))
    }

    /// A valid Google access token for a sync, and the account it belongs to. On the duty host the
    /// grant is read from the sealed store; on a desktop it is the sign-in kept in the OS credential
    /// store (`pending_grant`). The token is for the caller's one request: never log, store or sync
    /// it. Blocking; call it from a plain thread. The account is the sign-in's email on a desktop
    /// and the fixed grant name on the duty host, and is only used to bind a sync ledger to one
    /// Google account.
    pub fn access_token_for_sync(&self) -> Result<(String, String), Failure> {
        let (id, secret) = self.client_credentials()?;
        if let Ok(root) = self.store_root() {
            if let Ok(key) = self.store_key(root) {
                let store = oauth::sealed_file_store(root, key).map_err(|_| store_unavailable())?;
                let tokens = oauth::AccessTokens::with_store(id, secret, self.account, store)
                    .map_err(|_| self.sign_in_again())?;
                let token = tokens.access_token().map_err(|_| self.sign_in_again())?;
                return Ok((token, self.account.to_string()));
            }
        }
        let grant = self.pending_grant()?.ok_or_else(|| self.sign_in_again())?;
        let tokens = oauth::AccessTokens::new(id, secret, grant.refresh_token(), self.account)
            .map_err(|_| self.sign_in_again())?;
        let token = tokens.access_token().map_err(|_| self.sign_in_again())?;
        let account = if grant.email().is_empty() {
            self.account.to_string()
        } else {
            grant.email().to_string()
        };
        Ok((token, account))
    }

    /// Write a refresh token into the sealed store as the owner's grant, replacing any older one:
    /// the receiving end of the carry step. `refresh_token` is not logged, returned or kept.
    pub fn install_grant(&self, refresh_token: &str) -> Result<(), Failure> {
        if refresh_token.is_empty()
            || refresh_token.len() > GRANT_MAX
            || refresh_token.chars().any(char::is_control)
        {
            return Err(Failure::new(
                "google_grant_invalid",
                "the sign-in to install is not a refresh token",
            ));
        }
        let root = self.store_root()?;
        let key = self.store_key(root)?;
        let store = oauth::sealed_file_store(root, key).map_err(|_| store_unavailable())?;
        oauth::provision_refresh_token(&*store, self.account, refresh_token)
            .map_err(|_| store_unavailable())
    }
}
