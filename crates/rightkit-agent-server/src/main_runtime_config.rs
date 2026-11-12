/// Stable per-user data directory for the daemon DB. NOT the OS temp dir:
/// temp is (a) shared by every daemon instance — concurrent daemons collide on
/// one SQLite file and the second fails to open it ("Retry engine"), and (b)
/// periodically cleaned — users would lose all memory/sessions. Resolves the
/// per-user data dir (`%LOCALAPPDATA%` on Windows, `~/Library/Application
/// Support` on macOS, `$XDG_DATA_HOME`/`~/.local/share` on Linux) under a
/// `RightKit` subdir, creating it. Falls back to temp only if none resolve.
pub(crate) fn default_db_path() -> String {
    // A QA/test build with an isolated state root keeps the database inside it; a release
    // build never has one.
    let isolated_root = if rightkit_agent_config::state_root_is_isolated() {
        rightkit_agent_config::state_root()
    } else {
        None
    };
    let dir = default_db_dir(isolated_root, user_data_dir(), std::env::temp_dir());
    if rightkit_fs::ensure_private_dir(&dir).is_err() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir.join(format!("{}.db", crate::host_identity::app()))
        .display()
        .to_string()
}

fn default_db_dir(
    isolated_root: Option<std::path::PathBuf>,
    user_data: Option<std::path::PathBuf>,
    temp: std::path::PathBuf,
) -> std::path::PathBuf {
    match isolated_root {
        Some(root) => root,
        None => user_data.unwrap_or(temp).join("RightKit"),
    }
}

/// Atomic state-file write (`rightkit-fs`: temp file, flush, rename; parents created).
fn write_state_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    rightkit_fs::atomic_write_with(
        path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |file| std::io::Write::write_all(file, bytes),
    )
    .map_err(|e| e.to_string())
}

fn user_data_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| {
            std::path::PathBuf::from(h)
                .join("Library")
                .join("Application Support")
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
            })
    }
}

fn runtime_middleware_store_path(db_path: &str) -> std::path::PathBuf {
    let db_path = std::path::PathBuf::from(db_path);
    db_path
        .parent()
        .map(|parent| parent.join("runtime-middleware.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("runtime-middleware.json"))
}

fn recipe_store_path(db_path: &str) -> std::path::PathBuf {
    let db_path = std::path::PathBuf::from(db_path);
    db_path
        .parent()
        .map(|parent| parent.join("recipes.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("recipes.json"))
}

fn skillopt_store_path(db_path: &str) -> std::path::PathBuf {
    let db_path = std::path::PathBuf::from(db_path);
    db_path
        .parent()
        .map(|parent| parent.join("skillopt-runs.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("skillopt-runs.json"))
}

fn spec_store_path(db_path: &str) -> std::path::PathBuf {
    let db_path = std::path::PathBuf::from(db_path);
    db_path
        .parent()
        .map(|parent| parent.join("feature-specs.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("feature-specs.json"))
}

/// The user's last explicit model selection, remembered across launches.
/// There is no built-in default model; until one is selected the TUI asks.
pub(crate) fn model_selection_path() -> Option<std::path::PathBuf> {
    let root = rightkit_agent_config::state_root()?;
    Some(root.join("state").join("model-selection.json"))
}

/// `(provider, model)` from the remembered selection, if one was saved.
pub(crate) fn load_model_selection() -> Option<(String, String)> {
    let text = std::fs::read_to_string(model_selection_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let provider = value.get("provider")?.as_str()?.trim().to_string();
    let model = value.get("model")?.as_str()?.trim().to_string();
    (!provider.is_empty() && !model.is_empty()).then_some((provider, model))
}

/// The remembered reasoning effort that goes with the remembered model, if
/// one was ever chosen (`low|medium|high`).
pub(crate) fn load_model_selection_effort() -> Option<String> {
    let text = std::fs::read_to_string(model_selection_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    parse_selection_effort(&value)
}

fn parse_selection_effort(value: &serde_json::Value) -> Option<String> {
    let effort = value.get("effort")?.as_str()?.trim().to_ascii_lowercase();
    matches!(effort.as_str(), "low" | "medium" | "high").then_some(effort)
}

/// Remember an explicit `(provider, model)` selection for the next launch.
pub(crate) fn save_model_selection(provider: &str, model: &str) -> Result<(), String> {
    save_model_selection_with_effort(provider, model, None)
}

/// Remember `(provider, model)` plus the committed effort so the footer and
/// the next launch agree on `model · effort`.
pub(crate) fn save_model_selection_with_effort(
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> Result<(), String> {
    let path = model_selection_path().ok_or("no home directory for model selection")?;
    let mut body = serde_json::json!({ "provider": provider, "model": model });
    if let Some(effort) = effort.filter(|e| !e.trim().is_empty()) {
        body["effort"] = serde_json::Value::String(effort.trim().to_ascii_lowercase());
    }
    write_state_file(&path, body.to_string().as_bytes())
}

/// Effort a launch starts on: the remembered one, else the configured
/// session default. Only the closed `low|medium|high` set is accepted.
pub(crate) fn launch_effort() -> Option<String> {
    load_model_selection_effort().or_else(|| {
        let configured = load_current_merged_config().session.effort;
        let configured = configured.trim().to_ascii_lowercase();
        matches!(configured.as_str(), "low" | "medium" | "high").then_some(configured)
    })
}

/// Per-provider model visibility the `/providers` picker toggles: which
/// catalog models `/model` hides. Shares the `~/.{app}/state` directory
/// with `model-selection.json`.
fn model_visibility_path() -> Option<std::path::PathBuf> {
    model_selection_path()
        .and_then(|path| path.parent().map(|dir| dir.join("model-visibility.json")))
}

/// Parse the `{ "<provider>": { "hidden": [...] } }` file body into the
/// visibility map. Malformed or partial bodies degrade to "nothing
/// hidden" row by row rather than failing the whole file.
fn parse_model_visibility(body: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut map = std::collections::BTreeMap::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return map;
    };
    let Some(object) = value.as_object() else {
        return map;
    };
    for (provider, entry) in object {
        let Some(ids) = entry.get("hidden").and_then(serde_json::Value::as_array) else {
            continue;
        };
        let hidden: Vec<String> = ids
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect();
        if !hidden.is_empty() {
            map.insert(provider.clone(), hidden);
        }
    }
    map
}

/// Render the visibility map in the `{ "<provider>": { "hidden": [...] } }`
/// shape. Providers with an empty hidden list are omitted so the file
/// stays meaningful to read; ids are sorted for a stable file.
fn render_model_visibility(map: &std::collections::BTreeMap<String, Vec<String>>) -> String {
    let mut object = serde_json::Map::new();
    for (provider, hidden) in map {
        if hidden.is_empty() {
            continue;
        }
        let mut ids = hidden.clone();
        ids.sort();
        ids.dedup();
        object.insert(provider.clone(), serde_json::json!({ "hidden": ids }));
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(object)).unwrap_or_default()
}

/// The persisted visibility map: provider id → hidden model ids.
/// `BTreeMap` keeps the file stable across rewrites; a missing or
/// malformed file means "nothing hidden".
pub(crate) fn load_model_visibility() -> std::collections::BTreeMap<String, Vec<String>> {
    let Some(path) = model_visibility_path() else {
        return Default::default();
    };
    load_model_visibility_at(&path)
}

fn load_model_visibility_at(
    path: &std::path::Path,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    parse_model_visibility(&text)
}

/// Write the whole visibility map. The file is rewritten per toggle —
/// the map is a handful of rows.
fn save_model_visibility_at(
    path: &std::path::Path,
    map: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    write_state_file(path, render_model_visibility(map).as_bytes())
}

/// Flip one model's hidden flag for a provider and persist the whole map.
/// The ONE visibility writer: the `/providers` screen and
/// `{app} provider set-visibility` both call this.
pub(crate) fn set_model_hidden(provider: &str, model: &str, hidden: bool) -> Result<(), String> {
    let path = model_visibility_path().ok_or("no home directory for model visibility")?;
    set_model_hidden_at(&path, provider, model, hidden)
}

fn set_model_hidden_at(
    path: &std::path::Path,
    provider: &str,
    model: &str,
    hidden: bool,
) -> Result<(), String> {
    let mut map = load_model_visibility_at(path);
    {
        let entry = map.entry(provider.to_string()).or_default();
        entry.retain(|id| id != model);
        if hidden {
            entry.push(model.to_string());
        }
    }
    if map.get(provider).is_some_and(|ids| ids.is_empty()) {
        map.remove(provider);
    }
    save_model_visibility_at(path, &map)
}

pub(crate) fn load_current_merged_config() -> rightkit_agent_config::HostConfig {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    load_merged_config_for_workspace(&cwd, &env_vars)
}

fn web_search_config_from_config(
    config: &rightkit_agent_config::HostConfig,
) -> Option<rightkit_agent_tools::coding_tools::WebSearchConfig> {
    let provider = config
        .tools
        .web_search_provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let tinyfish_available = rightkit_llm::agent_providers::load_api_key("tinyfish")
                .is_some()
                || std::env::var("TINYFISH_API_KEY").is_ok();
            tinyfish_available.then(|| "tinyfish".to_string())
        })?;
    let api_key = config
        .tools
        .web_search_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    if provider != "tinyfish" && api_key.is_empty() {
        return None;
    }
    Some(rightkit_agent_tools::coding_tools::WebSearchConfig {
        provider,
        api_key: api_key.to_string(),
    })
}

pub(crate) fn resolve_engine_config(
    config: &rightkit_agent_config::HostConfig,
    requested_model: Option<&str>,
    requested_provider: Option<&str>,
) -> Option<api::EngineConfig> {
    // `--model provider::model` (the picker's route-qualified selection id)
    // names one route; the bare model id is what the provider and the model
    // catalog know.
    if requested_provider
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_none()
    {
        if let Some((provider_id, model)) = requested_model.and_then(|m| m.trim().split_once("::"))
        {
            let known = config.provider_routes.contains_key(provider_id)
                || rightkit_llm::agent_providers::find_provider(provider_id).is_some();
            if known && !model.is_empty() {
                return resolve_engine_config(config, Some(model), Some(provider_id));
            }
        }
    }
    if let Some(provider_id) = requested_provider
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(route) = config.provider_routes.get(provider_id) {
            return engine_config_from_configured_route(provider_id, route, requested_model);
        }
        let provider = rightkit_llm::agent_providers::find_provider(provider_id)?;
        // OpenAI's catalog route is authenticated by the native ChatGPT OAuth
        // broker in `provider_client`; it must not be rejected because no API
        // key exists in the legacy key store.
        let key = if provider_id.eq_ignore_ascii_case("openai") {
            rightkit_llm::agent_providers::load_api_key(provider_id).unwrap_or_default()
        } else {
            rightkit_llm::agent_providers::load_api_key(provider_id)?
        };
        return Some(api::EngineConfig {
            api_key: key,
            // No built-in default model: run only an explicit selection.
            model_id: requested_model
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_string(),
            base_url: provider.base_url.to_string(),
            fallback_models: config.model.fallback_models.clone(),
        });
    }

    let model = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let model = config.model.default_model.trim();
            (!model.is_empty()).then_some(model)
        });
    if let Some(model) = model {
        if let Some((provider_id, route)) = config
            .provider_routes
            .iter()
            .find(|(_, route)| route.model.trim().eq_ignore_ascii_case(model))
        {
            return engine_config_from_configured_route(provider_id, route, Some(model));
        }
    }

    let model = model?;
    let (provider, key) = select_keyed_catalog_provider(
        DEFAULT_HOSTED_PROVIDER_PRIORITY.iter().copied(),
        rightkit_llm::agent_providers::load_api_key,
    )?;
    Some(api::EngineConfig {
        api_key: key,
        model_id: model.to_string(),
        base_url: provider.base_url.to_string(),
        fallback_models: config.model.fallback_models.clone(),
    })
}

const DEFAULT_HOSTED_PROVIDER_PRIORITY: &[&str] = &["openrouter"];

fn select_keyed_catalog_provider<'a, I, F>(
    provider_ids: I,
    mut load_key: F,
) -> Option<(&'static rightkit_llm::agent_providers::ProviderInfo, String)>
where
    I: IntoIterator<Item = &'a str>,
    F: FnMut(&str) -> Option<String>,
{
    for provider_id in provider_ids {
        let Some(provider) = rightkit_llm::agent_providers::find_provider(provider_id) else {
            continue;
        };
        let Some(key) = load_key(provider_id) else {
            continue;
        };
        return Some((provider, key));
    }
    None
}

/// Resolve the default provider id the same way [`resolve_engine_config`]
/// does when no explicit provider is requested: a configured route whose
/// model matches the configured default model first, then the first hosted
/// provider with a stored key, then the hosted default. Callers that only
/// need a provider *id* (not a full engine config) — such as the bare
/// interactive REPL — use this instead of hardcoding one.
fn resolve_default_provider_id(config: &rightkit_agent_config::HostConfig) -> String {
    let default_model = config.model.default_model.trim();
    if !default_model.is_empty() {
        if let Some((provider_id, _)) = config
            .provider_routes
            .iter()
            .find(|(_, route)| route.model.trim().eq_ignore_ascii_case(default_model))
        {
            return provider_id.clone();
        }
    }
    if let Some((provider, _)) = select_keyed_catalog_provider(
        DEFAULT_HOSTED_PROVIDER_PRIORITY.iter().copied(),
        rightkit_llm::agent_providers::load_api_key,
    ) {
        return provider.id.to_string();
    }
    DEFAULT_HOSTED_PROVIDER_PRIORITY[0].to_string()
}

/// Same resolution as [`resolve_default_provider_id`], but returns `None`
/// instead of the unconditional last-resort fallback (the hosted default
/// provider id with no stored key) when nothing is actually configured.
/// KRN defect #1: a fallback id must never be treated as "a provider is
/// configured" -- callers that gate startup truthfulness on that
/// distinction (the bare interactive session, after onboarding) use this
/// instead of `resolve_default_provider_id`, whose fallback return value
/// stays unchanged for its own existing callers.
fn resolve_default_provider_id_checked(
    config: &rightkit_agent_config::HostConfig,
) -> Option<String> {
    let default_model = config.model.default_model.trim();
    if !default_model.is_empty() {
        if let Some((provider_id, _)) = config
            .provider_routes
            .iter()
            .find(|(_, route)| route.model.trim().eq_ignore_ascii_case(default_model))
        {
            return Some(provider_id.clone());
        }
    }
    select_keyed_catalog_provider(
        DEFAULT_HOSTED_PROVIDER_PRIORITY.iter().copied(),
        rightkit_llm::agent_providers::load_api_key,
    )
    .map(|(provider, _)| provider.id.to_string())
}

/// How the configured route selected by the default model would authenticate
/// a turn, classified from [`engine_config_from_configured_route`] — the same
/// resolution [`resolve_engine_config`] uses when no provider is requested.
/// `None` when no route matches the default model (a turn then falls back to
/// the hosted default). The resolved credential never leaves this function;
/// `{app} doctor` reports presence only.
pub(crate) fn default_route_auth(
    config: &rightkit_agent_config::HostConfig,
) -> Option<(String, crate::doctor::RouteAuth)> {
    let default_model = config.model.default_model.trim();
    if default_model.is_empty() {
        return None;
    }
    let (provider_id, route) = config
        .provider_routes
        .iter()
        .find(|(_, route)| route.model.trim().eq_ignore_ascii_case(default_model))?;
    let base_url = route.base_url.trim().trim_end_matches('/');
    let native_openai = rightkit_llm::agent_providers::find_provider_by_base_url(base_url)
        .is_some_and(|provider| provider.id.eq_ignore_ascii_case("openai"));
    let auth = match engine_config_from_configured_route(provider_id, route, Some(default_model)) {
        None if base_url.is_empty() => crate::doctor::RouteAuth::MissingBaseUrl,
        None => crate::doctor::RouteAuth::MissingKey,
        Some(engine) if !engine.api_key.is_empty() => crate::doctor::RouteAuth::Key,
        Some(_) if native_openai => crate::doctor::RouteAuth::ChatGptSignIn,
        Some(_) => crate::doctor::RouteAuth::NotRequired,
    };
    Some((provider_id.clone(), auth))
}

fn engine_config_from_configured_route(
    provider_id: &str,
    route: &rightkit_agent_config::ProviderRouteConfig,
    requested_model: Option<&str>,
) -> Option<api::EngineConfig> {
    let base_url = route.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return None;
    }
    let model_id = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let model = route.model.trim();
            (!model.is_empty()).then_some(model)
        })?
        .to_string();
    let api_key = configured_route_api_key(
        provider_id,
        route,
        |env_name| std::env::var(env_name).ok(),
        rightkit_llm::agent_providers::load_api_key,
    );
    let native_openai_route = rightkit_llm::agent_providers::find_provider_by_base_url(base_url)
        .map(|provider| provider.id.eq_ignore_ascii_case("openai"))
        .unwrap_or(false);
    let api_key = match (native_openai_route, route.api_key_required, api_key) {
        // Native ChatGPT OAuth is resolved later by `build_checked_client`.
        (true, _, None) => String::new(),
        (_, _, Some(key)) => key,
        (_, true, None) => return None,
        (_, false, None) => String::new(),
    };
    Some(api::EngineConfig {
        api_key,
        model_id,
        base_url: base_url.to_string(),
        fallback_models: Vec::new(),
    })
}

/// Fail-fast check for an explicitly selected configured route
/// (`--provider X` or `--model X::model`): a route declared
/// `api-key-required = true` with nothing resolvable must stop the run before
/// any session or provider request, never fall through to the default route.
/// Returns the failure message, or `None` when the route is usable or is not a
/// configured key-required route.
pub(crate) fn missing_required_route_key(
    workspace: &std::path::Path,
    model: Option<&str>,
    provider: Option<&str>,
) -> Option<String> {
    let provider_id = match provider.map(str::trim).filter(|value| !value.is_empty()) {
        Some(provider) => provider,
        None => model?.trim().split_once("::")?.0,
    };
    let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let config = load_merged_config_for_workspace(workspace, &env_vars);
    let route = config.provider_routes.get(provider_id)?;
    let base_url = route.base_url.trim().trim_end_matches('/');
    if !route.api_key_required || base_url.is_empty() {
        return None;
    }
    let native_openai = rightkit_llm::agent_providers::find_provider_by_base_url(base_url)
        .is_some_and(|provider| provider.id.eq_ignore_ascii_case("openai"));
    if native_openai {
        return None;
    }
    configured_route_api_key(
        provider_id,
        route,
        |env_name| std::env::var(env_name).ok(),
        rightkit_llm::agent_providers::load_api_key,
    )
    .is_none()
    .then(|| format!("provider route `{provider_id}` requires an API key and none is configured"))
}

fn configured_route_api_key<EnvLookup, SavedLookup>(
    provider_id: &str,
    route: &rightkit_agent_config::ProviderRouteConfig,
    env_lookup: EnvLookup,
    mut saved_lookup: SavedLookup,
) -> Option<String>
where
    EnvLookup: FnMut(&str) -> Option<String>,
    SavedLookup: FnMut(&str) -> Option<String>,
{
    route
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(env_lookup)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| saved_lookup(provider_id))
}

/// The explicit `provider-routes.*` rows that resolve to a usable engine
/// route through the same authority [`resolve_engine_config`] consults —
/// [`engine_config_from_configured_route`]. Each admitted row is the
/// `(provider_id, configured model)` pair the interactive model picker
/// offers: explicit configured routes are a route authority this product
/// already owns, so the configured model is a real selectable route,
/// while rows the engine cannot resolve (empty base-url/model, or a
/// required credential that is absent) are never offered. Credential
/// resolution uses the existing credential resolver. Native OpenAI routes
/// remain owned by the broker's account catalog. Sorted for stable order.
fn configured_catalog_routes(config: &rightkit_agent_config::HostConfig) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = config
        .provider_routes
        .iter()
        .filter(|(provider_id, route)| {
            let native_openai = rightkit_llm::agent_providers::find_provider_by_base_url(
                route.base_url.trim().trim_end_matches('/'),
            )
            .is_some_and(|provider| provider.id.eq_ignore_ascii_case("openai"));
            !native_openai
                && !provider_id.eq_ignore_ascii_case("openai")
                && engine_config_from_configured_route(provider_id, route, None).is_some()
        })
        .map(|(provider_id, route)| (provider_id.clone(), route.model.trim().to_string()))
        .collect();
    rows.sort();
    rows
}

fn load_merged_config_for_workspace(
    workspace: &std::path::Path,
    env_vars: &std::collections::HashMap<String, String>,
) -> rightkit_agent_config::HostConfig {
    let loader = rightkit_agent_config::ConfigLoader::new();
    let base = loader.load_files_for_workspace(workspace);
    loader.apply_env(base, env_vars)
}

pub(crate) fn load_cli_instruction_context(
    workspace_root: &std::path::Path,
    add_dir: &[String],
) -> Option<String> {
    let mut parts = Vec::new();

    let mut base_loader = rightkit_agent_config::InstructionLoader::new();
    if let Some(home) = rightkit_agent_config::os_home_dir() {
        base_loader.add_root(
            rightkit_agent_config::InstructionScope::User,
            home.join(".config")
                .join(crate::host_identity::config_dir()),
        );
    }
    base_loader.add_root(
        rightkit_agent_config::InstructionScope::Project,
        workspace_root.to_path_buf(),
    );
    base_loader.add_root(
        rightkit_agent_config::InstructionScope::Local,
        workspace_root.join(crate::host_identity::home_dir()),
    );

    let base_files = base_loader.discover();
    if !base_files.is_empty() {
        parts.push(base_loader.compose(&base_files));
    }

    for dir in add_dir {
        let mut extra_loader = rightkit_agent_config::InstructionLoader::new();
        extra_loader.add_root(
            rightkit_agent_config::InstructionScope::Local,
            workspace_root.join(dir),
        );
        let files = extra_loader.discover();
        if !files.is_empty() {
            parts.push(extra_loader.compose(&files));
        }
    }

    let combined = parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

pub(crate) fn compose_cli_system_prompt(
    instruction_context: Option<&str>,
    system_prompt: Option<&str>,
    append_system_prompt: Option<&str>,
) -> Option<String> {
    let parts = [instruction_context, system_prompt, append_system_prompt]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n---\n\n"))
    }
}

fn resolve_prompt_text(
    inline_label: &str,
    inline: Option<String>,
    file_label: &str,
    file: Option<String>,
) -> Result<Option<String>, String> {
    match (inline, file) {
        (Some(_), Some(_)) => Err(format!(
            "Error: Cannot use both {inline_label} and {file_label}. Please use only one."
        )),
        (Some(text), None) => Ok(Some(text)),
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map(Some)
            .map_err(|err| format!("Error: Cannot read {file_label} at {path}: {err}")),
        (None, None) => Ok(None),
    }
}
