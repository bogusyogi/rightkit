//! RightApp desktop shell. `rightkit-tauri-shell` hosts the window (hardened webview, window state, single
//! instance, tray and menu hooks, the renderer log commands). `rightkit-job-queue` supervises the
//! `rightapp-engine` sidecar: restart budget, per-call deadline, no replay of a timed-out call. The product's
//! work lives in `crates/engine` behind the rightkit-job-queue contract. The data root comes from
//! `rightapp_protocol::app_data_root`, which goes through `rightkit_daemon::mode` (nothing in the environment
//! can move it in a release build). `services` registers the settings, legal-receipt, licence and HTTP owners under
//! the shell's host caller policy.

pub mod services;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rightapp_protocol::app_data_root;
use rightkit_child_process::{OwnedCommand, RestartMode, RestartPolicy};
use rightkit_crash::{install_process_hook, CrashConfig, ProcessKind};
use rightkit_credentials::{Config, Namespace, Store};
use rightkit_device::{credentials::CredentialStore, DeviceService};
use rightkit_job_queue::{EngineSupervisor, SupervisorConfig};
use rightkit_tauri_shell::logs_commands::{self, LogsConfig};
use rightkit_tauri_shell::Shell;
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

/// A QA build (`qa-native`) launches the engine named by `RIGHTAPP_ENGINE_BIN`, because a hidden QA run starts the
/// bare executable, not an installed bundle. The read does not exist in any other build: the name is compiled out
/// (switches.json, class S3), so a packaged app never launches an executable named by its environment.
#[cfg(feature = "qa-native")]
fn engine_override() -> Option<PathBuf> {
    std::env::var_os("RIGHTAPP_ENGINE_BIN").map(PathBuf::from)
}

#[cfg(not(feature = "qa-native"))]
fn engine_override() -> Option<PathBuf> {
    None
}

/// Packaged: Tauri places `externalBin` next to the executable (triple suffix stripped). A QA build may name the
/// engine through `RIGHTAPP_ENGINE_BIN` (see `engine_override`).
fn engine_path(app: &tauri::AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = engine_override() {
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

/// The app's credential store (namespace = bundle identifier): the device identity and the licence record.
fn credential_store() -> Result<Store, String> {
    let namespace = Namespace::new(DEVICE_SERVICE).map_err(|e| e.to_string())?;
    Ok(Store::open(Config::new(namespace)))
}

/// Device identity lives in the OS credential store through `rightkit-credentials` (adapted by
/// `rightkit-device`), so it survives profile wipes and is not a readable file. Read failures are errors,
/// never absence.
fn device_service(store: Store) -> Result<Arc<DeviceService<CredentialStore>>, String> {
    let store = CredentialStore::new(store, DEVICE_ACCOUNT).map_err(|e| e.to_string())?;
    Ok(Arc::new(DeviceService::new(store)))
}

#[tauri::command]
fn device_get_or_create(
    device: tauri::State<'_, Arc<DeviceService<CredentialStore>>>,
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
    // The release-or-test decision comes first (inside app_data_root), before any store, log or credential opens.
    let data_root = app_data_root().expect("RightApp data root");
    install_process_hook(
        ProcessKind::Window,
        CrashConfig::new(
            "RightApp",
            env!("CARGO_PKG_VERSION"),
            data_root.join("crashes"),
        ),
    );
    // The shell plugin goes first so single-instance arbitration runs before any other plugin works.
    let shell = Shell::builder("rightapp")
        .app_name("RightApp")
        .frameless(true)
        .state_dir(data_root.join("shell"))
        .build()
        .plugin();
    let builder = tauri::Builder::default().plugin(shell);
    let store = credential_store().expect("credential store");
    let device = device_service(store.clone()).expect("device identity store");
    let license = services::license_setup(services::LICENSE.as_ref(), store, &data_root)
        .and_then(|setup| services::license_client(setup, device.clone()))
        .expect("licence client");
    // QA control never ships: this registration exists only in a `qa-native` build, which right-release refuses to seal.
    #[cfg(feature = "qa-native")]
    let builder = match rightkit_qa::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
        Some(control) => builder.plugin(control),
        None => builder,
    };
    builder
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(device)
        .setup(move |app| {
            // Settings, legal receipt and licence, under one caller policy (main window only). Installed before
            // the log commands, which then ask the same policy.
            services::host_services(&data_root, license)?.install(app.handle())?;
            if let Some(http) = services::app_http(services::LICENSE.as_ref()) {
                app.manage(http);
            }
            let mut logs = LogsConfig::new(
                "RightApp",
                env!("CARGO_PKG_VERSION"),
                data_root
                    .join("logs")
                    .join("rightkit-renderer.events.jsonl"),
            );
            logs.build = if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into();
            logs_commands::install(app.handle(), logs)?;
            let engine = EngineSupervisor::new(engine_config(engine_path(app.handle())?)?);
            engine.start()?;
            app.manage(Arc::new(engine));
            Ok(())
        })
        // The shell's handler! registers its own commands and, with the `logs` feature, rightkit_logs_write/collect/clear.
        .invoke_handler(rightkit_tauri_shell::handler![
            device_get_or_create,
            engine_call,
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

#[cfg(all(test, not(feature = "qa-native")))]
mod engine_override_tests {
    use super::engine_override;

    // rk-test: pure
    #[test]
    fn a_build_without_qa_native_never_honours_an_engine_override() {
        assert_eq!(engine_override(), None);
    }
}
