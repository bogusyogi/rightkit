// rightkit-tauri-shell 0.5.0 consumed by exact registry pin, outside the workspace.
//
// Proves, through public API only and with default features:
// * navigation scheme rules: external_scheme_allowed admits only http, https and mailto, and
//   refuses other schemes and control characters; validate_internal_scheme lowercases a
//   well-formed scheme and refuses web and script schemes and malformed names;
// * caller/window ACL label rules (PK04/PK08): is_content_label matches a registered content
//   webview by exact label or `prefix-*` pattern, and never an app window label;
// * WebView2 profile policy (PK08-1): webview2_env::plan refuses remote-debugging and
//   no-sandbox browser arguments and joins benign arguments in order;
// * preview store (PK08-5): PreviewStore registers a document under a declared kind, serves it
//   by token, refuses an undeclared kind, and a revoked token no longer resolves.
// Does not prove: webview creation, IPC, windows, menus, the updater, or any Tauri runtime.
// The full caller decision needs a live webview URL, so only its pure label rule runs here.
// State: in-memory only; OS entropy is read for the preview token. No network, no files,
// no process spawn, no user data.
use std::time::Instant;

use rightkit_tauri_shell::caller_gate::is_content_label;
use rightkit_tauri_shell::nav::{external_scheme_allowed, validate_internal_scheme};
use rightkit_tauri_shell::preview::{PreviewStore, TOKEN_HEX_LEN};
use rightkit_tauri_shell::webview2_env::{plan, ProfileEnv, ENV_ADDITIONAL_BROWSER_ARGUMENTS};

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

    let content = vec!["html-content".to_string(), "doc-*".to_string()];
    assert!(is_content_label(&content, "html-content"), "exact content label matches");
    assert!(is_content_label(&content, "doc-1"), "prefix content pattern matches");
    assert!(!is_content_label(&content, "main"), "the app window is not content");
    assert!(!is_content_label(&content, "settings"), "a trusted window is not content");
    assert!(!is_content_label(&[], "doc-1"), "no registered content labels, no match");

    for refused in ["--remote-debugging-port=9222", "--REMOTE-DEBUGGING-PIPE", "--no-sandbox"] {
        assert!(
            plan(&ProfileEnv::new().arg(refused)).is_err(),
            "shell policy refuses {refused}"
        );
    }
    let planned = plan(&ProfileEnv::new().arg("--lang=en-GB")).expect("a benign argument plans");
    assert_eq!(planned, vec![(ENV_ADDITIONAL_BROWSER_ARGUMENTS, "--lang=en-GB".to_string())]);

    let store = PreviewStore::new([("html", "default-src 'none'; img-src data:")])
        .expect("a declared preview kind builds a store");
    assert_eq!(store.kinds(), vec!["html"]);
    assert!(
        store.register("mdx", "<p>x</p>".to_string()).is_err(),
        "an undeclared preview kind is refused"
    );
    let token = store
        .register("html", "<p>preview</p>".to_string())
        .expect("a declared kind registers");
    assert_eq!(token.len(), TOKEN_HEX_LEN, "the token is full-length hex");
    let (kind, bytes) = store
        .lookup_at(&token, Instant::now())
        .expect("a live token resolves");
    assert_eq!(kind, "html");
    assert_eq!(&*bytes, b"<p>preview</p>");
    store.revoke(&token);
    assert!(
        store.lookup_at(&token, Instant::now()).is_none(),
        "a revoked token no longer resolves"
    );

    println!("OK rightkit-tauri-shell 0.5.0 (registry consumer)");
}
