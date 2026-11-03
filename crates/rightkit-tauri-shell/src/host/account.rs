//! The native account session commands (feature `license`, #1032): the shell owns them, apps do not.
//!
//! Nine commands over `rightkit_license::AccountSessionClient` (`status`, `refresh`,
//! `request_sign_in`, `complete_sign_in`, `sign_out`, `list_devices`, `rename_device`,
//! `remove_device`, `delete`) are what `createAccountClient` in `@rightkit/license` calls. An app
//! builds its `AccountSessionClient` over its licence client, registers it with
//! [`HostServicesBuilder::account_client`](super::HostServicesBuilder::account_client) and lists
//! nothing else: `handler!` carries the commands. The device-link pair stays on the licence
//! commands (`rightkit_license_link_request`, `rightkit_license_link_redeem`).
//!
//! # Secrets stay native
//!
//! The session token is held by the client in the OS credential store and never crosses this
//! module. The sign-in code and the e-mail enter once as arguments and are checked here (length and
//! control characters) before the client sees them. The commands answer only the client's
//! fixed-schema views and reject with its closed `AccountCommandError` (`{ code, reason? }`) or,
//! for a refused caller or an app with no account service, a [`HostError`] (`forbidden`,
//! `unsupported`); no message carries server text.
//!
//! # Who is calling
//!
//! The first call of every command is [`account_service`] over the facts of the invoking `Webview`
//! (label and current URL from Tauri's native context, never an argument). A caller the
//! [`HostCallerPolicy`](super::HostCallerPolicy) does not trust for the action is refused before the
//! client is touched.
//!
//! # Deleting the account
//!
//! `rightkit_account_delete` takes `confirmed: true` and builds the client's
//! `DeleteAccountConfirmation` from it; any other value is refused as `rejected` / `invalidInput`
//! without a client call. The page asks the person first; this command is the only place the proof
//! is made.

use std::sync::Arc;

use rightkit_license::{
    AccountCommandError, AccountDeletion, AccountDeviceView, AccountError, AccountErrorCode,
    AccountRejectReason, AccountSessionClient, AccountStatusView, DeleteAccountConfirmation,
    SecretString,
};
use serde::Serialize;
use tauri::{Manager, Webview, Wry};

use super::policy::ServiceId;
use super::{HostError, HostServices};
use crate::caller_gate::{self, CallerError, CallerFacts, Denial};

/// The longest sign-in e-mail address (characters) the commands accept.
pub const MAX_EMAIL_CHARS: usize = 320;

/// The longest sign-in code or link token (characters) the commands accept.
pub const MAX_CODE_CHARS: usize = 4096;

/// The registered account service: the client behind the nine commands.
pub struct AccountHostService {
    client: AccountSessionClient,
}

impl AccountHostService {
    pub fn new(client: AccountSessionClient) -> Self {
        Self { client }
    }

    pub fn status(&self) -> AccountStatusView {
        self.client.status()
    }

    pub fn refresh(&self) -> Result<AccountStatusView, AccountError> {
        self.client.refresh()
    }

    /// An address that is empty, too long or holds a control character is refused as
    /// `rejected` / `invalidInput` without a client call.
    pub fn request_sign_in(&self, email: &str) -> Result<(), AccountError> {
        if !plausible(email, MAX_EMAIL_CHARS) {
            return Err(invalid_input());
        }
        self.client.request_sign_in(email)
    }

    /// A code that is empty, too long or holds a control character is refused as `rejected` /
    /// `invalidInput` without a client call.
    pub fn complete_sign_in(&self, code: String) -> Result<AccountStatusView, AccountError> {
        if !plausible(&code, MAX_CODE_CHARS) {
            return Err(invalid_input());
        }
        self.client.complete_sign_in(SecretString::new(code))
    }

    pub fn sign_out(&self) -> Result<AccountStatusView, AccountError> {
        self.client.sign_out()
    }

    pub fn list_devices(&self) -> Result<Vec<AccountDeviceView>, AccountError> {
        self.client.list_devices()
    }

    pub fn rename_device(&self, device: &str, label: &str) -> Result<(), AccountError> {
        self.client.rename_device(device, label)
    }

    pub fn remove_device(&self, device: &str) -> Result<(), AccountError> {
        self.client.remove_device(device)
    }

    /// Delete the account. Only `confirmed == true` builds the confirmation proof.
    pub fn delete(&self, confirmed: bool) -> Result<AccountDeletion, AccountError> {
        if !confirmed {
            return Err(invalid_input());
        }
        self.client
            .delete_account(DeleteAccountConfirmation::confirmed_by_user())
    }
}

fn invalid_input() -> AccountError {
    AccountError::Rejected {
        reason: AccountRejectReason::InvalidInput,
    }
}

/// Non-empty, within `max` characters, no control character.
fn plausible(text: &str, max: usize) -> bool {
    !text.is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// What an account command rejects with: the client's closed `{ code, reason? }`, or a host
/// refusal (`forbidden`, `unsupported`, `other`) in the `{ code, message }` shape.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AccountHostError {
    Account(AccountCommandError),
    Host(HostError),
}

impl From<AccountError> for AccountHostError {
    fn from(error: AccountError) -> Self {
        Self::Account(error.to_command_error())
    }
}

impl AccountHostError {
    fn forbidden() -> Self {
        Self::Host(HostError::new(
            "forbidden",
            "This window cannot manage the account.",
        ))
    }

    fn unsupported() -> Self {
        Self::Host(HostError::unsupported(
            "This feature is not available in this app.",
        ))
    }

    /// An account call that never reached the client: it never ran.
    fn unresolved_task() -> Self {
        Self::Account(AccountCommandError {
            code: AccountErrorCode::Unresolved,
            reason: None,
        })
    }
}

/// The account service `facts` may use for `action`; the first call of every account command. A
/// refused caller is `forbidden`; an app with no account service registered, or one registered as
/// another type, is `unsupported`.
pub fn account_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
) -> Result<Arc<AccountHostService>, AccountHostError> {
    match host.policy().decide(ServiceId::ACCOUNT, action, facts) {
        Ok(()) => {}
        Err(CallerError::Forbidden(Denial::ServiceNotRegistered)) => {
            return Err(AccountHostError::unsupported())
        }
        Err(CallerError::Forbidden(_)) => return Err(AccountHostError::forbidden()),
    }
    host.service::<AccountHostService>(ServiceId::ACCOUNT)
        .ok_or_else(AccountHostError::unsupported)
}

fn account_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<AccountHostService>, AccountHostError> {
    let host = super::managed(webview.app_handle()).map_err(|_| AccountHostError::unsupported())?;
    let owned = caller_gate::gather(webview).map_err(|_| AccountHostError::forbidden())?;
    account_for(&host, action, &owned.facts())
}

/// Run blocking client work off the command thread. A panic in it leaves the stored session
/// unproven, so it is answered as `unresolved`.
async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AccountError> + Send + 'static,
) -> Result<T, AccountHostError> {
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result.map_err(AccountHostError::from),
        Err(_) => Err(AccountHostError::unresolved_task()),
    }
}

/// The account session as stored. No network.
#[tauri::command]
pub async fn rightkit_account_status(
    webview: Webview<Wry>,
) -> Result<AccountStatusView, AccountHostError> {
    let service = account_service(&webview, "status")?;
    off_thread(move || Ok(service.status())).await
}

/// Confirm the session with the service and refresh the stored e-mail.
#[tauri::command]
pub async fn rightkit_account_refresh(
    webview: Webview<Wry>,
) -> Result<AccountStatusView, AccountHostError> {
    let service = account_service(&webview, "refresh")?;
    off_thread(move || service.refresh()).await
}

/// E-mail a sign-in link or code (`{ email }`). Answers nothing: the service never says whether the
/// address has an account.
#[tauri::command]
pub async fn rightkit_account_request_sign_in(
    webview: Webview<Wry>,
    email: String,
) -> Result<(), AccountHostError> {
    let service = account_service(&webview, "request_sign_in")?;
    off_thread(move || service.request_sign_in(&email)).await
}

/// Complete the sign-in with the code from the e-mail (`{ code }`; zeroed after use).
#[tauri::command]
pub async fn rightkit_account_complete_sign_in(
    webview: Webview<Wry>,
    code: String,
) -> Result<AccountStatusView, AccountHostError> {
    let service = account_service(&webview, "complete_sign_in")?;
    off_thread(move || service.complete_sign_in(code)).await
}

/// Sign out. Never touches the licence.
#[tauri::command]
pub async fn rightkit_account_sign_out(
    webview: Webview<Wry>,
) -> Result<AccountStatusView, AccountHostError> {
    let service = account_service(&webview, "sign_out")?;
    off_thread(move || service.sign_out()).await
}

/// The account's devices.
#[tauri::command]
pub async fn rightkit_account_list_devices(
    webview: Webview<Wry>,
) -> Result<Vec<AccountDeviceView>, AccountHostError> {
    let service = account_service(&webview, "list_devices")?;
    off_thread(move || service.list_devices()).await
}

/// Rename a device row (`{ device, label }`; `device` is an `AccountDeviceView::id`).
#[tauri::command]
pub async fn rightkit_account_rename_device(
    webview: Webview<Wry>,
    device: String,
    label: String,
) -> Result<(), AccountHostError> {
    let service = account_service(&webview, "rename_device")?;
    off_thread(move || service.rename_device(&device, &label)).await
}

/// Remove a device row, which frees its seat (`{ device }`).
#[tauri::command]
pub async fn rightkit_account_remove_device(
    webview: Webview<Wry>,
    device: String,
) -> Result<(), AccountHostError> {
    let service = account_service(&webview, "remove_device")?;
    off_thread(move || service.remove_device(&device)).await
}

/// Delete the account (`{ confirmed: true }`). Irreversible; see the module documentation.
#[tauri::command]
pub async fn rightkit_account_delete(
    webview: Webview<Wry>,
    confirmed: bool,
) -> Result<AccountDeletion, AccountHostError> {
    let service = account_service(&webview, "delete")?;
    off_thread(move || service.delete(confirmed)).await
}
