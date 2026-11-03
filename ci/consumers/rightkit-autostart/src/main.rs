// rightkit-autostart 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Renders login-entry text and reads status only. `set_enabled`, `migrate_plugin_entry`,
// `migrate_launch_agent`, `register`, `unregister` and `set_background_io` change login-item state and
// are never called. `status` is a read against a config home that does not exist, so it creates
// nothing and never sees the real user's login items.
use std::path::{Path, PathBuf};

use rightkit_autostart::autostart::{
    entry_path, linux_desktop_entry, linux_desktop_entry_for, macos_plist, macos_plist_with,
    macos_program_arguments, macos_program_arguments_with, reports_unsupported,
    startup_approved_is_disabled, status, uses_service_management, windows_run_command,
    AutostartOptions, AutostartSpec, LegacyAutostartNames, MacosBackend, BACKGROUND_ARG,
};
use rightkit_autostart::login_item::{
    available, executable_in_app_bundle, login_item_status_from_raw, register_result,
    service_management_selected, unregister_result, LoginItemStatus,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn main() {
    // Options and the default launch argument.
    let defaults = AutostartOptions::default();
    check(
        "defaults",
        BACKGROUND_ARG == "--background"
            && defaults.args == vec!["--background".to_string()]
            && !defaults.low_priority_io
            && !defaults.migrate_from_plugin_entry
            && defaults.startup_wm_class.is_none()
            && defaults.config_home.is_none()
            && !defaults.macos_unsupported
            && defaults.macos_backend == MacosBackend::LaunchAgent,
    );
    let opted_out = AutostartOptions {
        macos_unsupported: true,
        ..AutostartOptions::default()
    };
    check(
        "unsupported only applies on macOS",
        reports_unsupported(&opted_out, true) && !reports_unsupported(&opted_out, false),
    );

    // macOS program arguments: a bundle launches through LaunchServices.
    check(
        "bundle program arguments",
        macos_program_arguments(Path::new("/Applications/Demo.app/Contents/MacOS/demo"))
            == [
                "/usr/bin/open",
                "-g",
                "-a",
                "/Applications/Demo.app",
                "--args",
                "--background",
            ],
    );
    check(
        "bare executable program arguments",
        macos_program_arguments_with(Path::new("/usr/local/bin/demo"), &["--flag".to_string()])
            == ["/usr/local/bin/demo", "--flag"],
    );
    check(
        "bundle without arguments has no --args",
        macos_program_arguments_with(Path::new("/Applications/Demo.app/Contents/MacOS/demo"), &[])
            == ["/usr/bin/open", "-g", "-a", "/Applications/Demo.app"],
    );

    // LaunchAgent plist text.
    let plist = macos_plist(
        "com.example.demo",
        &["--background".to_string(), "a&b".to_string()],
    );
    check(
        "plist text",
        plist.contains("<string>com.example.demo</string>")
            && plist.contains("<string>--background</string>")
            && plist.contains("<string>a&amp;b</string>")
            && plist.contains("<key>RunAtLoad</key>")
            && !plist.contains("LowPriorityIO"),
    );
    check(
        "low priority io key",
        macos_plist_with("com.example.demo", &[], true).contains("<key>LowPriorityIO</key>"),
    );

    // XDG desktop entry text.
    check(
        "desktop entry exec line",
        linux_desktop_entry("Demo", Path::new("/usr/bin/demo"))
            .contains("Exec=/usr/bin/demo --background\n"),
    );
    check(
        "desktop entry quotes spaces",
        linux_desktop_entry("Demo", Path::new("/opt/My App/demo"))
            .contains("Exec=\"/opt/My App/demo\" --background\n"),
    );
    check(
        "window class key",
        linux_desktop_entry_for("Demo", Path::new("/usr/bin/demo"), &[], Some(" Demo "))
            .contains("StartupWMClass=Demo\n"),
    );
    check(
        "window class with a control character writes no key",
        !linux_desktop_entry_for("Demo", Path::new("/usr/bin/demo"), &[], Some("bad\nclass"))
            .contains("StartupWMClass"),
    );

    // Windows Run command quoting.
    check(
        "windows run command",
        windows_run_command(
            Path::new("C:\\Program Files\\Demo\\demo.exe"),
            &["--background".to_string(), "--name=a b".to_string()],
        ) == "\"C:\\Program Files\\Demo\\demo.exe\" --background \"--name=a b\"",
    );

    // Names the old tauri-plugin-autostart wrote.
    let legacy = LegacyAutostartNames::for_entry_name("HeardRight");
    check(
        "legacy names",
        legacy.windows_run_value == "HeardRight"
            && legacy.macos_launch_agent_label == "HeardRight"
            && legacy.macos_plist_relative
                == PathBuf::from("Library/LaunchAgents/HeardRight.plist")
            && legacy.linux_desktop_relative
                == PathBuf::from(".config/autostart/HeardRight.desktop"),
    );

    // Task Manager's StartupApproved flag.
    check(
        "enabled flag",
        !startup_approved_is_disabled(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    check(
        "disabled flag carries a timestamp",
        startup_approved_is_disabled(&[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]),
    );
    check(
        "short value counts as enabled",
        !startup_approved_is_disabled(&[3, 0, 0]),
    );

    // SMAppService status mapping and the pure decision rules.
    check(
        "raw status mapping",
        login_item_status_from_raw(0) == LoginItemStatus::NotRegistered
            && login_item_status_from_raw(1) == LoginItemStatus::Enabled
            && login_item_status_from_raw(2) == LoginItemStatus::RequiresApproval
            && login_item_status_from_raw(3) == LoginItemStatus::NotFound
            && login_item_status_from_raw(9) == LoginItemStatus::Unknown(9),
    );
    check(
        "only enabled is enabled",
        LoginItemStatus::Enabled.is_enabled()
            && !LoginItemStatus::Unknown(1).is_enabled()
            && LoginItemStatus::RequiresApproval.needs_approval(),
    );
    check(
        "bundle detection",
        executable_in_app_bundle(Path::new("/Applications/Demo.app/Contents/MacOS/demo"))
            && !executable_in_app_bundle(Path::new("/usr/local/bin/demo")),
    );
    check(
        "service management needs all four conditions",
        service_management_selected(true, true, true, false)
            && !service_management_selected(true, true, true, true)
            && !service_management_selected(true, false, true, false),
    );
    check(
        "register treats an approval-pending failure as registered",
        register_result(Err("boom".into()), LoginItemStatus::RequiresApproval)
            == Ok(LoginItemStatus::RequiresApproval)
            && register_result(Err("boom".into()), LoginItemStatus::NotFound)
                == Err("boom".to_string()),
    );
    check(
        "unregister treats a missing item as the wanted state",
        unregister_result(Err("boom".into()), LoginItemStatus::NotRegistered)
            == Ok(LoginItemStatus::NotRegistered),
    );
    println!("ok: SMAppService available on this host: {}", available());

    // A read-only status query against a config home that does not exist.
    let missing_home = std::env::temp_dir().join(format!(
        "rightkit-autostart-consumer-{}",
        std::process::id()
    ));
    let spec = AutostartSpec {
        app_id: format!("dev.example.rightkit-consumer-{}", std::process::id()),
        app_name: "Consumer".to_string(),
        executable: PathBuf::from("/usr/local/bin/consumer"),
        options: AutostartOptions {
            config_home: Some(missing_home.clone()),
            ..AutostartOptions::default()
        },
        legacy_plugin_name: None,
    };
    check(
        "sandboxed spec does not use service management",
        !uses_service_management(&spec),
    );
    let report = status(&spec);
    check(
        "status reports supported and disabled",
        report.supported && !report.enabled,
    );
    check("nothing was created", !missing_home.exists());
    #[cfg(windows)]
    check(
        "windows entry is a registry value",
        entry_path(&spec).is_none() && report.entry.is_none(),
    );
    #[cfg(not(windows))]
    {
        let entry = entry_path(&spec).expect("entry path under the config home");
        check(
            "entry lives under the config home",
            entry.starts_with(&missing_home),
        );
        let entry_text = entry.to_string_lossy().into_owned();
        check(
            "status names the entry",
            report.entry.as_deref() == Some(entry_text.as_str()),
        );
    }
    let wire = serde_json::to_value(&report).unwrap_or_default();
    check(
        "camelCase wire",
        wire["supported"] == true && wire["enabled"] == false,
    );

    println!("OK rightkit-autostart 0.1.0 (registry consumer, public API)");
}
