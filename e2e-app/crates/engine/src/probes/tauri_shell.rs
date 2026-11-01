//! rightkit-tauri-shell: the navigation policy every webview of the shell enforces: the app's own origin loads in place, a web link
//! leaves for the system browser, and a dangerous scheme is blocked. The window itself (hardening script, window state, single
//! instance) is the app-level e2e's: it needs the shell's running webview.
use rightkit_tauri_shell::{NavDecision, NavPolicy};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let policy = NavPolicy {
        internal_hosts: vec!["tauri.localhost".into()],
        ..NavPolicy::default()
    };
    let url = |s: &str| url::Url::parse(s).map_err(|e| format!("{s}: {e}"));
    let own = policy.decide(&url("http://tauri.localhost/index.html")?);
    let web = policy.decide(&url("https://example.com/page")?);
    let js = policy.decide(&url("javascript:alert(1)")?);
    let file = policy.decide(&url("file:///etc/passwd")?);
    let ok = matches!(own, NavDecision::Allow)
        && matches!(web, NavDecision::External { .. })
        && matches!(js, NavDecision::Block)
        && !matches!(file, NavDecision::Allow);
    if !ok {
        return Err(format!(
            "own origin {own:?}, web {web:?}, javascript: {js:?}, file: {file:?}"
        ));
    }
    Ok(
        json!({ "own": format!("{own:?}"), "web": format!("{web:?}"), "javascript": format!("{js:?}"), "file": format!("{file:?}") }),
    )
}
