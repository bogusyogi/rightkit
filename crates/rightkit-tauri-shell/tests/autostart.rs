//! Login-item options and the migration from `tauri-plugin-autostart`. The entries are written under
//! a temporary home (`RIGHTKIT_SHELL_HOME`, and `XDG_CONFIG_HOME` on Linux), never the real user's.
//! Windows registry behaviour is exercised by the pure command builders instead.

#![cfg(not(windows))]

use std::path::{Path, PathBuf};

use rightkit_tauri_shell::autostart::{
    self, linux_desktop_entry_for, macos_plist_with, macos_program_arguments_with,
    reports_unsupported, windows_run_command, AutostartMigration, AutostartOptions, AutostartSpec,
    BACKGROUND_ARG,
};

fn spec(app_id: &str, options: AutostartOptions, legacy: Option<&str>) -> AutostartSpec {
    AutostartSpec {
        app_id: app_id.into(),
        app_name: "Demo".into(),
        executable: PathBuf::from("/Applications/Demo.app/Contents/MacOS/Demo"),
        options,
        legacy_plugin_name: legacy.map(str::to_owned),
    }
}

/// The path of the shell's entry for `app_id`, derived the way the platform does.
fn our_entry(home: &Path, app_id: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents")
            .join(format!("{app_id}.autostart.plist"))
    } else {
        home.join(".config/autostart")
            .join(format!("{app_id}.desktop"))
    }
}

/// The path of an entry written by `tauri-plugin-autostart` under the package name.
fn legacy_entry(home: &Path, name: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents")
            .join(format!("{name}.plist"))
    } else {
        home.join(".config/autostart")
            .join(format!("{name}.desktop"))
    }
}

#[test]
fn default_options_are_the_background_argument_with_no_io_priority_change() {
    let defaults = AutostartOptions::default();
    assert_eq!(defaults.args, vec![BACKGROUND_ARG.to_owned()]);
    assert!(!defaults.low_priority_io);
    assert!(!defaults.migrate_from_plugin_entry);
}

#[test]
fn pure_builders_place_arguments_and_low_priority_io() {
    let args = vec!["--background".to_owned(), "--quiet".to_owned()];
    let bundled = macos_program_arguments_with(
        Path::new("/Applications/Demo.app/Contents/MacOS/Demo"),
        &args,
    );
    assert_eq!(
        &bundled[..4],
        ["/usr/bin/open", "-g", "-a", "/Applications/Demo.app"]
    );
    assert_eq!(&bundled[4..], ["--args", "--background", "--quiet"]);

    let plain = macos_program_arguments_with(Path::new("/usr/local/bin/demo"), &args);
    assert_eq!(
        plain,
        vec!["/usr/local/bin/demo", "--background", "--quiet"]
    );

    assert!(macos_plist_with("x", &args, true).contains("<key>LowPriorityIO</key>"));
    assert!(!macos_plist_with("x", &args, false).contains("LowPriorityIO"));

    assert_eq!(
        windows_run_command(
            Path::new(r"C:\Program Files\Demo\demo.exe"),
            &["--background".to_owned(), "a b".to_owned()],
        ),
        r#""C:\Program Files\Demo\demo.exe" --background "a b""#
    );
}

#[test]
fn the_entry_carries_the_options_and_migration_leaves_one_login_item() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("RIGHTKIT_SHELL_HOME", home.path());
    std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));

    // An earlier build's login item, written by tauri-plugin-autostart under the package name.
    let legacy = legacy_entry(home.path(), "demo");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy plugin entry").unwrap();

    let options = AutostartOptions {
        args: vec!["--background".into(), "--quiet".into()],
        low_priority_io: true,
        migrate_from_plugin_entry: true,
        ..AutostartOptions::default()
    };
    let demo = spec("com.example.demo", options, Some("demo"));

    // Migration: the shell's entry is written with the options, and the legacy entry is removed.
    assert_eq!(
        autostart::migrate_plugin_entry(&demo, "demo").unwrap(),
        AutostartMigration::Migrated
    );
    assert!(!legacy.exists(), "the legacy login item must be removed");
    assert_eq!(
        autostart::migrate_plugin_entry(&demo, "demo").unwrap(),
        AutostartMigration::NoLegacy
    );

    let ours = our_entry(home.path(), "com.example.demo");
    let body = std::fs::read_to_string(&ours).unwrap();
    assert!(
        body.contains("--quiet"),
        "the configured arguments are written"
    );
    if cfg!(target_os = "macos") {
        assert!(body.contains("<key>LowPriorityIO</key>"));
        assert!(body.contains("/usr/bin/open"));
    }
    assert!(autostart::status(&demo).enabled);

    // Disabling removes the shell's entry, and with the migration flag it also leaves no legacy entry.
    let status = autostart::set_enabled(&demo, false).unwrap();
    assert!(!status.enabled);
    assert!(!ours.exists());

    // Default options write the plain background entry, with no LowPriorityIO.
    let plain = spec("com.example.plain", AutostartOptions::default(), None);
    autostart::set_enabled(&plain, true).unwrap();
    let body = std::fs::read_to_string(our_entry(home.path(), "com.example.plain")).unwrap();
    assert!(body.contains("--background"));
    assert!(!body.contains("LowPriorityIO"));
}

#[test]
fn the_linux_entry_gains_startup_wm_class_only_when_asked() {
    let exe = Path::new("/opt/code/coderight");
    let args = [BACKGROUND_ARG.to_owned()];
    let plain = linux_desktop_entry_for("CodeRight", exe, &args, None);
    assert!(!plain.contains("StartupWMClass"));
    // Byte-identical to the entry written before the option existed.
    assert_eq!(
        plain,
        autostart::linux_desktop_entry_with("CodeRight", exe, &args)
    );

    let with = linux_desktop_entry_for("CodeRight", exe, &args, Some("coderight"));
    assert!(with.contains("\nStartupWMClass=coderight\n"));
    assert!(with.starts_with("[Desktop Entry]\n"));
    assert!(with.contains("Exec=/opt/code/coderight --background\n"));
}

#[test]
fn a_wm_class_cannot_inject_extra_desktop_keys() {
    let exe = Path::new("/opt/code/coderight");
    for bad in ["x\nExec=/bin/evil", "x\r", "", "  "] {
        let body = linux_desktop_entry_for("CodeRight", exe, &[], Some(bad));
        assert!(!body.contains("StartupWMClass"), "{bad:?}");
        assert!(!body.contains("evil"));
    }
}

#[test]
fn macos_unsupported_is_a_host_choice_and_the_default_is_unchanged() {
    assert!(!reports_unsupported(&AutostartOptions::default(), true));
    let off = AutostartOptions {
        macos_unsupported: true,
        ..AutostartOptions::default()
    };
    assert!(reports_unsupported(&off, true));
    assert!(!reports_unsupported(&off, false), "only macOS is affected");
}

#[cfg(target_os = "macos")]
#[test]
fn an_unsupported_macos_host_creates_no_launch_agent() {
    let home = tempfile::tempdir().unwrap();
    let options = AutostartOptions {
        macos_unsupported: true,
        config_home: Some(home.path().to_path_buf()),
        ..AutostartOptions::default()
    };
    let s = spec("com.example.nolaunch", options, None);
    let status = autostart::status(&s);
    assert!(!status.supported && !status.enabled && status.entry.is_none());
    assert!(autostart::set_enabled(&s, true).is_err());
    assert!(
        !home.path().join("Library").exists(),
        "nothing may be written"
    );
    // Turning it off is harmless.
    assert!(autostart::set_enabled(&s, false).is_ok());
}

#[test]
fn an_injected_config_home_places_the_entry_without_touching_the_environment() {
    let home = tempfile::tempdir().unwrap();
    let options = AutostartOptions {
        config_home: Some(home.path().to_path_buf()),
        startup_wm_class: Some("demo-class".into()),
        ..AutostartOptions::default()
    };
    let s = spec("com.example.injected", options, None);
    let expected = if cfg!(target_os = "macos") {
        home.path()
            .join("Library/LaunchAgents/com.example.injected.autostart.plist")
    } else {
        home.path().join("autostart/com.example.injected.desktop")
    };
    assert_eq!(autostart::entry_path(&s), Some(expected.clone()));
    autostart::set_enabled(&s, true).unwrap();
    assert!(expected.is_file());
    if !cfg!(target_os = "macos") {
        let body = std::fs::read_to_string(&expected).unwrap();
        assert!(body.contains("StartupWMClass=demo-class"));
    }
    assert!(autostart::status(&s).enabled);
    autostart::set_enabled(&s, false).unwrap();
    assert!(!expected.exists());
}

#[test]
fn service_management_is_opt_in_and_a_redirected_home_never_selects_it() {
    use rightkit_tauri_shell::autostart::MacosBackend;
    assert_eq!(
        AutostartOptions::default().macos_backend,
        MacosBackend::LaunchAgent
    );
    // A sandboxed run (config_home) must not register the real user's login item: it keeps the plist.
    let home = tempfile::tempdir().unwrap();
    let options = AutostartOptions {
        macos_backend: MacosBackend::ServiceManagement,
        config_home: Some(home.path().to_path_buf()),
        ..AutostartOptions::default()
    };
    let s = spec("com.example.sm", options, None);
    assert!(!autostart::uses_service_management(&s));
    let status = autostart::set_enabled(&s, true).unwrap();
    assert!(status.supported);
    assert!(status.login_item.is_none());
}
