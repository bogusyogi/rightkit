//! Account list and account selection API.
//!
//! Two account kinds, one list:
//! - `subscription`: a managed vendor-CLI profile (claude/codex). The vendor
//!   CLI owns the login; we expose only a label and non-secret identity.
//! - `api_key`: a registry entry in `accounts.json` whose key sits in the OS
//!   credential store. The key never appears in any response or in config.
//!
//! Selection is explicit: a session selection, else the project default, else
//! nothing. There is no automatic switching; a selection whose account has
//! disappeared resolves to "unavailable", never to some other account.
//!
//! Policy: docs/reports/2026-10-16-claude-subscription-third-party-use.md
//! (issue #34): unmodified vendor binary, no token reads, sign-in through the
//! vendor flow, API keys only via rightkit-credentials.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use rightkit_agent_config::accounts::{
    account_kind_of, AccountKind, AccountSelections, AccountStore, AccountsError,
    ApiKeyAccountEntry, LinkedProfileStore,
};
use rightkit_agents::driver_host::cli_runtime::account_identity::{
    discover_claude_folders, list_subscription_accounts, AccountSource, DiscoveredFolder,
    SubscriptionAccount,
};
use rightkit_llm::agent_providers::client::{
    account_key_present_with, delete_account_key_with, store_account_key_with,
};
use rightkit_llm::agent_providers::KeyStoreBackend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::DaemonState;

type ApiError = (StatusCode, Json<Value>);

/// Error `code` of a refused cross-home account switch (#343).
pub(crate) const ACCOUNT_HOME_MISMATCH: &str = "account_home_mismatch";

pub fn account_routes() -> Router<DaemonState> {
    Router::new()
        .route("/accounts", get(list_accounts))
        .route("/accounts/api-key", post(add_api_key_account))
        .route(
            "/accounts/api-key/{provider}/{slug}",
            delete(remove_api_key_account),
        )
        .route("/accounts/linked", post(link_profile_folder))
        .route(
            "/accounts/linked/{provider}/{label}",
            delete(unlink_profile_folder),
        )
        .route("/accounts/discover", get(discover_profile_folders))
        .route("/accounts/selection", get(get_selection))
        .route("/accounts/session/{session_id}", put(put_session_account))
        .route("/accounts/project-default", put(put_project_default))
}

/// One row of the account list. Never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub kind: AccountKind,
    pub label: String,
    pub provider: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    /// provider + email + org UUID fingerprint (#341); `None` when the CLI exposes no identity.
    pub fingerprint: Option<String>,
    /// Subscription: signed in. API key: a key is stored.
    pub ready: bool,
    /// `managed` (a profile the host made), `linked` (a folder the user already had) or `api_key`.
    pub source: &'static str,
    /// The user's own folder, for a linked account only.
    pub folder: Option<String>,
    /// `bedrock` or `vertex` for a Claude folder set up for that platform; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// Everything the account logic touches, injectable for tests.
struct Ctx<'a> {
    store: AccountStore,
    keys: &'a dyn KeyStoreBackend,
    subscriptions: Vec<SubscriptionAccount>,
    links: LinkedProfileStore,
}

impl<'a> Ctx<'a> {
    fn live(keys: &'a dyn KeyStoreBackend) -> Result<Self, ApiError> {
        Ok(Self {
            store: AccountStore::default_location().map_err(accounts_err)?,
            keys,
            subscriptions: list_subscription_accounts(),
            links: LinkedProfileStore::default_location().map_err(accounts_err)?,
        })
    }

    fn list(&self) -> Result<Vec<AccountView>, ApiError> {
        self.list_inner(true)
    }

    /// `check_keys` false skips the credential-store lookup for API-key accounts (`ready` is then
    /// `true`): used by the session-list view, which only needs names.
    fn list_inner(&self, check_keys: bool) -> Result<Vec<AccountView>, ApiError> {
        let mut out: Vec<AccountView> = self
            .subscriptions
            .iter()
            .map(|a| AccountView {
                id: a.id.clone(),
                kind: AccountKind::Subscription,
                label: a.label.clone(),
                provider: a.provider.clone(),
                email: a.identity.as_ref().and_then(|i| i.email.clone()),
                organization: a.identity.as_ref().and_then(|i| i.organization.clone()),
                fingerprint: a.fingerprint.clone(),
                ready: a.signed_in,
                source: match a.source {
                    AccountSource::Managed => "managed",
                    AccountSource::Linked => "linked",
                },
                folder: a.folder.clone(),
                platform: a.platform.clone(),
            })
            .collect();
        for entry in self.store.load().map_err(accounts_err)?.api_key_accounts {
            let ready =
                !check_keys || account_key_present_with(self.keys, &entry.id).unwrap_or(false);
            out.push(AccountView {
                id: entry.id,
                kind: AccountKind::ApiKey,
                label: entry.label,
                provider: entry.provider,
                email: None,
                organization: None,
                fingerprint: None,
                ready,
                source: "api_key",
                folder: None,
                platform: None,
            });
        }
        Ok(out)
    }

    fn find(&self, id: &str) -> Result<Option<AccountView>, ApiError> {
        Ok(self.list()?.into_iter().find(|a| a.id == id))
    }

    fn add_api_key(
        &self,
        label: &str,
        provider: &str,
        api_key: &str,
    ) -> Result<ApiKeyAccountEntry, ApiError> {
        if api_key.trim().is_empty() {
            return Err(bad_request("apiKey must not be empty"));
        }
        let entry = self
            .store
            .add_api_key_account(label, provider)
            .map_err(accounts_err)?;
        if let Err(error) = store_account_key_with(self.keys, &entry.id, api_key) {
            // No entry without its key: roll the registry back.
            let _ = self.store.remove_api_key_account(&entry.id);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("credential store: {error}") })),
            ));
        }
        Ok(entry)
    }

    fn remove_api_key(&self, id: &str) -> Result<(), ApiError> {
        self.store
            .remove_api_key_account(id)
            .map_err(accounts_err)?;
        delete_account_key_with(self.keys, id).map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("credential store: {error}") })),
            )
        })
    }

    /// Link an existing folder. Nothing in the folder is opened except `.claude.json` when the
    /// account is listed afterwards; nothing is written to it.
    fn link_folder(
        &self,
        provider: &str,
        label: &str,
        path: &str,
    ) -> Result<AccountView, ApiError> {
        let in_use = self
            .subscriptions
            .iter()
            .any(|a| a.provider == provider && a.label == label);
        self.links
            .link(provider, label, path, in_use)
            .map_err(accounts_err)?;
        // Re-read so the answer shows exactly what the list will show.
        let fresh = Ctx {
            store: self.store.clone(),
            keys: self.keys,
            subscriptions: list_subscription_accounts(),
            links: self.links.clone(),
        };
        let id = rightkit_agent_config::accounts::subscription_account_id(provider, label);
        fresh.find(&id)?.ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "the folder was linked but could not be listed" })),
            )
        })
    }

    /// Candidates for linking, with whether each folder is already linked.
    fn discover(&self, found: Vec<DiscoveredFolder>) -> Result<Value, ApiError> {
        let links = self.links.load().map_err(accounts_err)?.links;
        let same = |a: &str, b: &str| {
            a == b
                || matches!(
                    (std::fs::canonicalize(a), std::fs::canonicalize(b)),
                    (Ok(x), Ok(y)) if x == y
                )
        };
        let rows: Vec<Value> = found
            .into_iter()
            .map(|folder| {
                let link = links.iter().find(|l| same(&l.path, &folder.path));
                json!({
                    "provider": "claude",
                    "path": folder.path,
                    "linked": link.is_some(),
                    "label": link.map(|l| l.label.clone()),
                    "email": folder.identity.as_ref().and_then(|i| i.email.clone()),
                    "organization": folder.identity.as_ref().and_then(|i| i.organization.clone()),
                    "platform": rightkit_agents::driver_host::cli_runtime::account_identity::claude_platform_of(
                        std::path::Path::new(&folder.path),
                    ),
                })
            })
            .collect();
        Ok(json!({ "folders": rows }))
    }

    fn require_account(&self, id: &str) -> Result<(), ApiError> {
        if account_kind_of(id).is_none() {
            return Err(bad_request("malformed account id"));
        }
        match self.find(id)? {
            Some(_) => Ok(()),
            None => Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("unknown account: {id}") })),
            )),
        }
    }

    /// The config home an account runs under: a subscription's managed home, `None` for an API-key
    /// account (it runs under the ambient vendor home).
    fn config_home(&self, id: &str) -> Option<std::path::PathBuf> {
        self.subscriptions
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.managed_home.clone())
    }

    /// Same-home rule (#343). The transcript and resume state of a thread live in the config home
    /// of the account it ran under, so a thread that already has an account must not be moved to
    /// an account with a different provider or config home. The user starts a new session on the
    /// chosen account instead (the desktop offers that, with a handoff summary); nothing switches
    /// automatically. `thread_started == Some(false)` means the thread has not run yet, so the
    /// choice is still free.
    fn check_same_home(
        &self,
        session_id: &str,
        new_account: Option<&str>,
        thread_started: Option<bool>,
    ) -> Result<(), ApiError> {
        if thread_started == Some(false) {
            return Ok(());
        }
        let file = self.store.load().map_err(accounts_err)?;
        let Some(current) = file.selections.sessions.get(session_id) else {
            // No account was recorded for this chat. If it has already run, it ran on the
            // default login or the project default, so moving it to a chosen account would also
            // change its home. Only a chat that has not run yet (or whose state is unknown) may
            // take its first account.
            if thread_started == Some(true) && new_account.is_some() {
                return Err(home_mismatch(None, new_account));
            }
            return Ok(());
        };
        if Some(current.as_str()) == new_account {
            return Ok(());
        }
        // A selection whose account vanished is "unavailable": the user must choose again.
        if self.find(current)?.is_none() {
            return Ok(());
        }
        let provider_of = |id: &str| id.split('/').nth(1).map(str::to_string);
        let same = match new_account {
            Some(new) => {
                provider_of(current) == provider_of(new)
                    && self.config_home(current) == self.config_home(new)
            }
            // Clearing returns the thread to the ambient login, a different home.
            None => false,
        };
        if same {
            return Ok(());
        }
        Err(home_mismatch(Some(current), new_account))
    }

    fn set_session(
        &self,
        session_id: &str,
        account_id: Option<&str>,
        thread_started: Option<bool>,
    ) -> Result<(), ApiError> {
        if let Some(id) = account_id {
            self.require_account(id)?;
        }
        self.check_same_home(session_id, account_id, thread_started)?;
        self.store
            .set_session_account(session_id, account_id)
            .map_err(accounts_err)
    }

    fn set_project(&self, project: &str, account_id: Option<&str>) -> Result<(), ApiError> {
        if let Some(id) = account_id {
            self.require_account(id)?;
        }
        self.store
            .set_project_default(project, account_id)
            .map_err(accounts_err)
    }

    /// `source` is `session`, `project` or `none`. A selected account that no
    /// longer exists reports `unavailable: true` with `account: null`; the
    /// caller must ask the user rather than pick another.
    fn selection(
        &self,
        session_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<Value, ApiError> {
        let file = self.store.load().map_err(accounts_err)?;
        let (id, source) = match session_id.and_then(|s| file.selections.sessions.get(s)) {
            Some(id) => (Some(id.clone()), "session"),
            None => match project.and_then(|p| file.selections.project_defaults.get(p)) {
                Some(id) => (Some(id.clone()), "project"),
                None => (None, "none"),
            },
        };
        let account = match &id {
            Some(id) => self.find(id)?,
            None => None,
        };
        Ok(json!({
            "accountId": id,
            "source": source,
            "from": (source != "none").then_some(source),
            "account": account,
            "unavailable": id.is_some() && account.is_none(),
        }))
    }
}

/// The refusal of a cross-home switch (#343): plain message, stable `code`, and the action the
/// desktop offers instead.
fn home_mismatch(current: Option<&str>, requested: Option<&str>) -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "This chat has already run on another account. Start a new chat to use a different account.",
            "code": ACCOUNT_HOME_MISMATCH,
            "action": "new_session",
            "currentAccountId": current,
            "requestedAccountId": requested,
        })),
    )
}

fn bad_request(message: &str) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn accounts_err(error: AccountsError) -> ApiError {
    let status = match error {
        AccountsError::InvalidLabel(_)
        | AccountsError::InvalidProvider(_)
        | AccountsError::InvalidPath(_)
        | AccountsError::InvalidAccountId(_) => StatusCode::BAD_REQUEST,
        AccountsError::LabelInUse(_) => StatusCode::CONFLICT,
        AccountsError::UnknownAccount(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() })))
}

fn os_keys() -> rightkit_llm::agent_providers::HostKeyStore {
    rightkit_llm::agent_providers::HostKeyStore::current()
}

async fn list_accounts() -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    let accounts = Ctx::live(&keys)?.list()?;
    Ok(Json(json!({ "accounts": accounts })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddApiKeyBody {
    label: String,
    provider: String,
    api_key: String,
}

async fn add_api_key_account(
    State(_state): State<DaemonState>,
    Json(body): Json<AddApiKeyBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let keys = os_keys();
    let ctx = Ctx::live(&keys)?;
    let entry = ctx.add_api_key(&body.label, &body.provider, &body.api_key)?;
    invalidate_snapshot();
    let account = ctx.find(&entry.id)?;
    Ok((StatusCode::CREATED, Json(json!({ "account": account }))))
}

async fn remove_api_key_account(
    Path((provider, slug)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    let id = rightkit_agent_config::accounts::api_key_account_id(&provider, &slug);
    Ctx::live(&keys)?.remove_api_key(&id)?;
    invalidate_snapshot();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkBody {
    provider: String,
    label: String,
    path: String,
}

async fn link_profile_folder(
    Json(body): Json<LinkBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let keys = os_keys();
    let account = Ctx::live(&keys)?.link_folder(&body.provider, &body.label, &body.path)?;
    invalidate_snapshot();
    Ok((StatusCode::CREATED, Json(json!({ "account": account }))))
}

/// Removes the link only; the folder and everything in it stay as they are.
async fn unlink_profile_folder(
    Path((provider, label)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let links = LinkedProfileStore::default_location().map_err(accounts_err)?;
    links.unlink(&provider, &label).map_err(accounts_err)?;
    invalidate_snapshot();
    Ok(Json(json!({ "ok": true })))
}

/// Looks for likely existing profile folders. Opens only `.claude.json` in each; links nothing.
async fn discover_profile_folders() -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    Ctx::live(&keys)?
        .discover(discover_claude_folders())
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectionQuery {
    session_id: Option<String>,
    project: Option<String>,
}

async fn get_selection(Query(q): Query<SelectionQuery>) -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    Ctx::live(&keys)?
        .selection(q.session_id.as_deref(), q.project.as_deref())
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectBody {
    /// `null` clears the selection.
    account_id: Option<String>,
    /// `false` when the chat has not run yet, which lifts the same-home rule (#343).
    #[serde(default)]
    thread_started: Option<bool>,
}

async fn put_session_account(
    Path(session_id): Path<String>,
    Json(body): Json<SelectBody>,
) -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    Ctx::live(&keys)?.set_session(&session_id, body.account_id.as_deref(), body.thread_started)?;
    invalidate_snapshot();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectDefaultBody {
    project: String,
    account_id: Option<String>,
}

async fn put_project_default(
    Json(body): Json<ProjectDefaultBody>,
) -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    Ctx::live(&keys)?.set_project(&body.project, body.account_id.as_deref())?;
    invalidate_snapshot();
    Ok(Json(json!({ "ok": true })))
}

// ---- Session creation and the session-list view ----------------------------------------------

/// Refuse an account a new chat cannot use, with a plain message, before anything is created.
pub(crate) fn validate_new_session_account(account_id: &str) -> Result<(), ApiError> {
    if account_kind_of(account_id).is_none() {
        return Err(bad_request("That account name is not valid."));
    }
    let keys = os_keys();
    let ctx = Ctx::live(&keys)?;
    match ctx.find(account_id)? {
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "That account is not set up. Pick one from the account list.",
                "code": "account_unknown",
            })),
        )),
        Some(account) if !account.ready => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "That account is not signed in yet. Sign in to it first.",
                "code": "account_not_ready",
            })),
        )),
        Some(_) => Ok(()),
    }
}

/// A new chat's account selection. Dropping it without `keep` takes the selection back, so a
/// create that fails after recording leaves nothing behind.
pub(crate) struct NewSessionAccount {
    session_id: String,
    kept: bool,
}

impl NewSessionAccount {
    pub(crate) fn keep(mut self) {
        self.kept = true;
        invalidate_snapshot();
    }
}

impl Drop for NewSessionAccount {
    fn drop(&mut self) {
        if !self.kept {
            if let Ok(store) = AccountStore::default_location() {
                if let Err(error) = store.set_session_account(&self.session_id, None) {
                    tracing::warn!(%error, session_id = %self.session_id, "could not release the account recorded for an unstarted session");
                }
            }
            invalidate_snapshot();
        }
    }
}

/// Record `account_id` as the selection of the new session `session_id`.
pub(crate) fn record_new_session_account(
    session_id: &str,
    account_id: &str,
) -> Result<NewSessionAccount, ApiError> {
    let store = AccountStore::default_location().map_err(accounts_err)?;
    store
        .set_session_account(session_id, Some(account_id))
        .map_err(accounts_err)?;
    Ok(NewSessionAccount {
        session_id: session_id.to_string(),
        kept: false,
    })
}

/// Account names and selections read once, for rows of a session list.
pub(crate) struct AccountsSnapshot {
    views: Vec<AccountView>,
    selections: AccountSelections,
}

static SNAPSHOT: std::sync::Mutex<Option<(std::time::Instant, std::sync::Arc<AccountsSnapshot>)>> =
    std::sync::Mutex::new(None);

/// Drop the cached snapshot (every route that changes accounts or selections calls this).
pub(crate) fn invalidate_snapshot() {
    if let Ok(mut cache) = SNAPSHOT.lock() {
        *cache = None;
    }
}

fn load_snapshot() -> AccountsSnapshot {
    let keys = os_keys();
    let views = Ctx::live(&keys)
        .and_then(|ctx| ctx.list_inner(false))
        .unwrap_or_default();
    let selections = AccountStore::default_location()
        .and_then(|store| store.load())
        .map(|file| file.selections)
        .unwrap_or_default();
    AccountsSnapshot { views, selections }
}

/// A fresh read of the account list and selections. Call once per session-list request and pass
/// the result to [`AccountsSnapshot::view_for`] for each row.
pub(crate) fn accounts_snapshot() -> AccountsSnapshot {
    load_snapshot()
}

impl AccountsSnapshot {
    /// `{"accountId","label","provider","email","source","from"}` for the account this chat runs
    /// on: its own selection (`from: "session"`), else the project default (`from: "project"`),
    /// else `None`. A selected account that no longer exists keeps its id and provider, has
    /// `label`, `email` and `source` null and `"unavailable": true`.
    pub(crate) fn view_for(&self, session_id: &str, workspace_root: &str) -> Option<Value> {
        let (id, from) = match self.selections.sessions.get(session_id) {
            Some(id) => (id, "session"),
            None => (
                self.selections.project_defaults.get(workspace_root)?,
                "project",
            ),
        };
        Some(match self.views.iter().find(|a| &a.id == id) {
            Some(account) => json!({
                "accountId": account.id,
                "label": account.label,
                "provider": account.provider,
                "email": account.email,
                "source": account.source,
                "from": from,
            }),
            None => json!({
                "accountId": id,
                "label": null,
                "provider": id.split('/').nth(1),
                "email": null,
                "source": null,
                "from": from,
                "unavailable": true,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rightkit_llm::agent_providers::keystore::{KeyDelete, KeyStoreBackend, KeyStoreError};

    /// In-memory [`KeyStoreBackend`] for these tests (the published provider crate has no fake):
    /// never touches an OS credential store.
    #[derive(Default)]
    struct NoKeys {
        entries: std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
    }

    impl KeyStoreBackend for NoKeys {
        fn get(&self, service: &str, key: &str) -> Result<String, KeyStoreError> {
            self.entries
                .lock()
                .unwrap()
                .get(&(service.to_string(), key.to_string()))
                .cloned()
                .ok_or_else(|| KeyStoreError::NotFound {
                    service: service.to_string(),
                    key: key.to_string(),
                })
        }

        fn set(&self, service: &str, key: &str, value: &str) -> Result<(), KeyStoreError> {
            self.entries
                .lock()
                .unwrap()
                .insert((service.to_string(), key.to_string()), value.to_string());
            Ok(())
        }

        fn create(&self, service: &str, key: &str, value: &str) -> Result<bool, KeyStoreError> {
            let mut entries = self.entries.lock().unwrap();
            match entries.entry((service.to_string(), key.to_string())) {
                std::collections::hash_map::Entry::Occupied(_) => Ok(false),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(value.to_string());
                    Ok(true)
                }
            }
        }

        fn delete(&self, service: &str, key: &str) -> Result<(), KeyStoreError> {
            self.entries
                .lock()
                .unwrap()
                .remove(&(service.to_string(), key.to_string()))
                .map(|_| ())
                .ok_or_else(|| KeyStoreError::NotFound {
                    service: service.to_string(),
                    key: key.to_string(),
                })
        }

        fn delete_if_equals(
            &self,
            service: &str,
            key: &str,
            expected: &str,
        ) -> Result<KeyDelete, KeyStoreError> {
            let mut entries = self.entries.lock().unwrap();
            let slot = (service.to_string(), key.to_string());
            Ok(match entries.get(&slot) {
                None => KeyDelete::Absent,
                Some(held) if held == expected => {
                    entries.remove(&slot);
                    KeyDelete::Deleted
                }
                Some(_) => KeyDelete::Changed,
            })
        }
    }

    fn sub(label: &str, home: &str, source: AccountSource) -> SubscriptionAccount {
        SubscriptionAccount {
            id: format!("subscription/claude/{label}"),
            provider: "claude".into(),
            label: label.into(),
            signed_in: true,
            auth_mode:
                rightkit_agents::driver_host::cli_runtime::ids::CliAuthMode::NativeCliAccount,
            identity: None,
            fingerprint: None,
            managed_home: home.into(),
            source,
            folder: (source == AccountSource::Linked).then(|| home.to_string()),
            platform: None,
        }
    }

    fn ctx<'a>(dir: &std::path::Path, keys: &'a NoKeys, subs: Vec<SubscriptionAccount>) -> Ctx<'a> {
        Ctx {
            store: AccountStore::at(dir.join("accounts.json")),
            keys,
            subscriptions: subs,
            links: LinkedProfileStore::at(
                dir.join("linked-profiles.json"),
                Some(dir.to_path_buf()),
            ),
        }
    }

    fn code(err: &ApiError) -> Option<String> {
        err.1
             .0
            .get("code")
            .and_then(|c| c.as_str())
            .map(str::to_string)
    }

    #[test]
    fn a_chat_that_has_run_cannot_move_to_a_linked_account_with_another_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = NoKeys::default();
        let c = ctx(
            tmp.path(),
            &keys,
            vec![
                sub("work", "/home/u/.claude-work", AccountSource::Linked),
                sub("home", "/home/u/.claude-home", AccountSource::Linked),
            ],
        );
        c.store
            .set_session_account("s1", Some("subscription/claude/work"))
            .unwrap();
        let err = c
            .set_session("s1", Some("subscription/claude/home"), Some(true))
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(code(&err).as_deref(), Some(ACCOUNT_HOME_MISMATCH));
        let message = err.1 .0["error"].as_str().unwrap();
        assert!(message.contains("Start a new chat"), "{message}");
        // The same account is still fine; so is the choice before the chat has run.
        c.set_session("s1", Some("subscription/claude/work"), Some(true))
            .unwrap();
        c.set_session("s1", Some("subscription/claude/home"), Some(false))
            .unwrap();
        assert_eq!(
            c.store.resolve(Some("s1"), None).unwrap().as_deref(),
            Some("subscription/claude/home")
        );
    }

    #[test]
    fn linked_and_managed_homes_are_different_homes() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = NoKeys::default();
        let c = ctx(
            tmp.path(),
            &keys,
            vec![
                sub("work", "/home/u/.claude-work", AccountSource::Linked),
                sub(
                    "main",
                    "/state/cli-profiles/claude/main",
                    AccountSource::Managed,
                ),
            ],
        );
        c.store
            .set_session_account("s", Some("subscription/claude/main"))
            .unwrap();
        let err = c
            .set_session("s", Some("subscription/claude/work"), None)
            .unwrap_err();
        assert_eq!(code(&err).as_deref(), Some(ACCOUNT_HOME_MISMATCH));
        // Clearing the selection of a chat that has run is also a move to the default login.
        let err = c.set_session("s", None, Some(true)).unwrap_err();
        assert_eq!(code(&err).as_deref(), Some(ACCOUNT_HOME_MISMATCH));
    }

    #[test]
    fn a_chat_that_ran_without_a_recorded_account_cannot_pick_one() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = NoKeys::default();
        let c = ctx(
            tmp.path(),
            &keys,
            vec![sub("work", "/home/u/.claude-work", AccountSource::Linked)],
        );
        let err = c
            .set_session("fresh", Some("subscription/claude/work"), Some(true))
            .unwrap_err();
        assert_eq!(code(&err).as_deref(), Some(ACCOUNT_HOME_MISMATCH));
        // A chat that has not run, or whose state is not reported, may take its first account.
        c.set_session("fresh", Some("subscription/claude/work"), Some(false))
            .unwrap();
        c.set_session("other", Some("subscription/claude/work"), None)
            .unwrap();
    }

    #[test]
    fn linking_lists_the_folder_and_refuses_a_used_label() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = NoKeys::default();
        // The folder to link sits outside the state root (a folder inside it is refused for
        // that reason first, before the label is looked at).
        let outside = tempfile::tempdir().unwrap();
        let folder = outside.path().join(".claude-work");
        std::fs::create_dir_all(&folder).unwrap();
        let c = ctx(
            tmp.path(),
            &keys,
            vec![sub(
                "main",
                "/state/cli-profiles/claude/main",
                AccountSource::Managed,
            )],
        );
        let used = c
            .link_folder("claude", "main", folder.to_str().unwrap())
            .unwrap_err();
        assert_eq!(used.0, StatusCode::CONFLICT);
        let bad = c.link_folder("claude", "x", "relative").unwrap_err();
        assert_eq!(bad.0, StatusCode::BAD_REQUEST);
        assert!(c.links.load().unwrap().links.is_empty());
    }

    #[test]
    fn the_view_names_the_account_a_chat_runs_on_and_where_the_choice_came_from() {
        let snapshot = AccountsSnapshot {
            views: vec![AccountView {
                id: "subscription/claude/work".into(),
                kind: AccountKind::Subscription,
                label: "work".into(),
                provider: "claude".into(),
                email: Some("w@example.test".into()),
                organization: None,
                fingerprint: None,
                ready: true,
                source: "linked",
                folder: Some("/home/u/.claude-work".into()),
                platform: None,
            }],
            selections: AccountSelections {
                project_defaults: [("/ws".to_string(), "subscription/claude/work".to_string())]
                    .into(),
                sessions: [("s1".to_string(), "subscription/claude/gone".to_string())].into(),
            },
        };
        let project = snapshot.view_for("s2", "/ws").unwrap();
        assert_eq!(project["from"], "project");
        assert_eq!(project["source"], "linked");
        assert_eq!(project["email"], "w@example.test");
        let gone = snapshot.view_for("s1", "/ws").unwrap();
        assert_eq!(gone["from"], "session");
        assert_eq!(gone["unavailable"], true);
        assert!(snapshot.view_for("s3", "/elsewhere").is_none());
    }

    #[test]
    fn a_failed_create_takes_the_recorded_account_back() {
        // The guard's drop path needs the real state root; the logic is the store call below.
        let tmp = tempfile::tempdir().unwrap();
        let store = AccountStore::at(tmp.path().join("accounts.json"));
        store
            .set_session_account("s", Some("subscription/claude/work"))
            .unwrap();
        store.set_session_account("s", None).unwrap();
        assert_eq!(store.resolve(Some("s"), None).unwrap(), None);
    }
}
