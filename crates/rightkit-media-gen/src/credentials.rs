//! Provider API keys. Production uses the OS keychain (service = the host app identifier,
//! [`crate::host::HostConfig::app_identifier`]; account = provider id) through
//! `rightkit_credentials::Store`. Key values are never logged or returned by any command.

#[cfg(any(test, feature = "memory-backend"))]
use std::sync::Arc;

#[cfg(any(test, feature = "memory-backend"))]
use rightkit_credentials::Namespace;
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

/// In-memory `Store` (the published `MemoryBackend`) for tests.
#[cfg(any(test, feature = "memory-backend"))]
pub struct MemoryCredentials {
    store: Store,
}

#[cfg(any(test, feature = "memory-backend"))]
impl std::fmt::Debug for MemoryCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemoryCredentials")
    }
}

#[cfg(any(test, feature = "memory-backend"))]
impl MemoryCredentials {
    pub fn new() -> Self {
        Self::with_backend(Arc::new(rightkit_credentials::MemoryBackend::new()))
    }
    pub fn with_backend(backend: Arc<dyn rightkit_credentials::Backend>) -> Self {
        let ns = Namespace::new(service()).expect("fixed service");
        Self {
            store: Store::open(rightkit_credentials::Config::new(ns).backend(backend)),
        }
    }
}

#[cfg(any(test, feature = "memory-backend"))]
impl Default for MemoryCredentials {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "memory-backend"))]
impl CredentialSource for MemoryCredentials {
    fn store(&self) -> &Store {
        &self.store
    }
}

fn kerr(e: rightkit_credentials::Error) -> GenError {
    GenError::new("keychain", format!("keychain error: {e}"))
}

/// The injected store, else the host OS store (`RIGHTKIT_NO_KEYCHAIN` guards it into memory).
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

#[cfg(any(test, feature = "memory-backend"))]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Arc;

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |n| m.get(n).cloned()
    }

    fn ctx() -> (Context, Arc<MemoryCredentials>) {
        let creds = Arc::new(MemoryCredentials::new());
        (
            Context::new(std::env::temp_dir()).with_credentials(creds.clone()),
            creds,
        )
    }

    #[test]
    fn passive_status_uses_presence() {
        let (ctx, creds) = ctx();
        creds.set("fal", "k").unwrap();
        let env = fake_env(&[]);
        assert_eq!(status_source_with(&ctx, "fal", &env), Some("keychain"));
        assert_eq!(status_source_with(&ctx, "heygen", &env), None);
    }

    #[test]
    fn keychain_service_is_the_host_identifier() {
        assert_eq!(service(), crate::host::config().app_identifier);
    }

    #[test]
    fn import_stores_respects_overwrite_and_reports_missing() {
        let (ctx, creds) = ctx();
        let env = fake_env(&[
            ("FAL_API_KEY", "SECRET-FAL"),
            ("XI_API_KEY", "SECRET-XI"),
            ("HEYGEN_API_KEY", "  "),
        ]);
        creds.set("fal", "OLD-FAL").unwrap();
        let r = import_env_with(&ctx, false, &env).unwrap();
        assert_eq!(r["imported"], serde_json::json!(["elevenlabs"]));
        assert_eq!(r["skipped_present"], serde_json::json!(["fal"]));
        assert_eq!(
            r["missing"],
            serde_json::json!([
                "nanobanana",
                "wavespeed",
                "heygen",
                "openai",
                "google",
                "replicate",
                "muapi",
                "higgsfield",
                "evolink",
                "pexels",
                "pixabay"
            ])
        );
        assert_eq!(r["source"], "environment");
        assert_eq!(creds.get("fal").unwrap().as_deref(), Some("OLD-FAL"));
        assert_eq!(
            creds.get("elevenlabs").unwrap().as_deref(),
            Some("SECRET-XI")
        );
        let r2 = import_env_with(&ctx, true, &env).unwrap();
        assert_eq!(r2["imported"], serde_json::json!(["fal", "elevenlabs"]));
        assert_eq!(creds.get("fal").unwrap().as_deref(), Some("SECRET-FAL"));
        for out in [r.to_string(), r2.to_string()] {
            assert!(!out.contains("SECRET") && !out.contains("OLD-FAL"));
        }
    }

    #[test]
    fn stock_provider_keys_are_registered_with_env_fallback() {
        for (id, var) in [("pexels", "PEXELS_API_KEY"), ("pixabay", "PIXABAY_API_KEY")] {
            assert!(
                ENV_VARS.iter().any(|(p, v)| *p == id && v.contains(&var)),
                "{id} is registered"
            );
        }
        let (ctx, _creds) = ctx();
        let env = fake_env(&[("PEXELS_API_KEY", "SECRET-PX"), ("PIXABAY_API_KEY", "  ")]);
        assert_eq!(
            status_source_with(&ctx, "pexels", &env),
            Some("environment")
        );
        assert_eq!(
            get_with_env(&ctx, "pexels", &env).unwrap().as_deref(),
            Some("SECRET-PX")
        );
        assert_eq!(
            status_source_with(&ctx, "pixabay", &env),
            None,
            "a blank variable is no key"
        );
        assert_eq!(get_with_env(&ctx, "pixabay", &env).unwrap(), None);
    }

    #[test]
    fn stock_keys_import_from_environment_into_keychain() {
        let (ctx, creds) = ctx();
        let env = fake_env(&[("PIXABAY_API_KEY", "SECRET-PB")]);
        let r = import_env_with(&ctx, false, &env).unwrap();
        assert_eq!(r["imported"], serde_json::json!(["pixabay"]));
        assert_eq!(creds.get("pixabay").unwrap().as_deref(), Some("SECRET-PB"));
        assert!(!r.to_string().contains("SECRET"));
    }

    #[test]
    fn first_nonempty_var_wins() {
        let env = fake_env(&[("FAL_KEY", ""), ("FAL_API_KEY", "b")]);
        assert_eq!(env_key(&env, "fal").as_deref(), Some("b"));
        assert_eq!(env_key(&env, "nope"), None);
    }

    #[test]
    fn status_source_and_fallback() {
        let (ctx, creds) = ctx();
        let env = fake_env(&[
            ("WAVESPEED_API_KEY", "SECRET-WS"),
            ("FAL_KEY", "SECRET-ENV-FAL"),
        ]);
        creds.set("fal", "SECRET-KC").unwrap();
        assert_eq!(status_source_with(&ctx, "fal", &env), Some("keychain"));
        assert_eq!(
            status_source_with(&ctx, "wavespeed", &env),
            Some("environment")
        );
        assert_eq!(status_source_with(&ctx, "heygen", &env), None);
        assert_eq!(
            get_with_env(&ctx, "fal", &env).unwrap().as_deref(),
            Some("SECRET-KC")
        );
        assert_eq!(
            get_with_env(&ctx, "wavespeed", &env).unwrap().as_deref(),
            Some("SECRET-WS")
        );
        assert_eq!(get_with_env(&ctx, "heygen", &env).unwrap(), None);
    }

    // M11 A.7 (a): imported, verified, marker written, the environment reader is only read.
}
