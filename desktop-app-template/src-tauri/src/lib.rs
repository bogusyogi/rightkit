//! RightApp desktop shell. `rightkit-tauri-shell` hosts the window (hardened webview, window state,
//! single instance, tray and menu hooks) and owns the `rightapp-engine` sidecar: it starts the engine
//! off the UI thread, answers the renderer's `rk_shell_engine_call` under a native method allowlist,
//! never replays a call lost in flight, and shuts the engine down on exit. It also registers the
//! renderer log commands (`logs`) and the watchdog's stall capture (`crash`). The product's work
//! lives in `crates/engine` behind the rightkit-job-queue contract. The data root comes from
//! `rightapp_protocol::app_data_root`.

use std::path::{Path, PathBuf};

use rightapp_protocol::{
    app_data_root, build_label, crash_dir, APP_ID, APP_NAME, METHOD_ECHO, MODE_APP,
};
use rightkit_child_process::OwnedCommand;
use rightkit_crash::{install_process_hook, CrashConfig, ProcessKind};
use rightkit_credentials::{Config, Namespace, Store};
use rightkit_device::credentials::CredentialStore;
use rightkit_device::DeviceService;
use rightkit_tauri_shell::engine::{EnginePolicy, EngineSpec};
use rightkit_tauri_shell::logs_commands::{self, LogsConfig};
use rightkit_tauri_shell::watchdog::{crash_capture, CrashCaptureOptions};
use rightkit_tauri_shell::{Shell, WatchdogConfig};

/// Sidecar binary name: `bundle.externalBin` is `binaries/rightapp-engine`.
const ENGINE_BIN: &str = "rightapp-engine";

/// Where the sidecar runs from. Packaged: Tauri places `externalBin` next to the executable (triple
/// suffix stripped). In a dev build the workspace's `cargo build -p rightapp-engine` lands beside the
/// app binary as well. Nothing is read from the environment, so a packaged app never launches an
/// executable that its environment names. Without an executable path the bare name is returned and
/// the spawn fails with a plain "not found" that the shell reports as the engine's lifecycle error.
fn engine_program(exe: Option<PathBuf>) -> PathBuf {
    let name = if cfg!(windows) {
        format!("{ENGINE_BIN}.exe")
    } else {
        ENGINE_BIN.to_owned()
    };
    match exe.as_deref().and_then(Path::parent) {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// The engine launch the shell supervises: `--stdio` serves the job-queue frames. The shell restarts a
/// dead engine on the next call and never replays a call that was lost in flight.
fn engine_spec() -> EngineSpec {
    EngineSpec::new(|| {
        let mut command = OwnedCommand::new(engine_program(std::env::current_exe().ok()));
        command.command_mut().arg("--stdio");
        command.windows_hide();
        command
    })
}

/// Which engine methods the renderer may call through `rk_shell_engine_call`. The engine serves more
/// (`ping`, `shutdown`, `job.*`); those stay native-only. Every method not listed is denied.
fn engine_policy() -> EnginePolicy {
    EnginePolicy::default().allow_methods(["doctor", METHOD_ECHO])
}

/// Crash reports of the window host land in the shared `crash` folder under the data root.
fn crash_config(data_root: &Path) -> CrashConfig {
    let mut config = CrashConfig::new(APP_NAME, env!("CARGO_PKG_VERSION"), crash_dir(data_root))
        .process_kind(ProcessKind::Window);
    config.build = Some(build_label().to_owned());
    config
}

/// Keychain service for the device identity: the bundle identifier (`tauri.conf.json` `identifier`).
/// Frozen per app; changing it orphans the stored identity.
const DEVICE_SERVICE: &str = "app.rightsuite.rightapp";
const DEVICE_ACCOUNT: &str = "device-id";

/// Device identity lives in the OS credential store through `rightkit-credentials`, so it survives
/// profile wipes and is not a readable file. `CredentialStore` creates atomically (`add_if_absent`),
/// so two creators cannot both mint an identity, and a read failure is an error, never absence.
fn device_service_with(config: Config) -> Result<DeviceService<CredentialStore>, String> {
    let store =
        CredentialStore::new(Store::open(config), DEVICE_ACCOUNT).map_err(|e| e.to_string())?;
    Ok(DeviceService::new(store))
}

fn open_device_service() -> Result<DeviceService<CredentialStore>, String> {
    let namespace = Namespace::new(DEVICE_SERVICE).map_err(|e| e.to_string())?;
    device_service_with(Config::new(namespace))
}

/// Managed state of the device command. A credential store that cannot open at launch does not stop
/// the app: the command answers with the reason and never with a made-up identity.
struct DeviceState(Result<DeviceService<CredentialStore>, String>);

/// The one native device-identity command of the `@rightkit/tauri` contract.
#[tauri::command]
fn secure_device_id_get_or_create(device: tauri::State<'_, DeviceState>) -> Result<String, String> {
    match &device.inner().0 {
        Ok(service) => service.get_or_create().map_err(|e| e.to_string()),
        Err(reason) => Err(reason.clone()),
    }
}

pub fn run() {
    // The mode decision comes before any store, log, credential or service opens.
    rightkit_daemon::mode::init_or_exit(&MODE_APP);
    let data_root = app_data_root().expect("RightApp data root");
    let crash = crash_config(&data_root);
    install_process_hook(ProcessKind::Window, crash.clone());
    // The shell plugin goes first so single-instance arbitration runs before any other plugin works.
    let shell = Shell::builder(APP_ID)
        .app_name(APP_NAME)
        .frameless(true)
        .state_dir(data_root.join("shell"))
        .watchdog(Some(WatchdogConfig::default().with_capture(crash_capture(
            crash,
            "ui_stall",
            CrashCaptureOptions::default(),
        ))))
        .engine_with_policy(engine_spec(), engine_policy())
        .build()
        .plugin();
    let builder = tauri::Builder::default().plugin(shell);
    // QA control never ships: the registration exists only in a `qa-native` build, and only a launch that
    // sets the control runtime flag starts the server (see Cargo.toml).
    #[cfg(feature = "qa-native")]
    let builder = match rightkit_qa::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
        Some(plugin) => builder.plugin(plugin),
        None => builder,
    };
    let logs_path = data_root.join("logs").join("events.jsonl");
    builder
        .plugin(tauri_plugin_os::init())
        .manage(DeviceState(open_device_service()))
        .setup(move |app| {
            let mut logs = LogsConfig::new(APP_NAME, env!("CARGO_PKG_VERSION"), logs_path);
            logs.build = build_label().to_owned();
            logs_commands::install(app.handle(), logs)?;
            Ok(())
        })
        .invoke_handler(rightkit_tauri_shell::handler![
            secure_device_id_get_or_create
        ])
        .run(tauri::generate_context!())
        .expect("error while running RightApp");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_credentials::GuardMode;

    // rk-test: pure
    #[test]
    fn the_engine_is_found_next_to_the_executable() {
        let name = if cfg!(windows) {
            "rightapp-engine.exe"
        } else {
            "rightapp-engine"
        };
        let exe = PathBuf::from("install-dir").join("rightapp");
        assert_eq!(
            engine_program(Some(exe)),
            PathBuf::from("install-dir").join(name)
        );
        assert_eq!(engine_program(None), PathBuf::from(name));
    }

    // rk-test: pure
    #[test]
    fn the_device_identity_is_minted_once_through_the_credential_store() {
        let namespace = Namespace::new("app.rightsuite.rightapp.test").unwrap();
        let service =
            device_service_with(Config::new(namespace).guard_when(true, GuardMode::Memory))
                .unwrap();
        let first = service.get_or_create().unwrap();
        assert_eq!(first.len(), 36);
        assert_eq!(service.get_or_create().unwrap(), first);
        assert_eq!(service.get().unwrap().as_deref(), Some(first.as_str()));
    }

    // rk-test: pure
    #[test]
    fn a_denied_credential_store_is_an_error_not_a_minted_identity() {
        let namespace = Namespace::new("app.rightsuite.rightapp.denied").unwrap();
        let service =
            device_service_with(Config::new(namespace).guard_when(true, GuardMode::Deny)).unwrap();
        assert!(service.get_or_create().is_err());
    }
}
