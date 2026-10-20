// rightkit-tauri-shell 0.5.0 consumed by exact registry pin, outside the workspace.
//
// Proves the navigation scheme rules a native shell applies before it opens a link or accepts an
// internal scheme: external_scheme_allowed admits only http, https and mailto, and refuses other
// schemes and control characters; validate_internal_scheme lowercases a well-formed scheme and
// refuses web and script schemes and malformed names. Public API only.
// Does not prove: webview creation, IPC, windows, menus, the updater, or any Tauri runtime.
// State: pure string checks. No network, no files, no process spawn, no user data.
use rightkit_tauri_shell::nav::{external_scheme_allowed, validate_internal_scheme};

fn main() {
    assert!(external_scheme_allowed("https://example.test/guide"), "https is external");
    assert!(
        external_scheme_allowed("  mailto:support@example.test"),
        "leading space is trimmed and mailto is external"
    );
    assert!(!external_scheme_allowed("javascript:alert(1)"), "javascript is never external");
    assert!(!external_scheme_allowed("file:///fixture/readme.txt"), "file is never external");
    assert!(
        !external_scheme_allowed("https://example.test/\u{7}"),
        "control characters are refused"
    );

    assert_eq!(
        validate_internal_scheme("App-Shell"),
        Ok("app-shell".to_string()),
        "a well-formed scheme is lowercased"
    );
    assert!(validate_internal_scheme("https").is_err(), "https cannot be internal");
    assert!(validate_internal_scheme("javascript").is_err(), "javascript cannot be internal");
    assert!(validate_internal_scheme("1scheme").is_err(), "a scheme starts with a letter");

    println!("OK rightkit-tauri-shell 0.5.0 (registry consumer)");
}
