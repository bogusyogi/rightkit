//! Keychain-backed [`rightkit_oauth::TokenStore`] for the subscription sign-ins (P-3).
//!
//! RightKit owns the refresh, the cross-process refresh lock and the revoked-grant cleanup
//! (`rightkit_oauth::credentials::Resolver`). Its own `SecretTokenStore` is not used: it binds
//! `rightkit-credentials` 0.2 and writes one JSON blob per account, and a ChatGPT access
//! token plus its refresh token exceed Windows Credential Manager's 2,560-byte blob limit
//! (and `rightkit-credentials` 0.3 does not chunk). This adapter wraps the 0.3 [`Store`]
//! the host app already pins.
//!
//! Layout, per provider `p` (the `Resolver` account):
//!
//! - `p.access`  : the access token (committed last on save, deleted first on delete, so its
//!   presence means "signed in")
//! - `p.refresh` : the refresh token, when the provider issued one
//! - `p.meta`    : small JSON, `type` / `expires_in` / `scope` / `obtained_at`
//!
//! A value longer than `MAX_CHUNK_UTF16` UTF-16 units is split on Windows into generation-
//! tagged chunks (`<account>.c.<generation>.<i>`) behind a manifest stored in the account
//! itself, so a 4 KB access token fits. The OpenID Connect `id_token` is never stored.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rightkit_credentials::Store;
use rightkit_http::Secret;
use rightkit_oauth::credentials::Resolver;
use rightkit_oauth::{OAuthClient, OAuthConfig, OAuthError, TokenSet, TokenStore};

use crate::agent_providers::credential_broker::SubscriptionLeg;
use crate::agent_providers::keystore::KeyStoreError;
use crate::agent_providers::subscription_auth::eligibility::DEFAULT_OAUTH_MINIMUM_VALIDITY_MS;
use crate::agent_providers::subscription_auth::native_oauth::{
    commandcode_oauth_config, openai_oauth_config, CommandCodeOAuth, OpenAICodexOAuth,
    SubscriptionLogin,
};

/// Keychain service of the subscription sign-ins.
pub const SUBSCRIPTION_KEYCHAIN_SERVICE: &str = "rightkit-agent.subscription_auth";

// Windows credential blobs are capped at 2,560 bytes; 1,000 UTF-16 units (two bytes each at
// worst) leaves margin below that native limit.
const MAX_CHUNK_UTF16: usize = 1000;
const MAX_CHUNKS: usize = 64;
const CHUNK_PREFIX: &str = "__rightkit_token_chunks_v1:";

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    generation: String,
    chunks: usize,
}

fn storage(error: impl std::fmt::Display) -> OAuthError {
    OAuthError::Storage(error.to_string())
}

fn parse_manifest(raw: &str) -> Option<Manifest> {
    let manifest: Manifest = serde_json::from_str(raw.strip_prefix(CHUNK_PREFIX)?).ok()?;
    (manifest.chunks > 0
        && manifest.chunks <= MAX_CHUNKS
        && !manifest.generation.is_empty()
        && manifest.generation.len() <= 64
        && manifest
            .generation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-'))
    .then_some(manifest)
}

fn chunk_account(account: &str, generation: &str, index: usize) -> String {
    format!("{account}.c.{generation}.{index}")
}

fn generation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn split_utf16(raw: &str) -> Option<Vec<String>> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for ch in raw.chars() {
        let n = ch.len_utf16();
        if units + n > MAX_CHUNK_UTF16 {
            if chunks.len() == MAX_CHUNKS {
                return None;
            }
            chunks.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(ch);
        units += n;
    }
    if !current.is_empty() {
        if chunks.len() == MAX_CHUNKS {
            return None;
        }
        chunks.push(current);
    }
    Some(chunks)
}

/// Only Windows has the blob limit; other platforms store the value whole.
fn chunk_limit() -> usize {
    if cfg!(windows) {
        MAX_CHUNK_UTF16
    } else {
        usize::MAX
    }
}

fn get_string(store: &Store, account: &str) -> Result<Option<String>, OAuthError> {
    match store.get(account).map_err(storage)? {
        None => Ok(None),
        Some(secret) => secret
            .expose_str()
            .map(|value| Some(value.to_owned()))
            .map_err(storage),
    }
}

fn read_chunks(
    store: &Store,
    manifest: &Manifest,
    account_of: impl Fn(&str, usize) -> String,
) -> Result<String, OAuthError> {
    let mut full = String::new();
    for index in 0..manifest.chunks {
        let chunk = get_string(store, &account_of(&manifest.generation, index))?
            .ok_or_else(|| storage("a stored credential chunk is missing"))?;
        full.push_str(&chunk);
    }
    Ok(full)
}

/// Read one logical value, following a chunk manifest. A writer replacing the value between
/// the manifest read and the chunk reads is retried once against the fresh manifest.
fn read_value(store: &Store, account: &str) -> Result<Option<String>, OAuthError> {
    let Some(raw) = get_string(store, account)? else {
        return Ok(None);
    };
    let Some(manifest) = parse_manifest(&raw) else {
        return Ok(Some(raw));
    };
    let chunk_of = |generation: &str, index: usize| chunk_account(account, generation, index);
    match read_chunks(store, &manifest, chunk_of) {
        Ok(full) => Ok(Some(full)),
        Err(first) => {
            let latest = get_string(store, account)?;
            match latest {
                None => Ok(None),
                Some(latest) if latest == raw => Err(first),
                Some(latest) => match parse_manifest(&latest) {
                    Some(manifest) => read_chunks(store, &manifest, chunk_of).map(Some),
                    None => Ok(Some(latest)),
                },
            }
        }
    }
}

fn remove_chunks(store: &Store, account: &str, manifest: &Manifest) {
    for index in 0..manifest.chunks {
        let _ = store.delete(&chunk_account(account, &manifest.generation, index));
    }
}

fn current_manifest(store: &Store, account: &str) -> Option<Manifest> {
    get_string(store, account)
        .ok()
        .flatten()
        .and_then(|raw| parse_manifest(&raw))
}

/// Write one logical value. Chunks first, the manifest (the commit point) last; the previous
/// generation's chunks are removed only after the new value is in place.
fn write_value(store: &Store, account: &str, value: &str) -> Result<(), OAuthError> {
    // Interactive on purpose, as the retired store was: replacing an item whose ACL belongs to
    // an older build REQUIRES macOS to ask the user to authorize it.
    let writer = store.interactive();
    let previous = current_manifest(store, account);
    if value.encode_utf16().count() <= chunk_limit() {
        writer.set_str(account, value).map_err(storage)?;
    } else {
        let chunks =
            split_utf16(value).ok_or_else(|| storage("credential exceeds secure storage limit"))?;
        let generation = generation_id();
        let rollback = |written: usize| {
            for index in 0..written {
                let _ = store.delete(&chunk_account(account, &generation, index));
            }
        };
        for (index, chunk) in chunks.iter().enumerate() {
            if let Err(error) = writer.set_str(&chunk_account(account, &generation, index), chunk) {
                rollback(index);
                return Err(storage(error));
            }
        }
        let manifest = format!(
            "{CHUNK_PREFIX}{}",
            serde_json::to_string(&Manifest {
                generation: generation.clone(),
                chunks: chunks.len(),
            })
            .map_err(storage)?
        );
        if let Err(error) = writer.set_str(account, &manifest) {
            rollback(chunks.len());
            return Err(storage(error));
        }
    }
    if let Some(previous) = previous {
        remove_chunks(store, account, &previous);
    }
    Ok(())
}

fn delete_value(store: &Store, account: &str) -> Result<(), OAuthError> {
    let previous = current_manifest(store, account);
    store.delete(account).map_err(storage)?;
    if let Some(previous) = previous {
        remove_chunks(store, account, &previous);
    }
    Ok(())
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn encode_meta(tokens: &TokenSet) -> String {
    serde_json::json!({
        "v": 1,
        "type": tokens.token_type,
        "expires_in": tokens.expires_in.map(|d| d.as_secs()),
        "scope": tokens.scope,
        "obtained_at": unix_secs(tokens.obtained_at),
    })
    .to_string()
}

/// Rebuild the tokens from the three stored values. A missing or unreadable `meta` is an
/// unknown lifetime and is treated as already expired: the refresh token (if any) renews it,
/// otherwise the user signs in again. Never trust an unknown lifetime as valid.
fn assemble(access: String, refresh: Option<String>, meta: Option<String>) -> TokenSet {
    let meta: Option<serde_json::Value> = meta.and_then(|raw| serde_json::from_str(&raw).ok());
    let known = meta.as_ref();
    TokenSet {
        access_token: Secret::new(access),
        token_type: known
            .and_then(|m| m.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("Bearer")
            .to_string(),
        expires_in: match known {
            Some(m) => m
                .get("expires_in")
                .and_then(|v| v.as_u64())
                .map(Duration::from_secs),
            None => Some(Duration::ZERO),
        },
        refresh_token: refresh.filter(|value| !value.is_empty()).map(Secret::new),
        scope: known
            .and_then(|m| m.get("scope"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        id_token: None,
        obtained_at: UNIX_EPOCH
            + Duration::from_secs(
                known
                    .and_then(|m| m.get("obtained_at"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            ),
    }
}

/// [`TokenStore`] over a `rightkit-credentials` 0.3 [`Store`]. See the module docs.
pub struct KeychainTokenStore(Store);

impl KeychainTokenStore {
    pub fn new(store: Store) -> Self {
        Self(store)
    }

    fn account(provider: &str, part: &str) -> String {
        format!("{provider}.{part}")
    }

    fn load_current(&self, provider: &str) -> Result<Option<TokenSet>, OAuthError> {
        let Some(access) = read_value(&self.0, &Self::account(provider, "access"))? else {
            return Ok(None);
        };
        let refresh = read_value(&self.0, &Self::account(provider, "refresh"))?;
        let meta = read_value(&self.0, &Self::account(provider, "meta")).unwrap_or(None);
        Ok(Some(assemble(access, refresh, meta)))
    }
}

impl std::fmt::Debug for KeychainTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeychainTokenStore")
            .field("namespace", &self.0.namespace().service())
            .finish()
    }
}

impl TokenStore for KeychainTokenStore {
    fn load(&self, account: &str) -> Result<Option<TokenSet>, OAuthError> {
        self.load_current(account)
    }

    fn save(&self, account: &str, tokens: &TokenSet) -> Result<(), OAuthError> {
        // Commit order: meta, refresh, then access last. A reader sees "signed in" only once
        // the access token is in place; `id_token` is never stored.
        write_value(
            &self.0,
            &Self::account(account, "meta"),
            &encode_meta(tokens),
        )?;
        let refresh_account = Self::account(account, "refresh");
        match &tokens.refresh_token {
            Some(refresh) => write_value(&self.0, &refresh_account, refresh.expose())?,
            None => delete_value(&self.0, &refresh_account)?,
        }
        write_value(
            &self.0,
            &Self::account(account, "access"),
            tokens.access_token(),
        )
    }

    fn delete(&self, account: &str) -> Result<(), OAuthError> {
        // Access first so the sign-in disappears at once.
        delete_value(&self.0, &Self::account(account, "access"))?;
        delete_value(&self.0, &Self::account(account, "refresh"))?;
        delete_value(&self.0, &Self::account(account, "meta"))?;
        Ok(())
    }
}

/// Open the OS credential store for the subscription sign-ins. Real-keychain access is
/// suppressed under `cargo test` and by `RIGHTKIT_DISABLE_OS_KEYCHAIN=1`
/// (see `crate::agent_providers::keystore`).
pub fn open_subscription_store() -> Result<Store, KeyStoreError> {
    crate::agent_providers::keystore::secret_store(SUBSCRIPTION_KEYCHAIN_SERVICE)
}

fn oauth_client(config: OAuthConfig) -> Result<OAuthClient, OAuthError> {
    let http = rightkit_http::ClientConfig {
        connect_timeout: Some(Duration::from_secs(10)),
        // The retired refresh was bounded at 15 s.
        request_timeout: Duration::from_secs(15),
        // A token endpoint is a credential boundary: never follow a redirect with a refresh token.
        max_redirects: 0,
        // One attempt, like the retired reqwest refresh; the caller decides about retrying.
        retry: rightkit_http::RetryPolicy::none(),
        // The same network path as the provider inference client (environment proxy honoured).
        proxy: rightkit_http::ProxyMode::Environment,
        ..Default::default()
    };
    OAuthClient::new(config, rightkit_http::Client::new(http))
}

fn build_resolver(
    provider: &str,
    tokens: Arc<dyn TokenStore>,
    lock_dir: PathBuf,
) -> Option<Resolver> {
    let config = match provider {
        "openai" => openai_oauth_config(),
        "commandcode" => commandcode_oauth_config(),
        _ => return None,
    };
    let client = oauth_client(config).ok()?;
    Some(
        Resolver::new(client, tokens, provider)
            .lock_dir(lock_dir)
            .refresh_skew(Duration::from_millis(
                DEFAULT_OAUTH_MINIMUM_VALIDITY_MS as u64,
            )),
    )
}

/// The RightKit `Resolver` for a subscription provider (`openai`, `commandcode`), over the
/// keychain adapter. `lock_dir` is the cross-process refresh-lock directory
/// (`<state root>/locks/oauth`). `Resolver::resolve` blocks: async callers run it on
/// `tokio::task::spawn_blocking`.
pub fn subscription_resolver(provider: &str, store: Store, lock_dir: PathBuf) -> Option<Resolver> {
    build_resolver(provider, Arc::new(KeychainTokenStore::new(store)), lock_dir)
}

/// The broker's subscription leg for a provider: the [`subscription_resolver`], the same token
/// store (for status), and the provider's login flow.
pub fn subscription_leg(
    provider: &str,
    store: Store,
    lock_dir: PathBuf,
) -> Option<SubscriptionLeg> {
    let tokens: Arc<dyn TokenStore> = Arc::new(KeychainTokenStore::new(store));
    let resolver = build_resolver(provider, tokens.clone(), lock_dir)?;
    let flow: Arc<dyn SubscriptionLogin> = match provider {
        "openai" => Arc::new(OpenAICodexOAuth::default()),
        "commandcode" => Arc::new(CommandCodeOAuth::default()),
        _ => return None,
    };
    Some(SubscriptionLeg::new(resolver, tokens, flow, provider))
}
