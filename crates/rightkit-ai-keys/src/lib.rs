//! Bring-your-own AI provider keys in the OS credential store: slot storage, save-time validation,
//! the live "is this key accepted" check and the read-state vocabulary.
//!
//! Mechanism only. The host supplies the namespace (how its [`Store`] is opened), the provider
//! table (slot account, display name, models URL, aliases), the entitlement gate and the product's
//! choice of eligible provider. Renderer fields are derived from the provider table.
//!
//! Security properties:
//!
//! - No key, and no fragment of one, appears in any error text, log line or `Debug` output this
//!   crate produces. Error messages name the provider slot and the reason, never a value.
//! - A read failure is never an empty key: [`SidecarKeyState::ReadError`] stays distinct from
//!   [`SidecarKeyState::Absent`], so an OS keychain or ACL regression is never read as "no key".
//! - A live check that cannot reach the provider is [`KeyCheckResult::Unverified`], never
//!   [`KeyCheckResult::Rejected`], so an offline user can still save a good key.
//! - Clearing a slot is never gated (the host gates only writes); one slot failing never stops the
//!   others.
//! - The store account for each slot is the provider's `id` exactly as the host declares it, so the
//!   existing keyring items stay addressed byte for byte.

use std::time::Duration;

use rightkit_credentials::slots::{Slot, SlotOutcome, SlotStore};
use rightkit_credentials::Store;
use serde::{Deserialize, Serialize};

mod byok;
pub use byok::{any_read_error, KeysUpdate, Readiness};

/// One provider the host supports.
#[derive(Debug, Clone, Copy)]
pub struct Provider {
    /// Slot name & store account, also used in error text.
    pub id: &'static str,
    /// Host's display name in failure messages.
    pub display_name: &'static str,
    /// Host-approved public HTTPS endpoint accepting an authenticated Bearer GET.
    pub models_url: &'static str,
    /// Extra spellings the live check accepts for this provider, lowercase.
    pub aliases: &'static [&'static str],
}

/// Result of a live save-time key check. Deliberately distinguishes a provider-rejected key
/// (401/403: the key is wrong) from an unreachable check (network error, timeout, non-auth error
/// status: we could not tell, so the offline user must still be able to save). Never conflate the
/// two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum KeyCheckResult {
    Valid,
    Rejected,
    Unverified,
}

/// Map an HTTP status code to a [`KeyCheckResult`]. Isolated from the transport.
pub fn classify_status(status: u16) -> KeyCheckResult {
    match status {
        200..=299 => KeyCheckResult::Valid,
        401 | 403 => KeyCheckResult::Rejected,
        _ => KeyCheckResult::Unverified,
    }
}

fn models_url(providers: &[Provider], provider: &str) -> Result<&'static str, String> {
    let wanted = provider.trim().to_ascii_lowercase();
    providers
        .iter()
        .find(|candidate| candidate.id == wanted || candidate.aliases.contains(&wanted.as_str()))
        .map(|candidate| candidate.models_url)
        .ok_or_else(|| "unknown provider".to_owned())
}

/// Fire one cheap authenticated GET against the provider's models endpoint with a 5 s timeout.
///
/// `make_client` builds the host's HTTP client for that timeout (the host owns the transport
/// policy: proxy, trust roots, address policy). A client that cannot be built, a timeout, DNS, a
/// refused connection or any non-2xx status other than 401/403 is `Unverified`, never `Rejected`.
/// A blank key is `Rejected` without a request. The key is sent only as a secret `Authorization`
/// header (`rightkit_http::Request::secret_header`, redacted in the request's `Debug`).
pub fn check_key_live(
    providers: &[Provider],
    provider: &str,
    key: &str,
    make_client: impl FnOnce(Duration) -> Result<rightkit_http::Client, String>,
) -> Result<KeyCheckResult, String> {
    let url = models_url(providers, provider)?;
    let key = key.trim();
    if key.is_empty() || key.chars().any(char::is_control) {
        return Ok(KeyCheckResult::Rejected);
    }
    // Validate before attaching any credential. Both guards are static: HTTPS/public
    // host validation plus HTTP's own URL shape/userinfo/origin parser. Resolution
    // remains subject to the host client's address policy on every hop.
    let origins = rightkit_http::ApprovedOrigins::new();
    rightkit_http::validate_public_https_url(url)
        .map_err(|_| "invalid provider check endpoint".to_owned())?;
    rightkit_http::check_redirect_target(url, url, url, &origins)
        .map_err(|_| "invalid provider check endpoint".to_owned())?;
    let Ok(client) = make_client(Duration::from_secs(5)) else {
        return Ok(KeyCheckResult::Unverified);
    };
    let request = rightkit_http::Request::get(url)
        .approved_redirect_origins(origins)
        .timeout(Duration::from_secs(5))
        .secret_header("Authorization", format!("Bearer {key}"));
    match client.send_headers(&request) {
        Ok(head) => Ok(classify_status(head.status)),
        Err(_) => Ok(KeyCheckResult::Unverified),
    }
}

/// Keychain read state used while handing keys to an engine. `label` is safe for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarKeyState {
    Present,
    Absent,
    ReadError,
}

impl SidecarKeyState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::ReadError => "read_error",
        }
    }
}

/// One slot read for an engine hand-off. `value` never leaves the process except through the
/// host's own secret hand-off channel.
pub struct SidecarKey {
    pub value: Option<String>,
    pub state: SidecarKeyState,
}

/// Classify one slot read: a non-blank value is `Present`, a blank one `Absent`, a failure
/// `ReadError` (never "empty").
pub fn classify_key_read(result: Result<String, String>) -> SidecarKey {
    match result {
        Ok(value) if !value.trim().is_empty() => SidecarKey {
            value: Some(value),
            state: SidecarKeyState::Present,
        },
        Ok(_) => SidecarKey {
            value: None,
            state: SidecarKeyState::Absent,
        },
        Err(_) => SidecarKey {
            value: None,
            state: SidecarKeyState::ReadError,
        },
    }
}

/// Trim and length-check a key before it is stored. The error names the provider, never the value.
pub fn validate_key<'a>(display_name: &str, key: &'a str) -> Result<&'a str, String> {
    let key = key.trim();
    if key.len() < 12 {
        return Err(format!("{display_name} API key looks too short"));
    }
    Ok(key)
}

/// One requested slot change. `clear` wins over `key`; a slot with neither is left alone.
pub struct KeyChange {
    pub provider: Provider,
    pub clear: bool,
    pub key: Option<String>,
}

/// Result of applying changes: whether any slot changed, and one message per slot that failed.
/// Messages name the provider and the reason, never a value.
pub struct SetOutcome {
    pub mutated: bool,
    pub failures: Vec<String>,
}

/// The provider slots over one credential namespace. The store is opened per operation, as the
/// host's namespace policy requires.
pub struct KeyVault<O> {
    open_store: O,
    providers: &'static [Provider],
}

impl<O> KeyVault<O>
where
    O: Fn() -> Result<Store, String>,
{
    /// `open_store` opens the host's store for its key namespace (for example
    /// `|| secret_store::credential_store(SERVICE).map_err(|e| e.to_string())`); an `Err` is
    /// reported per slot ("secure storage is not supported on this platform" on a target with no
    /// store).
    pub fn new(open_store: O, providers: &'static [Provider]) -> Self {
        Self {
            open_store,
            providers,
        }
    }

    pub fn providers(&self) -> &'static [Provider] {
        self.providers
    }

    fn open(&self) -> Result<SlotStore, String> {
        let store = (self.open_store)()?;
        SlotStore::new(
            store,
            self.providers
                .iter()
                .map(|provider| Slot::new(provider.id, provider.id))
                .collect(),
        )
        .map_err(|error| error.to_string())
    }

    /// `Ok("")` is an absent (or blank) slot; `Err` is a slot that could not be read.
    fn read_result(user: &str, outcome: SlotOutcome) -> Result<String, String> {
        match outcome.error() {
            Some(error) => Err(format!("could not read {user} API key: {error}")),
            None => Ok(outcome.text().unwrap_or_default().to_owned()),
        }
    }

    /// Every provider once, in table order. `retrying` reads with one retry after 100 ms (a
    /// sidecar restart can race a transient Keychain or Credential Manager read).
    pub fn read_all(&self, retrying: bool) -> Vec<Result<String, String>> {
        let slots = match self.open() {
            Ok(slots) => slots,
            Err(error) => {
                return self
                    .providers
                    .iter()
                    .map(|provider| {
                        Err(format!("could not read {} API key: {error}", provider.id))
                    })
                    .collect();
            }
        };
        let mut snapshot = if retrying {
            slots.read_all_retrying(Duration::from_millis(100))
        } else {
            slots.read_all()
        };
        self.providers
            .iter()
            .map(|provider| match snapshot.take(provider.id) {
                Some(outcome) => Self::read_result(provider.id, outcome),
                None => Err(format!(
                    "could not read {} API key: slot missing from snapshot",
                    provider.id
                )),
            })
            .collect()
    }

    pub fn save(&self, user: &str, key: &str) -> Result<(), String> {
        self.open()
            .map_err(|error| format!("could not save {user} API key: {error}"))?
            .write(user, key)
            .map_err(|error| format!("could not save {user} API key: {error}"))
    }

    pub fn clear(&self, user: &str) -> Result<(), String> {
        self.open()
            .map_err(|error| format!("could not remove {user} API key: {error}"))?
            .clear(user)
            .map_err(|error| format!("could not remove {user} API key: {error}"))
    }

    /// Apply every requested change. A failure on one slot never stops the others: all are
    /// attempted and each failure is reported as `<display name>: <reason>`.
    pub fn apply(&self, changes: Vec<KeyChange>) -> SetOutcome {
        let mut outcome = SetOutcome {
            mutated: false,
            failures: Vec::new(),
        };
        for change in changes {
            let provider = change.provider;
            let result = if change.clear {
                self.clear(provider.id)
            } else if let Some(key) = change.key {
                validate_key(provider.display_name, &key)
                    .and_then(|key| self.save(provider.id, key))
            } else {
                continue;
            };
            match result {
                Ok(()) => outcome.mutated = true,
                Err(error) => outcome
                    .failures
                    .push(format!("{}: {error}", provider.display_name)),
            }
        }
        outcome
    }
}
