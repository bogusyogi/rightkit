//! Provider API keys. Production uses the OS keychain (service = the host app identifier,
//! [`crate::host::HostConfig::app_identifier`]; account = provider id) through
//! `rightkit_credentials::Store`. Key values are never logged or returned by any command.

use rightkit_credentials::{Source, Store, WriteIntent};

use crate::{Context, GenError, GenResult};

/// The keychain service: the host app identifier.
pub fn service() -> &'static str {
    crate::host::config().app_identifier.as_str()
}

/// Holder of the `Store` a `Context` uses instead of the OS keychain (tests inject
/// [`MemoryCredentials`]). The provided methods are conveniences over the store.
pub trait CredentialSource: Send + Sync + std::fmt::Debug {
    fn store(&self) -> &Store;
    fn get(&self, provider: &str) -> GenResult<Option<String>> {
        match self.store().get(provider).map_err(kerr)? {
            Some(s) if !s.is_empty() => Ok(Some(s.expose_str().map_err(kerr)?.to_string())),
            _ => Ok(None),
        }
    }
    fn contains(&self, provider: &str) -> GenResult<bool> {
        self.store().exists(provider).map_err(kerr)
    }
    /// Store a key; an empty key deletes the entry.
    fn set(&self, provider: &str, key: &str) -> GenResult<()> {
        if key.is_empty() {
            self.store().delete(provider).map(|_| ()).map_err(kerr)
        } else {
            self.store().set_str(provider, key).map_err(kerr)
        }
    }
}

fn kerr(e: rightkit_credentials::Error) -> GenError {
    GenError::new("keychain", format!("keychain error: {e}"))
}

/// The injected store, else the host OS store (`RIGHTKIT_NO_KEYCHAIN` denies it).
fn store(ctx: &Context) -> Store {
    match &ctx.credentials {
        Some(c) => c.store().clone(),
        None => crate::host::keychain::store_for(service()),
    }
}

/// Provider id → environment variable names; the first non-empty value wins.
pub const ENV_VARS: &[(&str, &[&str])] = &[
    ("fal", &["FAL_KEY", "FAL_API_KEY"]),
    (
        "elevenlabs",
        &["ELEVENLABS_API_KEY", "ELEVEN_API_KEY", "XI_API_KEY"],
    ),
    (
        "nanobanana",
        &["NANOBANANAAPI_API_KEY", "NANOBANANA_API_KEY"],
    ),
    ("wavespeed", &["WAVESPEED_API_KEY"]),
    ("heygen", &["HEYGEN_API_KEY"]),
    ("openai", &["OPENAI_API_KEY"]),
    ("google", &["GEMINI_API_KEY", "GOOGLE_API_KEY"]),
    ("replicate", &["REPLICATE_API_TOKEN"]),
    ("muapi", &["MUAPI_API_KEY"]),
    ("higgsfield", &["HIGGSFIELD_API_KEY"]),
    ("evolink", &["EVOLINK_API_KEY"]),
    ("pexels", &["PEXELS_API_KEY"]),
    ("pixabay", &["PIXABAY_API_KEY"]),
];

/// Reads one environment variable; injectable so tests never touch the real environment.
pub type EnvReader<'a> = rightkit_credentials::EnvReader<'a>;

fn nonempty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Raw process environment: `Some` (possibly empty) when the variable is set.
fn process_raw(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
}

/// The process environment: a variable set, even to empty, is authoritative (empty means "no key").
fn real_env(name: &str) -> Option<String> {
    nonempty(process_raw(name))
}

/// First non-empty value among the provider's variables.
pub fn env_key(env: EnvReader, provider: &str) -> Option<String> {
    let (_, vars) = ENV_VARS.iter().find(|(id, _)| *id == provider)?;
    vars.iter().find_map(|v| nonempty(env(v)))
}

fn vars_of(provider: &str) -> &'static [&'static str] {
    ENV_VARS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, v)| *v)
        .unwrap_or(&[])
}

pub fn get(ctx: &Context, provider: &str) -> GenResult<Option<String>> {
    get_with_env(ctx, provider, &real_env)
}

/// Keychain first; falls back to the environment when the keychain has no key or is unreachable.
pub fn get_with_env(ctx: &Context, provider: &str, env: EnvReader) -> GenResult<Option<String>> {
    match store(ctx)
        .resolve(provider, vars_of(provider), env)
        .map_err(kerr)?
    {
        Some((s, Source::Store)) if s.is_empty() => Ok(env_key(env, provider)),
        Some((s, _)) => Ok(Some(s.expose_str().map_err(kerr)?.to_string())),
        None => Ok(None),
    }
}

pub fn set(ctx: &Context, provider: &str, key: &str) -> GenResult<()> {
    let s = store(ctx);
    if key.is_empty() {
        s.delete(provider).map(|_| ()).map_err(kerr)
    } else {
        s.set_str(provider, key).map_err(kerr)
    }
}

/// Passive presence check; never retrieves a Keychain password on macOS.
pub fn status(ctx: &Context, provider: &str) -> bool {
    status_source(ctx, provider).is_some()
}

/// Where the key comes from: `"keychain"`, `"environment"`, or none.
pub fn status_source(ctx: &Context, provider: &str) -> Option<&'static str> {
    status_source_with(ctx, provider, &real_env)
}

pub fn status_source_with(ctx: &Context, provider: &str, env: EnvReader) -> Option<&'static str> {
    store(ctx)
        .status_source(provider, vars_of(provider), env)
        .map(|s| match s {
            Source::Store => "keychain",
            Source::Environment => "environment",
        })
}

/// Copy environment keys into the keychain. Names only in the result.
pub fn import_env(ctx: &Context, overwrite: bool) -> GenResult<serde_json::Value> {
    import_env_with(ctx, overwrite, &real_env)
}

pub fn import_env_with(
    ctx: &Context,
    overwrite: bool,
    env: EnvReader,
) -> GenResult<serde_json::Value> {
    let intent = if overwrite {
        WriteIntent::Replace
    } else {
        WriteIntent::CreateOnly
    };
    let r = store(ctx).import_env(ENV_VARS, intent, env).map_err(kerr)?;
    Ok(serde_json::json!({
        "imported": r.imported,
        "skipped_present": r.skipped_present,
        "missing": r.missing,
        "source": "environment",
    }))
}
