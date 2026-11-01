//! Tauri commands for the demo UI (design, section 1). Every command is `async`: a synchronous
//! command runs on the main thread, and several of these wait on the recorder or the main thread
//! (the kill hotkey is installed there), which would deadlock.
//!
//! None of them takes a URL, a token or a path the webview chose: a bundle is named by its folder
//! name and resolved under the demo bundles folder. The session token never leaves the host.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use super::host::DemoHost;
use super::{BundleDto, PrepareDto, StatusDto};

type Host<'a> = State<'a, Arc<DemoHost>>;

#[derive(Serialize)]
pub struct CmdError {
    pub code: String,
    pub message: String,
}

fn err(message: impl Into<String>) -> CmdError {
    CmdError {
        code: "demo".into(),
        message: message.into(),
    }
}

/// Runs blocking host work off the async runtime's workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, CmdError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| err(e.to_string()))
}

#[derive(Serialize)]
pub struct CatalogItem {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub async fn demo_status(host: Host<'_>) -> Result<StatusDto, CmdError> {
    Ok(host.status())
}

#[tauri::command]
pub async fn demo_set_enabled(host: Host<'_>, enabled: bool) -> Result<StatusDto, CmdError> {
    let h = host.inner().clone();
    blocking(move || h.set_enabled(enabled).map(|()| h.status()))
        .await?
        .map_err(err)
}

#[tauri::command]
pub async fn demo_catalog(host: Host<'_>) -> Result<Vec<CatalogItem>, CmdError> {
    Ok(host
        .catalog()
        .apps
        .into_iter()
        .map(|a| CatalogItem {
            id: a.id,
            name: a.name,
        })
        .collect())
}

#[tauri::command]
pub async fn demo_prepare(host: Host<'_>) -> Result<PrepareDto, CmdError> {
    let h = host.inner().clone();
    blocking(move || h.prepare()).await
}

#[tauri::command]
pub async fn demo_consent_respond(
    host: Host<'_>,
    session: String,
    approve: bool,
) -> Result<(), CmdError> {
    let h = host.inner().clone();
    blocking(move || h.respond(&session, approve))
        .await?
        .map_err(err)
}

#[tauri::command]
pub async fn demo_begin_person(host: Host<'_>, app: String) -> Result<(), CmdError> {
    let h = host.inner().clone();
    blocking(move || h.begin_person(&app)).await?.map_err(err)
}

#[tauri::command]
pub async fn demo_stop(host: Host<'_>) -> Result<Option<BundleDto>, CmdError> {
    let h = host.inner().clone();
    blocking(move || h.stop()).await?.map_err(err)
}

/// The on-screen kill. The same path as the hotkey: the process-wide switch first.
#[tauri::command]
pub async fn demo_kill(host: Host<'_>) -> Result<(), CmdError> {
    let h = host.inner().clone();
    blocking(move || h.kill("kill button")).await
}

#[tauri::command]
pub async fn demo_bundle_reveal(host: Host<'_>, name: String) -> Result<(), CmdError> {
    host.bundle_reveal(&name).map_err(err)
}

#[tauri::command]
pub async fn demo_bundle_discard(host: Host<'_>, name: String) -> Result<(), CmdError> {
    let h = host.inner().clone();
    blocking(move || h.bundle_discard(&name))
        .await?
        .map_err(err)
}
