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
use rightkit_agents::driver_host::cli_runtime::account_identity::{
    list_subscription_accounts, SubscriptionAccount,
};
use rightkit_app_settings::agent_config::accounts::{
    account_kind_of, AccountKind, AccountStore, AccountsError, ApiKeyAccountEntry,
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
}

/// Everything the account logic touches, injectable for tests.
struct Ctx<'a> {
    store: AccountStore,
    keys: &'a dyn KeyStoreBackend,
    subscriptions: Vec<SubscriptionAccount>,
}

impl<'a> Ctx<'a> {
    fn live(keys: &'a dyn KeyStoreBackend) -> Result<Self, ApiError> {
        Ok(Self {
            store: AccountStore::default_location().map_err(accounts_err)?,
            keys,
            subscriptions: list_subscription_accounts(),
        })
    }

    fn list(&self) -> Result<Vec<AccountView>, ApiError> {
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
            })
            .collect();
        for entry in self.store.load().map_err(accounts_err)?.api_key_accounts {
            let ready = account_key_present_with(self.keys, &entry.id).unwrap_or(false);
            out.push(AccountView {
                id: entry.id,
                kind: AccountKind::ApiKey,
                label: entry.label,
                provider: entry.provider,
                email: None,
                organization: None,
                fingerprint: None,
                ready,
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
        Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "this chat already runs under an account with a different config home; start a new session to use another account",
                "code": ACCOUNT_HOME_MISMATCH,
                "action": "new_session",
                "currentAccountId": current,
                "requestedAccountId": new_account,
            })),
        ))
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
            "account": account,
            "unavailable": id.is_some() && account.is_none(),
        }))
    }
}

fn bad_request(message: &str) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn accounts_err(error: AccountsError) -> ApiError {
    let status = match error {
        AccountsError::InvalidLabel(_)
        | AccountsError::InvalidProvider(_)
        | AccountsError::InvalidAccountId(_) => StatusCode::BAD_REQUEST,
        AccountsError::UnknownAccount(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() })))
}

fn os_keys() -> rightkit_llm::agent_providers::OsKeyStore {
    rightkit_llm::agent_providers::OsKeyStore
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
    let account = ctx.find(&entry.id)?;
    Ok((StatusCode::CREATED, Json(json!({ "account": account }))))
}

async fn remove_api_key_account(
    Path((provider, slug)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let keys = os_keys();
    let id = rightkit_app_settings::agent_config::accounts::api_key_account_id(&provider, &slug);
    Ctx::live(&keys)?.remove_api_key(&id)?;
    Ok(Json(json!({ "ok": true })))
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
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agents::driver_host::cli_runtime::account_identity::AccountIdentity;
    use rightkit_agents::driver_host::cli_runtime::CliAuthMode;
    use rightkit_llm::agent_providers::InMemoryKeyStore;

    fn sub(label: &str, signed_in: bool, email: Option<&str>) -> SubscriptionAccount {
        SubscriptionAccount {
            id: format!("subscription/claude/{label}"),
            provider: "claude".into(),
            label: label.into(),
            signed_in,
            auth_mode: CliAuthMode::NativeCliAccount,
            identity: email.map(|e| AccountIdentity {
                email: Some(e.into()),
                organization: Some("Acme".into()),
                ..Default::default()
            }),
            fingerprint: email.map(|e| format!("fp-{e}")),
            managed_home: std::path::PathBuf::from(format!("/homes/{label}")),
        }
    }

    fn ctx<'a>(dir: &tempfile::TempDir, keys: &'a InMemoryKeyStore) -> Ctx<'a> {
        Ctx {
            store: AccountStore::at(dir.path().join("accounts.json")),
            keys,
            subscriptions: vec![sub("work", true, Some("w@x.io")), sub("spare", false, None)],
        }
    }

    #[test]
    fn list_merges_kinds_and_never_exposes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let c = ctx(&dir, &keys);
        c.add_api_key("Team", "openai", "sk-SECRET-123").unwrap();
        let list = c.list().unwrap();
        assert_eq!(list.len(), 3);
        let work = list.iter().find(|a| a.label == "work").unwrap();
        assert_eq!(work.kind, AccountKind::Subscription);
        assert_eq!(work.email.as_deref(), Some("w@x.io"));
        assert!(work.ready);
        let team = list.iter().find(|a| a.kind == AccountKind::ApiKey).unwrap();
        assert!(team.ready);
        let wire = serde_json::to_string(&list).unwrap();
        assert!(!wire.contains("SECRET"));
        let on_disk = std::fs::read_to_string(dir.path().join("accounts.json")).unwrap();
        assert!(!on_disk.contains("SECRET"));
    }

    #[test]
    fn api_key_rows_carry_label_and_kind_never_an_email_and_codex_rows_match_the_shape() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let mut c = ctx(&dir, &keys);
        c.subscriptions.push(SubscriptionAccount {
            id: "subscription/codex/personal".into(),
            provider: "codex".into(),
            label: "personal".into(),
            signed_in: true,
            auth_mode: CliAuthMode::NativeCliAccount,
            identity: None,
            fingerprint: None,
            managed_home: "/homes/codex-personal".into(),
        });
        c.add_api_key("Personal", "claude", "sk-ant-x").unwrap();
        let list = c.list().unwrap();
        let api = list.iter().find(|a| a.kind == AccountKind::ApiKey).unwrap();
        assert_eq!(api.label, "Personal");
        assert_eq!(api.email, None);
        let codex = list.iter().find(|a| a.provider == "codex").unwrap();
        assert_eq!(codex.kind, AccountKind::Subscription);
        assert_eq!(codex.label, "personal");
        assert_eq!(codex.email, None);
        let wire = serde_json::to_value(codex).unwrap();
        assert_eq!(wire["kind"], "subscription");
        assert_eq!(wire["id"], "subscription/codex/personal");
    }

    #[test]
    fn failed_key_store_rolls_back_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::failing("keychain locked");
        let c = ctx(&dir, &keys);
        assert!(c.add_api_key("Team", "openai", "sk-x").is_err());
        assert!(c.store.load().unwrap().api_key_accounts.is_empty());
        assert!(c.add_api_key("Team", "openai", "  ").is_err());
    }

    #[test]
    fn remove_deletes_key_and_selections() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let c = ctx(&dir, &keys);
        let e = c.add_api_key("Team", "openai", "sk-x").unwrap();
        c.set_session("s1", Some(&e.id), None).unwrap();
        c.remove_api_key(&e.id).unwrap();
        assert!(!account_key_present_with(&keys, &e.id).unwrap());
        let sel = c.selection(Some("s1"), None).unwrap();
        assert_eq!(sel["source"], "none");
    }

    #[test]
    fn selection_is_session_then_project_and_rejects_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let c = ctx(&dir, &keys);
        let e = c.add_api_key("Team", "openai", "sk-x").unwrap();
        c.set_project("/p", Some("subscription/claude/work"))
            .unwrap();
        c.set_session("s1", Some(&e.id), None).unwrap();
        assert_eq!(
            c.selection(Some("s1"), Some("/p")).unwrap()["source"],
            "session"
        );
        assert_eq!(
            c.selection(Some("s2"), Some("/p")).unwrap()["accountId"],
            "subscription/claude/work"
        );
        assert!(c
            .set_session("s1", Some("subscription/claude/ghost"), None)
            .is_err());
        assert!(c.set_session("s1", Some("garbage"), None).is_err());
    }

    fn mismatch_code(error: ApiError) -> (StatusCode, String) {
        (
            error.0,
            error.1 .0["code"].as_str().unwrap_or_default().to_string(),
        )
    }

    #[test]
    fn cross_home_switch_mid_thread_is_refused_with_a_new_session_offer() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let mut c = ctx(&dir, &keys);
        c.subscriptions.push(sub("personal", true, Some("p@x.io")));
        // First choice is free.
        c.set_session("s1", Some("subscription/claude/work"), None)
            .unwrap();
        // Same account again is a no-op.
        c.set_session("s1", Some("subscription/claude/work"), None)
            .unwrap();
        // A different home is refused, and the selection stays where it was.
        let err = c
            .set_session("s1", Some("subscription/claude/personal"), None)
            .unwrap_err();
        assert_eq!(
            mismatch_code(err),
            (StatusCode::CONFLICT, ACCOUNT_HOME_MISMATCH.to_string())
        );
        let sel = c.selection(Some("s1"), None).unwrap();
        assert_eq!(sel["accountId"], "subscription/claude/work");
        // Subscription to API key and clearing both leave the home too.
        let key = c.add_api_key("Team", "claude", "sk-ant-x").unwrap();
        assert!(c.set_session("s1", Some(&key.id), None).is_err());
        assert!(c.set_session("s1", None, None).is_err());
        let body = c
            .set_session("s1", Some("subscription/claude/personal"), None)
            .unwrap_err()
            .1
             .0;
        assert_eq!(body["action"], "new_session");
        assert_eq!(body["requestedAccountId"], "subscription/claude/personal");
        // The forked path: a brand-new session takes the other account without any rule firing.
        c.set_session("s2", Some("subscription/claude/personal"), None)
            .unwrap();
    }

    #[test]
    fn same_home_and_not_yet_started_switches_are_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let mut c = ctx(&dir, &keys);
        // Two accounts that share one config home.
        c.subscriptions.push(sub("alias", true, Some("alias@x.io")));
        c.subscriptions.last_mut().unwrap().managed_home = "/homes/work".into();
        c.set_session("s1", Some("subscription/claude/work"), None)
            .unwrap();
        c.set_session("s1", Some("subscription/claude/alias"), None)
            .unwrap();
        // Two API-key accounts of one provider share the ambient home.
        let a = c.add_api_key("A", "claude", "sk-a").unwrap();
        let b = c.add_api_key("B", "claude", "sk-b").unwrap();
        c.set_session("s2", Some(&a.id), None).unwrap();
        c.set_session("s2", Some(&b.id), None).unwrap();
        // A chat that has not started may still pick any account.
        c.set_session("s3", Some("subscription/claude/work"), None)
            .unwrap();
        c.set_session("s3", Some("subscription/claude/spare"), Some(false))
            .unwrap();
    }

    #[test]
    fn list_carries_the_fingerprint_and_never_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let c = ctx(&dir, &keys);
        let list = c.list().unwrap();
        let work = list.iter().find(|a| a.label == "work").unwrap();
        assert_eq!(work.fingerprint.as_deref(), Some("fp-w@x.io"));
        assert!(!serde_json::to_string(&list).unwrap().contains("/homes/"));
    }

    #[test]
    fn vanished_selection_is_unavailable_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryKeyStore::new();
        let mut c = ctx(&dir, &keys);
        c.set_session("s1", Some("subscription/claude/work"), None)
            .unwrap();
        c.subscriptions.clear();
        let sel = c.selection(Some("s1"), None).unwrap();
        assert_eq!(sel["unavailable"], true);
        assert!(sel["account"].is_null());
    }
}
