//! RightApp desktop shell: a thin supervisor over the engine sidecar. Windowing, dialogs, updater
//! and diagnostics live here; all product work lives in `crates/engine` behind the framed protocol.

mod diagnostics;
mod engine_supervisor;

use std::sync::Mutex;

use rightkit_tauri::{DeviceService, SecureStore};
use serde_json::Value;
use tauri::Manager;

use engine_supervisor::EngineSupervisor;

/// App data root. Tests and QA override it with `RIGHTAPP_APP_DATA`.
pub fn app_data_root() -> std::path::PathBuf {
    if let Some(v) = std::env::var_os("RIGHTAPP_APP_DATA") {
        return v.into();
    }
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("RightApp")
}

/// PLACEHOLDER store: a plain file. Licensed apps replace it with an OS-keyring `SecureStore`
/// (see ScrapeRight `license.rs` `KeyringStringStore`) so device identity survives profile wipes.
struct FileDeviceStore;
impl FileDeviceStore {
    fn path() -> std::path::PathBuf {
        app_data_root().join("device-id")
    }
}
impl SecureStore<String> for FileDeviceStore {
    fn get(&self) -> Result<Option<String>, String> {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn set(&self, value: &String) -> Result<(), String> {
        std::fs::create_dir_all(app_data_root()).map_err(|e| e.to_string())?;
        std::fs::write(Self::path(), value).map_err(|e| e.to_string())
    }
    fn delete(&self) -> Result<(), String> {
        match std::fs::remove_file(Self::path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[tauri::command]
fn device_get_or_create(device: tauri::State<'_, DeviceService<FileDeviceStore>>) -> Result<String, String> {
    device.get_or_create().map_err(|e| e.to_string())
}

#[tauri::command]
fn engine_call(
    engine: tauri::State<'_, Mutex<EngineSupervisor>>,
    cmd: String,
    input: Value,
) -> Result<Value, String> {
    engine.lock().map_err(|_| "engine lock poisoned".to_string())?.call(&cmd, input)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(DeviceService::new(FileDeviceStore))
        .setup(|app| {
            let supervisor = EngineSupervisor::spawn(app.handle())?;
            app.manage(Mutex::new(supervisor));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            device_get_or_create,
            engine_call,
            diagnostics::rightkit_logs_write,
            diagnostics::rightkit_logs_collect,
            diagnostics::rightkit_logs_clear,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RightApp");
}
