/// Stable per-user data directory for the daemon DB. NOT the OS temp dir:
/// temp is (a) shared by every daemon instance — concurrent daemons collide on
/// one SQLite file and the second fails to open it ("Retry engine"), and (b)
/// periodically cleaned — users would lose all memory/sessions. Resolves the
/// per-user data dir (`%LOCALAPPDATA%` on Windows, `~/Library/Application
/// Support` on macOS, `$XDG_DATA_HOME`/`~/.local/share` on Linux) under an
/// `Orthic Labs/<app name>` subdir, so the folder is `<data>/Orthic Labs/<app name>`, creating it.
/// The base is read from the environment (`user_data_dir`) and deliberately does NOT use
/// `rightkit-fs` AppDirs: the release e2e sandboxes point `HOME`/`XDG_DATA_HOME` into a throwaway
/// home and depend on that, and AppDirs ignores `XDG_DATA_HOME` in release builds.
/// There is no fallback (clean port, #406): when the per-user
/// data dir does not resolve, or the private directory cannot be created, this fails.
/// A database left in the old `<data>/<app name>` folder is moved once (see
/// [`move_legacy_database_folder`]); the server's earlier `<data>/RightKit` folder is looked at
/// only when the host's own folders hold no database, so a host whose own database exists is never
/// refused because of it. Any failure of a move fails this call too.
pub(crate) fn default_db_path() -> Result<String, String> {
    // A QA/test build with an isolated state root keeps the database inside it; a release
    // build never has one.
    let isolated_root = if rightkit_agent_config::state_root_is_isolated() {
        rightkit_agent_config::state_root()
    } else {
        None
    };
    let isolated = isolated_root.is_some();
    let user_data = user_data_dir();
    let dir = default_db_dir(isolated_root, user_data.clone()).ok_or_else(|| {
        "the per-user data directory is unavailable, so the database has no location".to_string()
    })?;
    if !isolated {
        if let Some(user_data) = user_data {
            for (index, legacy) in legacy_folder_names().into_iter().enumerate() {
                // Only the host's own folder (first) is always looked at: the earlier server
                // folder is consulted when the host has no database of its own yet.
                if index > 0 && std::fs::symlink_metadata(dir.join(db_file_name())).is_ok() {
                    break;
                }
                move_legacy_database_folder(&user_data.join(legacy), &dir)?;
            }
        }
    }
    rightkit_fs::ensure_private_dir(&dir).map_err(|error| {
        format!(
            "cannot create the private database directory {}: {error}",
            dir.display()
        )
    })?;
    Ok(dir.join(db_file_name()).display().to_string())
}

/// [`default_db_path`] for a startup path that cannot run without the database: prints the reason
/// and exits with status 2.
pub(crate) fn default_db_path_required() -> String {
    default_db_path().unwrap_or_else(|error| {
        eprintln!("{}: {error}", rightkit_agent_config::host_slug());
        std::process::exit(2)
    })
}

fn default_db_dir(
    isolated_root: Option<std::path::PathBuf>,
    user_data: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    match isolated_root {
        Some(root) => Some(root),
        None => user_data.map(|dir| {
            dir.join("Orthic Labs")
                .join(rightkit_agent_config::app_name())
        }),
    }
}

/// The folder this server's own earlier releases kept the database in: `<data>/RightKit`, a
/// stored location (the name of a folder on the user's disk), not a product name.
const SERVER_LEGACY_FOLDER_NAME: &str = "RightKit";

/// Names of the folders (under the per-user data directory) that may still hold the database from
/// before it moved under `Orthic Labs`: the host's own `<data>/<app name>` and this server's
/// earlier `<data>/RightKit`, each once, the host's own first. The caller looks at a later folder
/// only while the host's database does not exist yet; a database that would have to move onto an
/// existing one is refused (the move reports both folders).
fn legacy_folder_names() -> Vec<&'static str> {
    let mut names = vec![rightkit_agent_config::app_name()];
    if SERVER_LEGACY_FOLDER_NAME != rightkit_agent_config::app_name() {
        names.push(SERVER_LEGACY_FOLDER_NAME);
    }
    names
}

/// File name of the database inside its folder: `<app>.db`.
fn db_file_name() -> String {
    format!("{}.db", crate::host_identity::app())
}

/// Files of the old folder that move with the database, besides the database and its sidecars (the
/// D4 list): the stores that derive their path from the PARENT of the database path.
const LEGACY_MOVE_FILES: &[&str] = &[
    "recipes.json",
    "feature-specs.json",
    "skillopt-runs.json",
    "triggers.json",
    "runtime-middleware.json",
    "github-app-state.json",
];
/// Directory of the old folder that moves with the database (raw trigger artifacts).
const LEGACY_MOVE_DIR: &str = "workflow-trigger-artifacts";

/// REMOVE THIS CODE in the release after the first one that ships it (D4, one release only). It is not a compatibility reader.
///
/// Moves the database and its sibling stores from `old_dir` to `new_dir` (same parent, so one
/// volume). Nothing else is moved: on Windows the old folder is the install directory. An absent or
/// database-less old folder is Ok and never read again. Any problem returns Err and moves nothing
/// (or undoes what it moved); the app then refuses to start rather than pick a database.
fn move_legacy_database_folder(
    old_dir: &std::path::Path,
    new_dir: &std::path::Path,
) -> Result<(), String> {
    // True when the old folder holds a regular database file; a symlink there is refused.
    fn old_database_present(old_dir: &std::path::Path) -> Result<bool, String> {
        let path = old_dir.join(db_file_name());
        match std::fs::symlink_metadata(&path) {
            Err(_) => Ok(false),
            Ok(meta) if meta.file_type().is_symlink() => Err(format!(
                "{} is a symbolic link: {} will not move it",
                path.display(),
                rightkit_agent_config::app_name()
            )),
            Ok(meta) => Ok(meta.is_file()),
        }
    }
    let exists = |path: std::path::PathBuf| std::fs::symlink_metadata(path).is_ok();
    let old = old_dir.display();
    let new = new_dir.display();

    if !old_database_present(old_dir)? {
        return Ok(());
    }
    rightkit_fs::ensure_private_dir(new_dir).map_err(|error| {
        format!(
            "cannot create the private database directory {}: {error}",
            new_dir.display()
        )
    })?;
    let _move_lock = rightkit_fs::FileLock::acquire(
        &new_dir.join("db-folder-move.lock"),
        &rightkit_fs::LockOptions::default().labeled(&crate::host_identity::prefixed("db-move")),
    )
    .map_err(|error| format!("cannot lock the database folder move in {new}: {error}"))?;
    // A second process may have moved the files while this one waited for the lock.
    if !old_database_present(old_dir)? {
        return Ok(());
    }
    if exists(new_dir.join(db_file_name())) {
        let app = rightkit_agent_config::app_name();
        return Err(format!(
            "the database exists in both {old} and {new}: {app} will not choose between them and will not start. Keep the one you want, move or delete the other, then start {app} again."
        ));
    }
    let old_daemon_lock = old_dir.join(format!("{}.daemon.lock", crate::host_identity::app()));
    if exists(old_daemon_lock.clone()) {
        let held = rightkit_fs::FileLock::try_acquire(&old_daemon_lock);
        if held.is_err() {
            let app = rightkit_agent_config::app_name();
            return Err(format!(
                "a {app} daemon of an earlier version is still using the database in {old}: quit {app} and start it again"
            ));
        }
        drop(held);
    }

    // Entries present in the old folder, in move order: sidecars first, the database LAST.
    let is_regular_file = |name: &str| {
        std::fs::symlink_metadata(old_dir.join(name))
            .map(|meta| meta.is_file())
            .unwrap_or(false)
    };
    let mut names: Vec<String> = Vec::new();
    let db_name = db_file_name();
    let wal_name = format!("{db_name}-wal");
    let shm_name = format!("{db_name}-shm");
    for name in [&wal_name, &shm_name] {
        if is_regular_file(name.as_str()) {
            names.push(name.clone());
        }
    }
    for name in LEGACY_MOVE_FILES {
        if is_regular_file(name) {
            names.push((*name).to_string());
        }
    }
    if let Ok(entries) = std::fs::read_dir(old_dir) {
        let mut backups: Vec<String> = entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(&format!("{db_name}.pre-v")) && name.ends_with(".bak"))
            .collect();
        backups.sort();
        names.extend(backups);
    }
    if std::fs::symlink_metadata(old_dir.join(LEGACY_MOVE_DIR))
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        names.push(LEGACY_MOVE_DIR.to_string());
    }
    names.push(db_name);

    for name in &names {
        if exists(new_dir.join(name)) {
            return Err(format!("cannot move {name}: it already exists in {new}"));
        }
    }
    let mut moved: Vec<&String> = Vec::new();
    for name in &names {
        if let Err(error) = std::fs::rename(old_dir.join(name), new_dir.join(name)) {
            let mut undone = true;
            for done in moved.iter().rev() {
                if std::fs::rename(new_dir.join(done), old_dir.join(done)).is_err() {
                    undone = false;
                }
            }
            let tail = if undone {
                "the move was undone"
            } else {
                "the move could not be fully undone, look in both folders"
            };
            return Err(format!(
                "could not move {name} from {old} to {new}: {error}; {tail}"
            ));
        }
        moved.push(name);
    }
    tracing::info!("moved the database folder from {old} to {new}");
    Ok(())
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
    let api_key = route_api_key(provider_id, route);
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
    if route_api_key(provider_id, route).is_some() {
        return None;
    }
    // Round 2 L11: a key that exists but is withheld by the host binding is not "missing".
    if rightkit_llm::agent_providers::load_api_key(provider_id).is_some() {
        if let Some(refusal) = host_binding_refusal(provider_id, base_url) {
            return Some(format!(
                "provider route `{provider_id}` requires an API key: {refusal}"
            ));
        }
    }
    Some(format!(
        "provider route `{provider_id}` requires an API key and none is configured"
    ))
}

/// A route's key (providers R4): the variable its `api-key-env` names, when that is a
/// `<PREFIX>_<NAME>_API_KEY` and this host has no OS credential store (environment, then
/// `<state root>/.env`); otherwise the key saved under the route id. A name outside that shape
/// finds nothing, so a config file cannot send an arbitrary variable to an endpoint.
pub(crate) fn route_api_key(
    provider_id: &str,
    route: &rightkit_agent_config::ProviderRouteConfig,
) -> Option<String> {
    if let Some(name) = route
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let host = rightkit_llm::agent_providers::host_credentials::host();
        if let Ok(Some(found)) = host.route_key(name) {
            let value = found.value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    if !saved_key_bound_to_route_host(provider_id, &route.base_url) {
        return None;
    }
    configured_route_api_key(provider_id, rightkit_llm::agent_providers::load_api_key)
}

/// Audit 2026-11-05 platform H1: a key saved under a built-in provider id (`openrouter`,
/// `openai`, ...) is that provider's credential. It is attached only when the route's base URL
/// has the registry entry's scheme, host and port, so a route that reuses the id with another
/// host (written by `POST /local-models/add-provider` or by hand) never receives it. Ids outside
/// the registry, and the registry's host-less `custom` entry, are not restricted here.
///
/// Round 2 M4: keys are also saved for credential ids outside that registry (`anthropic`,
/// `google`, `mistral`, `antigravity`, `tinyfish`). Each is bound to its vendor's API origin,
/// and an id with no API route of its own (a vendor CLI or web-search key) is never attached to
/// a provider route at all.
pub(crate) fn saved_key_bound_to_route_host(provider_id: &str, route_base_url: &str) -> bool {
    match saved_key_binding(provider_id) {
        SavedKeyBinding::Unbound => true,
        SavedKeyBinding::Origin(origin) => same_url_origin(&origin, route_base_url),
        SavedKeyBinding::NoRoute => false,
    }
}

/// Where a key saved under a provider id may be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SavedKeyBinding {
    /// A route-specific id (written for that route), or the registry's host-less `custom`.
    Unbound,
    /// Only a route on this origin receives the key.
    Origin(String),
    /// A credential id with no API route of its own: never attached to a route.
    NoRoute,
}

/// API origins of the credential ids the host store knows that the provider registry does not.
fn non_registry_credential_origin(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "anthropic" => Some("https://api.anthropic.com"),
        "google" => Some("https://generativelanguage.googleapis.com"),
        "mistral" => Some("https://api.mistral.ai"),
        _ => None,
    }
}

pub(crate) fn saved_key_binding(provider_id: &str) -> SavedKeyBinding {
    let id = provider_id.trim().to_ascii_lowercase();
    if let Some(provider) = rightkit_llm::agent_providers::find_provider(&id) {
        return if provider.base_url.is_empty() {
            SavedKeyBinding::Unbound
        } else {
            SavedKeyBinding::Origin(provider.base_url.to_string())
        };
    }
    if let Some(origin) = non_registry_credential_origin(&id) {
        return SavedKeyBinding::Origin(origin.to_string());
    }
    // Any other id the credential store has fixed env names for is a well-known vendor
    // credential (a CLI or web-search key), not a key saved for a route.
    if !rightkit_llm::agent_providers::host_credentials::provider_env_names(&id).is_empty() {
        return SavedKeyBinding::NoRoute;
    }
    SavedKeyBinding::Unbound
}

/// Round 2 L11: when a saved key exists but is withheld by the host binding, say so instead of
/// "no key configured". `None` when the binding is not what withheld it.
pub(crate) fn host_binding_refusal(provider_id: &str, route_base_url: &str) -> Option<String> {
    if saved_key_bound_to_route_host(provider_id, route_base_url) {
        return None;
    }
    let route_host = url::Url::parse(route_base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| route_base_url.trim().to_string());
    Some(match saved_key_binding(provider_id) {
        SavedKeyBinding::Origin(origin) => format!(
            "the key saved for `{provider_id}` is only sent to {origin}, and this route points at {route_host}; \
             save a key under a different route id for this endpoint (or set `api-key-env`)"
        ),
        _ => format!(
            "the key saved for `{provider_id}` is a vendor credential and is never sent to a provider route \
             ({route_host}); save a key under a different route id for this endpoint (or set `api-key-env`)"
        ),
    })
}

/// True when both URLs parse and share scheme, host (ASCII case-insensitive) and effective port.
pub(crate) fn same_url_origin(left: &str, right: &str) -> bool {
    let (Ok(left), Ok(right)) = (url::Url::parse(left.trim()), url::Url::parse(right.trim()))
    else {
        return false;
    };
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && match (left.host_str(), right.host_str()) {
            (Some(l), Some(r)) => l.eq_ignore_ascii_case(r),
            _ => false,
        }
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod route_key_host_binding_tests {
    use super::{
        host_binding_refusal, same_url_origin, saved_key_binding, saved_key_bound_to_route_host,
        SavedKeyBinding,
    };

    #[test]
    fn registry_id_key_binds_to_registry_host() {
        assert!(saved_key_bound_to_route_host(
            "openrouter",
            "https://openrouter.ai/api/v1"
        ));
        assert!(saved_key_bound_to_route_host(
            "openrouter",
            "https://OpenRouter.ai/api/v1/"
        ));
        assert!(!saved_key_bound_to_route_host(
            "openrouter",
            "https://attacker.example/v1"
        ));
        assert!(!saved_key_bound_to_route_host(
            "openai",
            "http://api.openai.com/v1"
        ));
        assert!(!saved_key_bound_to_route_host(
            "minimax",
            "https://api.minimax.io:8443/v1"
        ));
        assert!(!saved_key_bound_to_route_host("OpenAI", "not a url"));
    }

    /// Round 2 M4: keys saved for credential ids outside the registry follow their vendor's
    /// origin, or no route at all.
    #[test]
    fn non_registry_credential_ids_are_host_bound() {
        assert!(saved_key_bound_to_route_host(
            "anthropic",
            "https://api.anthropic.com/v1"
        ));
        assert!(!saved_key_bound_to_route_host(
            "anthropic",
            "https://attacker.example/v1"
        ));
        assert!(!saved_key_bound_to_route_host(
            "Anthropic",
            "https://attacker.example/v1"
        ));
        assert!(saved_key_bound_to_route_host(
            "google",
            "https://generativelanguage.googleapis.com/v1beta/openai"
        ));
        assert!(!saved_key_bound_to_route_host(
            "google",
            "https://attacker.example/v1"
        ));
        assert!(saved_key_bound_to_route_host(
            "mistral",
            "https://api.mistral.ai/v1"
        ));
        assert!(!saved_key_bound_to_route_host(
            "mistral",
            "http://api.mistral.ai/v1"
        ));
        for id in ["antigravity", "tinyfish"] {
            assert!(
                !saved_key_bound_to_route_host(id, "https://attacker.example/v1"),
                "{id}"
            );
            assert_eq!(saved_key_binding(id), SavedKeyBinding::NoRoute, "{id}");
        }
    }

    /// Round 2 L11: a withheld key is reported as withheld, naming both hosts.
    #[test]
    fn host_binding_refusal_names_the_bound_origin() {
        let message = host_binding_refusal("openrouter", "https://litellm.internal:4000/v1")
            .expect("openrouter is bound to openrouter.ai");
        assert!(message.contains("openrouter.ai"), "{message}");
        assert!(message.contains("litellm.internal"), "{message}");
        assert!(host_binding_refusal("openrouter", "https://openrouter.ai/api/v1").is_none());
        assert!(host_binding_refusal("my-lab", "https://lab.example/v1").is_none());
        assert!(host_binding_refusal("tinyfish", "https://x.example/v1")
            .is_some_and(|m| m.contains("never sent")));
    }

    #[test]
    fn non_registry_and_custom_ids_are_not_host_bound() {
        assert!(saved_key_bound_to_route_host(
            "my-lab",
            "https://lab.example/v1"
        ));
        assert!(saved_key_bound_to_route_host(
            "custom",
            "http://127.0.0.1:11434/v1"
        ));
    }

    #[test]
    fn origin_compare_requires_parseable_hosts() {
        assert!(same_url_origin(
            "https://a.example/x",
            "https://a.example:443/y"
        ));
        assert!(!same_url_origin(
            "https://a.example",
            "https://a.example.evil"
        ));
        assert!(!same_url_origin("https://a.example", ""));
    }
}

/// R41 (store branch): a route's saved API key comes from the credential store, or on a host
/// with no store from `load_api_key`'s environment and `.env` sources. No
/// environment variable is read for it (a caller can set any variable, and the
/// child environment is visible to every tool the run starts).
fn configured_route_api_key<SavedLookup>(
    provider_id: &str,
    mut saved_lookup: SavedLookup,
) -> Option<String>
where
    SavedLookup: FnMut(&str) -> Option<String>,
{
    saved_lookup(provider_id)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

pub(crate) fn load_merged_config_for_workspace(
    workspace: &std::path::Path,
    env_vars: &std::collections::HashMap<String, String>,
) -> rightkit_agent_config::HostConfig {
    let loader = rightkit_agent_config::ConfigLoader::new();
    let base = loader.load_files_for_workspace(workspace);
    loader.apply_env(base, env_vars)
}

/// Instruction context for a daemon session. `granted`: the session carries the per-session
/// `trustWorkspace` config grant, so project and local instruction files load although the trust
/// store has no record (the grant is never persisted).
pub(crate) fn load_cli_instruction_context_granted(
    workspace_root: &std::path::Path,
    add_dir: &[String],
    granted: bool,
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
        workspace_root.join(rightkit_agent_config::state_dir_name()),
    );

    // Rules 13 and 67: project and local instruction files load only in a trusted workspace.
    // `granted`: the session carries a per-session `trustWorkspace` grant (never persisted).
    let trusted = granted || rightkit_agent_config::is_workspace_trusted(workspace_root);
    // Project root down to the working directory (nested AGENTS.md, CLAUDE.md, right.md).
    base_loader.add_project_walk(workspace_root);
    let base_files = base_loader.discover_gated(|_| trusted);
    if !base_files.is_empty() {
        parts.push(base_loader.compose(&base_files));
    }

    for dir in add_dir {
        let mut extra_loader = rightkit_agent_config::InstructionLoader::new();
        extra_loader.add_root(
            rightkit_agent_config::InstructionScope::Local,
            workspace_root.join(dir),
        );
        let files = extra_loader.discover_gated(|_| trusted);
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
