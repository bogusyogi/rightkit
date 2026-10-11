//! Windows: the `tauri-plugin-autostart` migration carries Task Manager's flag. The plugin writes the
//! legacy command under `Run` and a same-named 12-byte value under `StartupApproved\Run`
//! (`02 00 00 00 ...` enabled, `03 00 00 00` plus a FILETIME disabled). A user who disabled the item
//! in Task Manager must stay off after migration. Everything runs against a scratch key under
//! `HKCU\Software\RightKitShellTest`, never the real Run keys.

#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use rightkit_shell::autostart::{
    migrate_plugin_entry_in, startup_approved_is_disabled, AutostartMigration, AutostartOptions,
    AutostartSpec, RegistryKeys,
};

mod common;

const ENABLED: &str = "020000000000000000000000";
const DISABLED: &str = "030000000102030405060708";

fn reg(args: &[&str]) -> std::process::Output {
    std::process::Command::new("reg.exe")
        .creation_flags(0x0800_0000)
        .args(args)
        .output()
        .unwrap()
}

struct Scratch {
    root: String,
    keys: RegistryKeys,
}

impl Scratch {
    fn new(case: &str) -> Self {
        let root = format!(
            r"HKCU\Software\RightKitShellTest\{}-{case}",
            std::process::id()
        );
        common::assert_test_name(&root, r"HKCU\Software\RightKitShellTest\");
        let keys = RegistryKeys {
            run: format!(r"{root}\Run"),
            startup_approved: format!(r"{root}\StartupApproved\Run"),
        };
        Self { root, keys }
    }

    fn put_run(&self, name: &str, command: &str) {
        assert!(reg(&[
            "ADD",
            &self.keys.run,
            "/v",
            name,
            "/t",
            "REG_SZ",
            "/d",
            command,
            "/f"
        ])
        .status
        .success());
    }

    fn put_approved(&self, name: &str, hex: &str) {
        assert!(reg(&[
            "ADD",
            &self.keys.startup_approved,
            "/v",
            name,
            "/t",
            "REG_BINARY",
            "/d",
            hex,
            "/f"
        ])
        .status
        .success());
    }

    fn run_exists(&self, name: &str) -> bool {
        reg(&["QUERY", &self.keys.run, "/v", name]).status.success()
    }

    /// The approved value as uppercase hex, or None when absent.
    fn approved(&self, name: &str) -> Option<String> {
        let out = reg(&["QUERY", &self.keys.startup_approved, "/v", name]);
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let line = text.lines().find(|l| l.contains("REG_BINARY"))?;
        Some(line.split("REG_BINARY").nth(1)?.trim().to_uppercase())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = reg(&["DELETE", &self.root, "/f"]);
    }
}

fn spec(app_id: &str) -> AutostartSpec {
    AutostartSpec {
        app_id: app_id.into(),
        app_name: "Demo".into(),
        executable: PathBuf::from(r"C:\Program Files\Demo\demo.exe"),
        options: AutostartOptions::default(),
        legacy_plugin_name: None,
    }
}

const LEGACY: &str = "RkLegacyDemo";
const NEW: &str = "RkNewDemo";

#[test]
fn the_flag_rule_matches_auto_launch() {
    assert!(!startup_approved_is_disabled(&[
        2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
    ]));
    assert!(startup_approved_is_disabled(&[
        3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8
    ]));
    assert!(
        !startup_approved_is_disabled(&[3, 0, 0]),
        "too short to be a Task Manager record"
    );
    assert!(!startup_approved_is_disabled(&[]));
}

#[test]
fn an_enabled_legacy_flag_migrates_without_disabling_the_new_entry() {
    if !common::real_user_stores_allowed(
        "an_enabled_legacy_flag_migrates_without_disabling_the_new_entry",
    ) {
        return;
    }
    let s = Scratch::new("enabled");
    s.put_run(LEGACY, r"C:\old\demo.exe --x");
    s.put_approved(LEGACY, ENABLED);
    let outcome = migrate_plugin_entry_in(&s.keys, &spec(NEW), LEGACY).unwrap();
    assert_eq!(outcome, AutostartMigration::Migrated);
    assert!(s.run_exists(NEW));
    assert!(!s.run_exists(LEGACY), "the legacy command is removed");
    assert_eq!(s.approved(LEGACY), None, "the legacy flag is removed");
    assert_eq!(
        s.approved(NEW),
        None,
        "an enabled item leaves no disabled flag behind"
    );
}

#[test]
fn a_disabled_legacy_flag_is_carried_to_the_new_entry() {
    if !common::real_user_stores_allowed("a_disabled_legacy_flag_is_carried_to_the_new_entry") {
        return;
    }
    let s = Scratch::new("disabled");
    s.put_run(LEGACY, r"C:\old\demo.exe");
    s.put_approved(LEGACY, DISABLED);
    migrate_plugin_entry_in(&s.keys, &spec(NEW), LEGACY).unwrap();
    assert!(s.run_exists(NEW));
    assert!(!s.run_exists(LEGACY));
    assert_eq!(s.approved(LEGACY), None, "the legacy flag is removed");
    assert_eq!(
        s.approved(NEW).as_deref(),
        Some(DISABLED),
        "the user's Task Manager choice is kept"
    );
}

#[test]
fn an_absent_legacy_flag_migrates_cleanly() {
    if !common::real_user_stores_allowed("an_absent_legacy_flag_migrates_cleanly") {
        return;
    }
    let s = Scratch::new("absent");
    s.put_run(LEGACY, r"C:\old\demo.exe");
    migrate_plugin_entry_in(&s.keys, &spec(NEW), LEGACY).unwrap();
    assert!(s.run_exists(NEW));
    assert!(!s.run_exists(LEGACY));
    assert_eq!(s.approved(NEW), None);
}

#[test]
fn no_legacy_run_value_means_nothing_to_migrate_and_nothing_is_touched() {
    if !common::real_user_stores_allowed(
        "no_legacy_run_value_means_nothing_to_migrate_and_nothing_is_touched",
    ) {
        return;
    }
    let s = Scratch::new("none");
    s.put_approved(LEGACY, DISABLED);
    let outcome = migrate_plugin_entry_in(&s.keys, &spec(NEW), LEGACY).unwrap();
    assert_eq!(outcome, AutostartMigration::NoLegacy);
    assert!(!s.run_exists(NEW));
    assert_eq!(s.approved(LEGACY).as_deref(), Some(DISABLED));
}

#[test]
fn when_the_new_entry_has_the_legacy_name_the_flag_stays_and_still_applies() {
    if !common::real_user_stores_allowed(
        "when_the_new_entry_has_the_legacy_name_the_flag_stays_and_still_applies",
    ) {
        return;
    }
    let s = Scratch::new("same-name");
    s.put_run(LEGACY, r"C:\old\demo.exe");
    s.put_approved(LEGACY, DISABLED);
    migrate_plugin_entry_in(&s.keys, &spec(LEGACY), LEGACY).unwrap();
    assert!(s.run_exists(LEGACY), "the shell's entry now owns the name");
    assert_eq!(
        s.approved(LEGACY).as_deref(),
        Some(DISABLED),
        "the user stays off"
    );
}
