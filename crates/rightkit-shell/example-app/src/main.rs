//! Tiny example app proving `rightkit-shell`: hardened webview, native menu, tray, single
//! instance, deep links, state restore, autostart, rich clipboard, watchdog, engine wiring.
//! The same executable doubles as the engine sidecar (`--engine`) so a QA bundle is self-contained.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use rightkit_child_process::OwnedCommand;
use rightkit_control::embedded::Control;
use rightkit_shell::engine::EngineSpec;
use rightkit_shell::{
    ExternalOpener, MenuEntry, MenuGroup, MenuSpec, RecoverySpec, Shell, StreamPolicy, TraySpec,
    WatchdogConfig,
};
use serde_json::json;

/// Records the link instead of launching a browser (tests must not open real applications).
struct LogOpener(PathBuf);
impl ExternalOpener for LogOpener {
    fn open(&self, target: &str) -> Result<(), String> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{target}").map_err(|e| e.to_string())
    }
}

fn engine_main() {
    use rightkit_sidecar::{read_json_line, write_json_line};
    use serde_json::Value;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let limit = 1 << 20;
    write_json_line(&mut out, &json!({"type": "hello", "engine": "shell-example-engine", "version": "0.1.0", "pid": std::process::id()}), limit).ok();
    let mut buf = Vec::new();
    while let Ok(Some(req)) = read_json_line::<_, Value>(&mut reader, &mut buf, limit) {
        let id = req["id"].clone();
        match req["method"].as_str().unwrap_or("") {
            "echo" => {
                write_json_line(&mut out, &json!({"type": "response", "id": id, "ok": true, "result": {"echo": req["params"], "pid": std::process::id()}}), limit).ok();
            }
            "stream" => {
                for n in 0..3 {
                    write_json_line(
                        &mut out,
                        &json!({"type": "event", "id": id, "event": {"tick": n}}),
                        limit,
                    )
                    .ok();
                }
                write_json_line(
                    &mut out,
                    &json!({"type": "response", "id": id, "ok": true, "result": {"ticks": 3}}),
                    limit,
                )
                .ok();
            }
            "shutdown" => {
                write_json_line(
                    &mut out,
                    &json!({"type": "response", "id": id, "ok": true, "result": {}}),
                    limit,
                )
                .ok();
                return;
            }
            other => {
                write_json_line(&mut out, &json!({"type": "response", "id": id, "ok": false, "error": {"code": "unknown_method", "message": other}}), limit).ok();
            }
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--engine") {
        return engine_main();
    }
    let mut shell = Shell::builder("rk-shell-example")
        .app_name("RkShellExample")
        .deep_link_scheme("rkshell-example")
        .menu(MenuSpec {
            groups: vec![MenuGroup {
                label: "Demo".into(),
                entries: vec![MenuEntry::accel("demo.ping", "Ping", "CmdOrCtrl+Shift+P")],
            }],
            help: vec![MenuEntry::item("help.docs", "Documentation")],
            settings: Some(("app.settings".into(), "CmdOrCtrl+,".into())),
            ..Default::default()
        })
        .tray(TraySpec {
            tooltip: "RkShellExample".into(),
            items: vec![MenuEntry::item("tray.status", "Status")],
            ..Default::default()
        })
        .watchdog(Some(WatchdogConfig {
            threshold: Duration::from_millis(900),
            heartbeat_interval: Duration::from_millis(150),
            on_stall: Some(std::sync::Arc::new(|r| {
                if let Some(p) = std::env::var_os("RIGHTKIT_EXAMPLE_STALL_LOG") {
                    let _ = std::fs::write(
                        p,
                        format!(
                            "stalled_ms={} threshold_ms={}\n",
                            r.stalled_ms, r.threshold_ms
                        ),
                    );
                }
            })),
            ..Default::default()
        }))
        .stream_protocol("rk-stream", StreamPolicy::default())
        .recovery(RecoverySpec::documents())
        .engine(EngineSpec::new(|| {
            let mut c = OwnedCommand::new(std::env::current_exe().expect("exe"));
            c.command_mut().arg("--engine");
            c.windows_hide();
            c
        }));
    if let Ok(ms) = std::env::var("RIGHTKIT_EXAMPLE_IDLE_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            shell = shell.webview_idle(Duration::from_millis(ms));
        }
    }
    if let Some(dir) = std::env::var_os("RIGHTKIT_EXAMPLE_CACHE_DIR") {
        shell = shell.cache_dir(PathBuf::from(dir));
    }
    if std::env::var_os("RIGHTKIT_EXAMPLE_CLOSE_TO_TRAY").is_some() {
        shell = shell.close_to_tray(true);
    }
    if let Some(dir) = std::env::var_os("RIGHTKIT_EXAMPLE_STATE_DIR") {
        shell = shell.state_dir(PathBuf::from(dir));
    }
    if let Some(log) = std::env::var_os("RIGHTKIT_EXAMPLE_OPEN_LOG") {
        shell = shell.opener(LogOpener(PathBuf::from(log)));
    }

    let mut builder = tauri::Builder::default().plugin(shell.build().plugin());
    // QA background/hidden launches must opt out before tao starts its event loop.
    // The shell plugin's setup hook runs after Tauri has created configured windows.
    #[cfg(target_os = "macos")]
    if std::env::var_os("RIGHTKIT_QA_BACKGROUND").as_deref() == Some(std::ffi::OsStr::new("1")) {
        builder = builder.activate_ignoring_other_apps(false);
    }
    builder = builder.setup(|app| {
        use tauri::Manager;
        if let Some(path) = std::env::var_os("RIGHTKIT_EXAMPLE_GRANT_FILE") {
            app.state::<rightkit_shell::ShellState>()
                .broker
                .grant_file(path)?;
        }
        Ok(())
    });
    let control = Control::<tauri::Wry>::new()
        .command("block_main_thread", |app, _| {
            app.run_on_main_thread(|| std::thread::sleep(Duration::from_millis(1800))).map_err(|e| e.to_string())?;
            Ok("true".into())
        })
        // Logical geometry of the main window, as the shell sees it.
        .command("geometry", |app, _| {
            use tauri::Manager;
            let w = app.get_webview_window("main").ok_or("no main window")?;
            let scale = w.scale_factor().map_err(|e| e.to_string())?;
            let (p, s) = (w.outer_position().map_err(|e| e.to_string())?, w.inner_size().map_err(|e| e.to_string())?);
            Ok(json!({"x": f64::from(p.x) / scale, "y": f64::from(p.y) / scale, "w": f64::from(s.width) / scale, "h": f64::from(s.height) / scale}).to_string())
        })
        .command("set_geometry", |app, args| {
            use tauri::{LogicalPosition, LogicalSize, Manager};
            // The control server hands the whole request body; `args` is the caller's JSON text.
            let body: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
            let v: serde_json::Value = serde_json::from_str(body["args"].as_str().unwrap_or("{}")).map_err(|e| e.to_string())?;
            let w = app.get_webview_window("main").ok_or("no main window")?;
            let f = |k: &str| v[k].as_f64().ok_or_else(|| format!("missing {k}"));
            w.set_size(LogicalSize::new(f("w")?, f("h")?)).map_err(|e| e.to_string())?;
            w.set_position(LogicalPosition::new(f("x")?, f("y")?)).map_err(|e| e.to_string())?;
            Ok("true".into())
        });
    if let Some(plugin) = control.build_if_enabled() {
        builder = builder.plugin(plugin);
    }
    builder
        .invoke_handler(rightkit_shell::handler![])
        .run(tauri::generate_context!())
        .expect("run shell-example-app");
}
