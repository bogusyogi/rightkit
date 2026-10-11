//! The exact names `tauri-plugin-single-instance` and `tauri-plugin-autostart` derive, pinned with
//! HeardRight's real values as one fixture. Read from the plugin sources (single-instance 2.4.2 and
//! 2.4.3 `src/platform_impl/*.rs`; autostart 2.5.1 `src/lib.rs` with `auto-launch` 0.5.0
//! `src/{windows,macos,linux}.rs`; `tauri-codegen` 2.6.3 `src/context.rs` for `PackageInfo::name`).
//! The derivation is pure, so these run on every host.

use std::path::PathBuf;

use rightkit_shell::{LegacyAutostartNames, LegacySingleInstanceNames};

/// HeardRight `tauri.conf.json`: `identifier` and `productName`.
const HEARDRIGHT_IDENTIFIER: &str = "app.heardright.next";
const HEARDRIGHT_PRODUCT_NAME: &str = "HeardRight";

#[test]
fn heardright_single_instance_names() {
    let n = LegacySingleInstanceNames::for_identifier(HEARDRIGHT_IDENTIFIER);
    assert_eq!(n.windows_mutex, "app.heardright.next-sim");
    assert_eq!(n.windows_window_class, "app.heardright.next-sic");
    assert_eq!(n.windows_window_title, "app.heardright.next-siw");
    assert_eq!(
        n.macos_socket,
        PathBuf::from("/tmp/app_heardright_next_si.sock")
    );
    assert_eq!(n.linux_dbus_name, "app.heardright.next.SingleInstance");
}

#[test]
fn macos_socket_replaces_dots_and_dashes_but_windows_names_keep_the_identifier() {
    let n = LegacySingleInstanceNames::for_identifier("com.some-co.app");
    assert_eq!(
        n.macos_socket,
        PathBuf::from("/tmp/com_some_co_app_si.sock")
    );
    assert_eq!(n.windows_mutex, "com.some-co.app-sim");
}

#[test]
fn heardright_autostart_names() {
    // PackageInfo::name is productName when set, so the entry name is "HeardRight", not the Cargo
    // package "heardright_next".
    let n = LegacyAutostartNames::for_entry_name(HEARDRIGHT_PRODUCT_NAME);
    assert_eq!(n.windows_run_value, "HeardRight");
    assert_eq!(n.macos_launch_agent_label, "HeardRight");
    assert_eq!(
        n.macos_plist_relative,
        PathBuf::from("Library/LaunchAgents/HeardRight.plist")
    );
    assert_eq!(
        n.linux_desktop_relative,
        PathBuf::from(".config/autostart/HeardRight.desktop")
    );
}
