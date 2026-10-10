//! RightApp desktop shell. `rightkit-shell` hosts the window (hardened webview, window state, single
//! instance, tray and menu hooks). `rightkit-jobs` supervises the `rightapp-engine` sidecar: restart
//! budget, per-call deadline, no replay of a timed-out call. The product's work lives in `crates/engine`
//! behind the rightkit-jobs contract. The data root comes from `rightapp_protocol::app_data_root`.

mod diagnostics;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rightapp_protocol::app_data_root;
use rightkit_jobs::{EngineSupervisor, SupervisorConfig};
use rightkit_process::{OwnedCommand, RestartMode, RestartPolicy};
use rightkit_shell::Shell;
use rightkit_tauri::{DeviceService, SecureStore};
use serde_json::Value;
use tauri::Manager;

/// Sidecar binary name: `bundle.externalBin` is `binaries/rightapp-engine`.
const ENGINE_BIN: &str = "rightapp-engine";

/// Engine launch for the supervisor. The engine resolves its own data root, so the shell passes nothing
/// app-specific in the environment. A call that outlives its deadline kills the engine and fails; it is
/// never replayed. Restarts are on demand, within 5 per minute.
pub fn engine_config(engine: PathBuf) -> Result<SupervisorConfig, String> {
    let restart = RestartPolicy::new(
        RestartMode::OnDemand,
        5,
        Duration::from_secs(60),
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .map_err(|e| e.to_string())?;
    Ok(SupervisorConfig::new(ENGINE_BIN, move || {
        let mut command = OwnedCommand::new(&engine);
        command.command_mut().arg("--stdio");
        command.windows_hide();
        Ok(command)
    })
    .with_restart(restart)
    .with_rpc_timeout(Duration::from_secs(30)))
}

/// Packaged: Tauri places `externalBin` next to the executable (triple suffix stripped). Dev and QA
/// override the path with `RIGHTAPP_ENGINE_BIN`.
fn engine_path(app: &tauri::AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = std::env::var_os("RIGHTAPP_ENGINE_BIN") {
        return Ok(p.into());
    }
    let name = if cfg!(windows) { "rightapp-engine.exe" } else { "rightapp-engine" };
    let exe_dir = std::env::current_exe()?.parent().map(|p| p.to_path_buf()).ok_or("no exe dir")?;
    let packaged = exe_dir.join(name);
    if packaged.is_file() {
        return Ok(packaged);
    }
    Ok(app.path().resource_dir()?.join(name))
}

/// Device identity is private state under the app data root.
/// PLACEHOLDER store: a plain file. Licensed apps replace it with an OS-keyring `SecureStore`
/// (see ScrapeRight `license.rs` `KeyringStringStore`) so device identity survives profile wipes.
struct FileDeviceStore;

impl FileDeviceStore {
    fn path() -> Result<PathBuf, String> {
        Ok(app_data_root()?.join("device-id"))
    }
}

impl SecureStore<String> for FileDeviceStore {
    fn get(&self) -> Result<Option<String>, String> {
        match std::fs::read_to_string(Self::path()?) {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, value: &String) -> Result<(), String> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, value).map_err(|e| e.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        match std::fs::remove_file(Self::path()?) {
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

/// One engine call. The blocking supervisor call runs off the UI thread, so a slow command never
/// stalls the window.
#[tauri::command]
async fn engine_call(engine: tauri::State<'_, Arc<EngineSupervisor>>, cmd: String, input: Value) -> Result<Value, String> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine.call(&cmd, input).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    let data_root = app_data_root().expect("RightApp data root");
    // The shell plugin goes first so single-instance arbitration runs before any other plugin works.
    let shell = Shell::builder("rightapp")
        .app_name("RightApp")
        .frameless(true)
        .state_dir(data_root.join("shell"))
        .build()
        .plugin();
    tauri::Builder::default()
        .plugin(shell)
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(DeviceService::new(FileDeviceStore))
        .setup(|app| {
            let engine = EngineSupervisor::new(engine_config(engine_path(app.handle())?)?);
            engine.start()?;
            app.manage(Arc::new(engine));
            Ok(())
        })
        .invoke_handler(rightkit_shell::handler![
            device_get_or_create,
            engine_call,
            diagnostics::rightkit_logs_write,
            diagnostics::rightkit_logs_collect,
            diagnostics::rightkit_logs_clear,
        ])
        .build(tauri::generate_context!())
        .expect("error while building RightApp")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(engine) = app.try_state::<Arc<EngineSupervisor>>() {
                    engine.shutdown();
                }
            }
        });
}
