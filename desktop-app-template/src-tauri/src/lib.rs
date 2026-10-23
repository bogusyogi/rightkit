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
use rightkit_secrets::{Config, Namespace, Store};
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

/// Engine path override from `RIGHTAPP_ENGINE_BIN`, honoured only when `allowed`. A release build
/// passes `false`, so a packaged app never launches an executable named by its environment.
fn engine_override(value: Option<std::ffi::OsString>, allowed: bool) -> Option<PathBuf> {
    if allowed {
        value.map(PathBuf::from)
    } else {
        None
    }
}

/// Packaged: Tauri places `externalBin` next to the executable (triple suffix stripped). Debug builds
/// (dev and QA) may override the path with `RIGHTAPP_ENGINE_BIN`; release builds ignore it.
fn engine_path(app: &tauri::AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = engine_override(
        std::env::var_os("RIGHTAPP_ENGINE_BIN"),
        cfg!(debug_assertions),
    ) {
        return Ok(p);
    }
    let name = if cfg!(windows) {
        "rightapp-engine.exe"
    } else {
        "rightapp-engine"
    };
    let exe_dir = std::env::current_exe()?
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or("no exe dir")?;
    let packaged = exe_dir.join(name);
    if packaged.is_file() {
        return Ok(packaged);
    }
    Ok(app.path().resource_dir()?.join(name))
}

/// Keychain service for the device identity: the bundle identifier (`tauri.conf.json` `identifier`).
/// Frozen per app; changing it orphans the stored identity.
const DEVICE_SERVICE: &str = "app.rightsuite.rightapp";
const DEVICE_ACCOUNT: &str = "device-id";

/// Device identity lives in the OS credential store through `rightkit-secrets`, so it survives
/// profile wipes and is not a readable file. Read failures are errors, never absence.
struct KeyringDeviceStore {
    store: Store,
}

impl KeyringDeviceStore {
    fn new() -> Result<Self, String> {
        let namespace = Namespace::new(DEVICE_SERVICE).map_err(|e| e.to_string())?;
        Ok(Self::with_config(Config::new(namespace)))
    }

    fn with_config(config: Config) -> Self {
        Self {
            store: Store::open(config),
        }
    }
}

impl SecureStore<String> for KeyringDeviceStore {
    fn get(&self) -> Result<Option<String>, String> {
        match self.store.get(DEVICE_ACCOUNT).map_err(|e| e.to_string())? {
            Some(secret) => Ok(Some(
                secret
                    .expose_str()
                    .map_err(|e| e.to_string())?
                    .trim()
                    .to_string(),
            )),
            None => Ok(None),
        }
    }

    fn set(&self, value: &String) -> Result<(), String> {
        self.store
            .set_str(DEVICE_ACCOUNT, value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        self.store
            .delete(DEVICE_ACCOUNT)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn device_get_or_create(
    device: tauri::State<'_, DeviceService<KeyringDeviceStore>>,
) -> Result<String, String> {
    device.get_or_create().map_err(|e| e.to_string())
}

/// One engine call. The blocking supervisor call runs off the UI thread, so a slow command never
/// stalls the window.
#[tauri::command]
async fn engine_call(
    engine: tauri::State<'_, Arc<EngineSupervisor>>,
    cmd: String,
    input: Value,
) -> Result<Value, String> {
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        engine.call(&cmd, input).map_err(|e| e.to_string())
    })
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
    let builder = tauri::Builder::default();
    // QA control never ships: see the `qa-native` pattern in Cargo.toml. Once adopted, uncomment:
    // #[cfg(feature = "qa-native")]
    // let builder = match rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
    //     Some(control) => builder.plugin(control), // logs "RIGHTKIT QA CONTROL SERVER ACTIVE" when the launcher sets RIGHTKIT_CONTROL_SERVICE
    //     None => builder,
    // };
    builder
        .plugin(shell)
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(DeviceService::new(
            KeyringDeviceStore::new().expect("device identity store"),
        ))
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

#[cfg(test)]
mod engine_override_tests {
    use super::engine_override;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn a_release_build_ignores_the_engine_override() {
        assert_eq!(
            engine_override(Some(OsString::from("/tmp/evil")), false),
            None
        );
    }

    #[test]
    fn a_debug_build_honours_the_engine_override() {
        assert_eq!(
            engine_override(Some(OsString::from("/tmp/dev-engine")), true),
            Some(PathBuf::from("/tmp/dev-engine"))
        );
        assert_eq!(engine_override(None, true), None);
    }
}

#[cfg(test)]
mod device_store_tests {
    use super::*;
    use rightkit_secrets::GuardMode;

    fn memory_store() -> KeyringDeviceStore {
        let namespace = Namespace::new("app.rightsuite.rightapp.test").unwrap();
        KeyringDeviceStore::with_config(Config::new(namespace).guard_when(true, GuardMode::Memory))
    }

    #[test]
    fn device_identity_round_trips_through_the_secret_store_and_not_a_file() {
        let store = memory_store();
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
        store.set(&"device-123".to_string()).unwrap();
        assert_eq!(store.get().unwrap().as_deref(), Some("device-123"));
        store.delete().unwrap();
        assert_eq!(store.get().unwrap(), None);
    }

    #[test]
    fn a_denied_secret_store_is_an_error_not_an_absent_identity() {
        let namespace = Namespace::new("app.rightsuite.rightapp.denied").unwrap();
        let store = KeyringDeviceStore::with_config(
            Config::new(namespace).guard_when(true, GuardMode::Deny),
        );
        assert!(store.get().is_err());
    }
}
