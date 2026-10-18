//! DevTools policy decisions that need no GUI: argument and environment neutralisation, registry
//! policy parsing, policy selection, content-webview registration and the capability check.
//!
//! The switch and variable names are rebuilt from pieces: `right-release` scans shipped binaries
//! for them, and test sources are not binaries, but spelling them out here would invite a copy
//! into production code.

use std::time::{Duration, SystemTime};

use rightkit_tauri_shell::devtools::{
    argv_violations, build_gate, build_gate_with, capability_granting_ipc, decode_reg_text,
    env_vars_to_scrub, forbidden_switches_in, glob_match, is_live_debug_port_file,
    live_debug_port_file, patterns_may_overlap, policy_violations, ContentWebview, DevToolsError,
    DevToolsGate, DevToolsPolicy, PolicyKeyOpen, RegValue, WebviewDecision, DEBUG_PORT_FILE,
    DEV_INSPECT_COMPILED, DEV_INSPECT_MARKER,
};
use rightkit_tauri_shell::{Hardening, Shell};

#[cfg(windows)]
mod common;

/// A native DevTools switch that can reject the call or ignore it.
struct FakeSwitch {
    enabled: bool,
    reject_set: bool,
    ignore_set: bool,
    reject_get: bool,
}

impl FakeSwitch {
    fn on() -> Self {
        Self {
            enabled: true,
            reject_set: false,
            ignore_set: false,
            reject_get: false,
        }
    }
}

impl rightkit_tauri_shell::devtools::DevToolsSwitch for FakeSwitch {
    fn set(&mut self, enabled: bool) -> Result<(), String> {
        if self.reject_set {
            return Err("E_FAIL".to_owned());
        }
        if !self.ignore_set {
            self.enabled = enabled;
        }
        Ok(())
    }
    fn get(&self) -> Result<bool, String> {
        if self.reject_get {
            return Err("E_FAIL".to_owned());
        }
        Ok(self.enabled)
    }
}

fn enforce(decision: WebviewDecision, switch: Option<&mut FakeSwitch>) -> Result<(), String> {
    use rightkit_tauri_shell::devtools::{enforce_switch, DevToolsSwitch};
    enforce_switch(
        "main",
        decision,
        switch.map(|s| s as &mut dyn DevToolsSwitch),
    )
    .map_err(|e| e.code().to_owned())
}

#[test]
fn a_denied_webview_is_refused_unless_devtools_are_off_and_read_back_off() {
    let refused = Err("native_policy_not_applied".to_owned());
    let mut switch = FakeSwitch::on();
    assert_eq!(enforce(WebviewDecision::Deny, Some(&mut switch)), Ok(()));
    assert!(!switch.enabled);

    assert_eq!(enforce(WebviewDecision::Deny, None), refused);
    let mut switch = FakeSwitch {
        reject_set: true,
        ..FakeSwitch::on()
    };
    assert_eq!(enforce(WebviewDecision::Deny, Some(&mut switch)), refused);
    let mut switch = FakeSwitch {
        ignore_set: true,
        ..FakeSwitch::on()
    };
    assert_eq!(enforce(WebviewDecision::Deny, Some(&mut switch)), refused);
    let mut switch = FakeSwitch {
        reject_get: true,
        ..FakeSwitch::on()
    };
    assert_eq!(enforce(WebviewDecision::Deny, Some(&mut switch)), refused);
}

#[test]
fn a_content_webview_is_allowed_and_never_refused_by_the_native_switch() {
    let mut switch = FakeSwitch {
        enabled: false,
        ..FakeSwitch::on()
    };
    assert_eq!(
        enforce(WebviewDecision::AllowContent, Some(&mut switch)),
        Ok(())
    );
    assert!(switch.enabled);
    let mut switch = FakeSwitch {
        reject_set: true,
        ..FakeSwitch::on()
    };
    assert_eq!(
        enforce(WebviewDecision::AllowContent, Some(&mut switch)),
        Ok(())
    );
    assert_eq!(enforce(WebviewDecision::AllowContent, None), Ok(()));
}

fn blocked(key: u32, mods: &str, rules: rightkit_tauri_shell::devtools::KeyRules) -> bool {
    use rightkit_tauri_shell::devtools::{is_blocked_accelerator, KeyModifiers};
    let held = KeyModifiers {
        ctrl: mods.contains('c'),
        shift: mods.contains('s'),
        alt: mods.contains('a'),
    };
    is_blocked_accelerator(key, held, rules)
}

const F5: u32 = 0x74;
const F12: u32 = 0x7B;

#[test]
fn a_denied_webview_blocks_only_the_inspector_and_view_source_keys() {
    use rightkit_tauri_shell::devtools::KeyRules;
    let denied = KeyRules {
        devtools: true,
        ..KeyRules::default()
    };
    let key = |c: char| c as u32;
    assert!(blocked(F12, "", denied) && blocked(F12, "cs", denied));
    for c in ['I', 'J', 'C'] {
        assert!(blocked(key(c), "cs", denied), "Ctrl+Shift+{c}");
    }
    assert!(blocked(key('U'), "c", denied));
    // Ctrl+C is copy, and a plain letter is typing.
    assert!(!blocked(key('C'), "c", denied) && !blocked(key('I'), "", denied));
    // Reload, print, find, zoom and history keys are not this rule's.
    assert!(!blocked(F5, "", denied) && !blocked(key('R'), "c", denied));
    for c in ['P', 'F', '0'] {
        assert!(!blocked(key(c), "c", denied), "Ctrl+{c}");
    }
    assert!(!blocked(0xBB, "c", denied) && !blocked(0x25, "a", denied));
    // A webview that may be inspected has no rule at all.
    assert!(!blocked(F12, "", KeyRules::default()));
}

#[test]
fn with_block_reload_ctrl_p_still_reaches_the_page_and_f5_does_not() {
    use rightkit_tauri_shell::devtools::KeyRules;
    let reload = KeyRules {
        reload: true,
        ..KeyRules::default()
    };
    let key = |c: char| c as u32;
    assert!(blocked(F5, "", reload) && blocked(F5, "c", reload));
    assert!(blocked(key('R'), "c", reload) && blocked(key('R'), "cs", reload));
    assert!(!blocked(key('R'), "", reload));
    assert!(!blocked(key('P'), "c", reload) && !blocked(key('F'), "c", reload));
    assert!(
        !blocked(F12, "", reload) && !blocked(0x25, "a", reload) && !blocked(0xBB, "c", reload)
    );
}

#[test]
fn history_and_zoom_keys_are_blocked_only_by_their_own_rule() {
    use rightkit_tauri_shell::devtools::KeyRules;
    let history = KeyRules {
        history: true,
        ..KeyRules::default()
    };
    let zoom = KeyRules {
        zoom: true,
        ..KeyRules::default()
    };
    // Alt+Left, Alt+Right, browser back and forward; a plain arrow moves the caret.
    assert!(blocked(0x25, "a", history) && blocked(0x27, "a", history));
    assert!(blocked(0xA6, "", history) && blocked(0xA7, "", history));
    assert!(!blocked(0x25, "", history) && !blocked(0xBB, "c", history));
    // Ctrl with plus, minus, zero, also on the number pad; a plain zero is typing.
    for k in [0xBB, 0xBD, 0x30, 0x6B, 0x6D, 0x60] {
        assert!(blocked(k, "c", zoom), "{k:#x}");
        assert!(!blocked(k, "", zoom) && !blocked(k, "c", history), "{k:#x}");
    }
    assert!(!blocked(0x25, "a", zoom) && !blocked('P' as u32, "c", zoom));
}

fn dbg_port() -> String {
    ["--remote-", "debugging-", "port"].concat()
}
fn env_override() -> String {
    ["WEBVIEW2_ADDITIONAL", "_BROWSER_ARGUMENTS"].concat()
}

const MAIN_CAP: &str = r#"{
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["core:default", "core:window:allow-close"]
}"#;
const MAIN_WEBVIEW_CAP: &str = r#"{
  "identifier": "main-only",
  "webviews": ["main"],
  "permissions": ["core:default"]
}"#;
const EMPTY_CAP: &str = r#"{ "identifier": "inert", "windows": ["content"], "permissions": [] }"#;

fn content(label: &str, window: &str) -> ContentWebview {
    ContentWebview::new(label, window).app_acl_manifest(true)
}

#[test]
fn default_policy_is_denied() {
    assert_eq!(DevToolsPolicy::default(), DevToolsPolicy::Denied);
    let gate = build_gate(DevToolsPolicy::default(), "main", &[], true).unwrap();
    assert_eq!(gate.policy(), DevToolsPolicy::Denied);
    assert_eq!(gate.decide("main"), WebviewDecision::Deny);
    assert_eq!(gate.decide("anything"), WebviewDecision::Deny);
    assert_eq!(
        DevToolsGate::denied("main").decide("preview"),
        WebviewDecision::Deny
    );
}

#[test]
fn builder_defaults_to_denied_and_accepts_the_allowance_explicitly() {
    // Compiles against the public builder; the plugin itself needs a Tauri host.
    let _ = Shell::builder("devtools-policy-test").build();
    let _ = Shell::builder("devtools-policy-test")
        .devtools_policy(DevToolsPolicy::ContentOnly)
        .content_webview(content("preview", "content"))
        .build();
    // The Hardening struct no longer has a devtools switch to turn on.
    let _ = Hardening::default();
}

#[test]
fn debugging_switches_are_found_in_every_spelling() {
    let port = dbg_port();
    for text in [
        format!("{port}=9222"),
        format!("--disable-gpu {port}=0"),
        format!("\"{port}=9222\""),
        format!("-{}", &port[2..]),
        "--remote-debugging-pipe".to_owned(),
        "--REMOTE-DEBUGGING-ADDRESS=0.0.0.0".to_owned(),
        "--remote-allow-origins=*".to_owned(),
        "--auto-open-devtools-for-tabs".to_owned(),
        "--msEdgeDevToolsWdpRemoteDebugging".to_owned(),
        "--js-flags=--inspect-brk".to_owned(),
    ] {
        assert!(!forbidden_switches_in(&text).is_empty(), "{text}");
    }
    for text in [
        "",
        "--disable-gpu",
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection",
        "--autoplay-policy=no-user-gesture-required",
        "--js-flags=--max-old-space-size=512",
        "--proxy-server=http://127.0.0.1:8080",
        "remote-debugging-port",
    ] {
        assert!(forbidden_switches_in(text).is_empty(), "{text}");
    }
}

#[test]
fn launch_arguments_with_a_debugging_switch_are_refused() {
    let bad = vec!["--background".to_owned(), format!("{}=9333", dbg_port())];
    assert_eq!(argv_violations(&bad), vec![format!("{}=9333", dbg_port())]);
    assert!(argv_violations(&["--background".to_owned(), "/tmp/doc.html".to_owned()]).is_empty());
}

#[test]
fn webview2_overrides_are_scrubbed_case_insensitively_and_nothing_else() {
    let lower = env_override().to_ascii_lowercase();
    let found = env_vars_to_scrub([
        "PATH".to_owned(),
        env_override(),
        lower.clone(),
        "WEBVIEW2_USER_DATA_FOLDER".to_owned(),
        "WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER".to_owned(),
    ]);
    assert_eq!(
        found,
        vec![
            env_override(),
            lower,
            "WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER".to_owned()
        ]
    );
    assert!(env_vars_to_scrub(["HOME", "TMPDIR"]).is_empty());
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn registry_text_is_decoded_by_type_and_a_debugging_policy_is_refused() {
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const REG_DWORD: u32 = 4;
    const REG_MULTI_SZ: u32 = 7;
    assert_eq!(
        decode_reg_text(REG_SZ, &utf16le("--disable-gpu\0")).as_deref(),
        Some("--disable-gpu")
    );
    assert_eq!(
        decode_reg_text(REG_EXPAND_SZ, &utf16le("%X% --a")).as_deref(),
        Some("%X% --a")
    );
    // Every string of a multi-string counts.
    assert_eq!(
        decode_reg_text(REG_MULTI_SZ, &utf16le("--a\0--b=1\0\0")).as_deref(),
        Some("--a --b=1")
    );
    assert_eq!(decode_reg_text(REG_SZ, &[]).as_deref(), Some(""));
    // A number or binary data carries no argument.
    assert_eq!(decode_reg_text(REG_DWORD, &[1, 0, 0, 0]), None);
    assert_eq!(decode_reg_text(3, &utf16le("--x")), None);

    let values = vec![
        RegValue {
            name: "*".into(),
            value: decode_reg_text(
                REG_SZ,
                &utf16le(&format!("{}=9222 --disable-gpu\0", dbg_port())),
            )
            .unwrap(),
        },
        RegValue {
            name: "viewright.exe".into(),
            value: "--disable-gpu".into(),
        },
    ];
    let errors = policy_violations(&values, "HKLM policy (64-bit view)");
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        &errors[0],
        DevToolsError::BrowserPolicyInjects { source, switch }
            if source.contains("HKLM policy") && switch.starts_with(&dbg_port())
    ));
    assert!(policy_violations(&[], "HKCU policy").is_empty());
}

#[test]
fn only_a_missing_key_means_no_policy() {
    assert_eq!(PolicyKeyOpen::from_status(0), PolicyKeyOpen::Opened);
    // ERROR_FILE_NOT_FOUND.
    assert_eq!(PolicyKeyOpen::from_status(2), PolicyKeyOpen::Absent);
    // Access denied, path not found, a registry I/O failure: the policy is unknown, not empty.
    for status in [5, 3, 1016, 6] {
        assert_eq!(
            PolicyKeyOpen::from_status(status),
            PolicyKeyOpen::Unreadable(status)
        );
    }
}

/// The real registry: a key that does not exist reads as absent, and a key with text values of
/// each type, a long value and a number reads back as the guard would see a policy key.
#[cfg(windows)]
#[test]
fn a_real_registry_key_is_read_value_by_value() {
    use rightkit_tauri_shell::devtools::read_user_key_text_values;
    if !common::real_user_stores_allowed("a_real_registry_key_is_read_value_by_value") {
        return;
    }
    let key = format!(r"Software\RightKitTest\devtools-{}", std::process::id());
    common::assert_test_name(&key, r"Software\RightKitTest\devtools-");
    assert_eq!(read_user_key_text_values(&key), Ok(None));

    let reg = |args: &[&str]| {
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let status = std::process::Command::new(
            std::path::Path::new(&system)
                .join("System32")
                .join("reg.exe"),
        )
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
        assert!(status.success(), "reg {args:?}");
    };
    let full = format!(r"HKCU\{key}");
    /// Removes the test key however the test ends.
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
            let _ = std::process::Command::new(
                std::path::Path::new(&system)
                    .join("System32")
                    .join("reg.exe"),
            )
            .args(["delete", &self.0, "/f"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        }
    }
    let _cleanup = Cleanup(full.clone());
    let long = format!("--long={}", "x".repeat(9000));
    reg(&[
        "add",
        &full,
        "/v",
        "*",
        "/t",
        "REG_SZ",
        "/d",
        &format!("{}=9222", dbg_port()),
        "/f",
    ]);
    reg(&[
        "add",
        &full,
        "/v",
        "many",
        "/t",
        "REG_MULTI_SZ",
        "/d",
        r"--a\0--b",
        "/f",
    ]);
    reg(&[
        "add", &full, "/v", "long", "/t", "REG_SZ", "/d", &long, "/f",
    ]);
    reg(&[
        "add",
        &full,
        "/v",
        "number",
        "/t",
        "REG_DWORD",
        "/d",
        "7",
        "/f",
    ]);

    let read = read_user_key_text_values(&key);
    reg(&["delete", &full, "/f"]);
    let mut values = read.expect("the key is readable").expect("the key exists");
    values.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        values,
        vec![
            RegValue {
                name: "*".into(),
                value: format!("{}=9222", dbg_port())
            },
            RegValue {
                name: "long".into(),
                value: long
            },
            RegValue {
                name: "many".into(),
                value: "--a --b".into()
            },
        ]
    );
    assert_eq!(policy_violations(&values, "test key").len(), 1);
    assert_eq!(read_user_key_text_values(&key), Ok(None));
}

#[test]
fn content_registration_under_denied_is_refused() {
    let err = build_gate(
        DevToolsPolicy::Denied,
        "main",
        &[content("preview", "content")],
        true,
    )
    .unwrap_err();
    assert_eq!(err, DevToolsError::NotContentOnly);
    assert_eq!(err.code(), "not_content_only");
}

#[test]
fn content_only_needs_the_feature_compiled() {
    let err = build_gate(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content")],
        false,
    )
    .unwrap_err();
    assert_eq!(err, DevToolsError::ContentDevtoolsNotCompiled);
}

#[test]
fn the_main_webview_can_never_be_registered() {
    let err = build_gate(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("main", "main")],
        true,
    )
    .unwrap_err();
    assert!(matches!(err, DevToolsError::MainWebview { .. }));
    // The app may rename its main label; the rule follows the configured label.
    let err = build_gate(
        DevToolsPolicy::ContentOnly,
        "shell",
        &[content("shell", "w")],
        true,
    )
    .unwrap_err();
    assert!(matches!(err, DevToolsError::MainWebview { .. }));
}

#[test]
fn labels_are_validated() {
    for bad in ["", "has space", "dot.label", "emoji-\u{1F600}"] {
        let err = build_gate(
            DevToolsPolicy::ContentOnly,
            "main",
            &[content(bad, "content")],
            true,
        )
        .unwrap_err();
        assert!(matches!(err, DevToolsError::InvalidLabel { .. }), "{bad}");
    }
}

#[test]
fn a_content_webview_needs_the_app_acl_manifest_attestation() {
    let reg = ContentWebview::new("preview", "content");
    let err = build_gate(DevToolsPolicy::ContentOnly, "main", &[reg], true).unwrap_err();
    assert!(matches!(err, DevToolsError::AppAclManifestMissing { .. }));
}

#[test]
fn a_child_webview_inherits_its_windows_capability() {
    // Tauri matches a capability's `windows` against the window that hosts the webview, so a
    // content webview placed in the main window reaches everything the main window can.
    let reg = content("preview", "main").capability(MAIN_CAP);
    let err = build_gate(DevToolsPolicy::ContentOnly, "main", &[reg], true).unwrap_err();
    assert!(
        matches!(&err, DevToolsError::ContentHasIpc { capability, .. } if capability == "default"),
        "{err:?}"
    );
}

#[test]
fn a_webview_scoped_capability_for_main_does_not_reach_a_content_webview_in_the_same_window() {
    let reg = content("preview", "main").capability(MAIN_WEBVIEW_CAP);
    let gate = build_gate(DevToolsPolicy::ContentOnly, "main", &[reg], true).unwrap();
    assert_eq!(gate.decide("preview"), WebviewDecision::AllowContent);
    assert_eq!(gate.decide("main"), WebviewDecision::Deny);
}

#[test]
fn wildcard_and_array_capabilities_are_checked() {
    let wildcard = r#"{ "identifier": "all", "windows": ["*"], "permissions": ["core:default"] }"#;
    let err = build_gate(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content").capability(wildcard)],
        true,
    )
    .unwrap_err();
    assert!(matches!(err, DevToolsError::ContentHasIpc { .. }));

    let array = format!("[{EMPTY_CAP}, {MAIN_CAP}, {wildcard}]");
    assert_eq!(
        capability_granting_ipc(&array, "preview", "content").unwrap(),
        Some("all".to_owned())
    );
    // Applies by label but grants nothing: harmless.
    assert_eq!(
        capability_granting_ipc(EMPTY_CAP, "preview", "content").unwrap(),
        None
    );
}

#[test]
fn an_unreadable_capability_is_a_refusal_not_a_pass() {
    let err = build_gate(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content").capability("{ not json")],
        true,
    )
    .unwrap_err();
    assert!(matches!(err, DevToolsError::CapabilitiesUnreadable { .. }));
}

#[test]
fn only_registered_labels_are_allowed_and_only_under_content_only() {
    let reg = [
        content("html-interactive", "content").capability(MAIN_WEBVIEW_CAP),
        content("html-live", "content"),
    ];
    let gate = build_gate(DevToolsPolicy::ContentOnly, "main", &reg, true).unwrap();
    assert_eq!(gate.policy(), DevToolsPolicy::ContentOnly);
    assert_eq!(
        gate.decide("html-interactive"),
        WebviewDecision::AllowContent
    );
    assert_eq!(gate.decide("html-live"), WebviewDecision::AllowContent);
    assert_eq!(gate.decide("print-preview"), WebviewDecision::Deny);
    assert_eq!(gate.decide("main"), WebviewDecision::Deny);
    assert_eq!(gate.content_labels(), vec!["html-interactive", "html-live"]);

    // ContentOnly with nothing registered denies everything.
    let none = build_gate(DevToolsPolicy::ContentOnly, "main", &[], true).unwrap();
    assert_eq!(none.decide("html-live"), WebviewDecision::Deny);
}

#[test]
fn glob_patterns_follow_the_capability_subset() {
    assert!(glob_match("main", "main"));
    assert!(!glob_match("main", "main2"));
    assert!(glob_match("*", "anything"));
    assert!(glob_match("html-*", "html-live"));
    assert!(glob_match("*-live", "html-live"));
    assert!(glob_match("h?ml", "html"));
    assert!(!glob_match("html-*", "xhtml-live"));
    assert!(glob_match("a*b*c", "aXXbYYc"));
}

#[test]
fn a_remote_debugging_port_file_is_live_only_when_fresh() {
    let start = SystemTime::now();
    assert!(is_live_debug_port_file(start, start));
    assert!(is_live_debug_port_file(
        start + Duration::from_secs(5),
        start
    ));
    assert!(!is_live_debug_port_file(
        start - Duration::from_secs(3600),
        start
    ));

    let dir = tempfile::tempdir().unwrap();
    let dirs = vec![dir.path().to_path_buf()];
    assert_eq!(live_debug_port_file(&dirs, start), None);
    let nested = dir.path().join("EBWebView");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join(DEBUG_PORT_FILE), "9222\n/devtools/browser/x").unwrap();
    assert_eq!(
        live_debug_port_file(&dirs, start),
        Some(nested.join(DEBUG_PORT_FILE))
    );
    // The same file is a leftover when the process started long after it was written.
    let later = SystemTime::now() + Duration::from_secs(3600);
    assert_eq!(live_debug_port_file(&dirs, later), None);
}

#[test]
fn errors_are_typed_loud_and_convert_to_the_shell_error() {
    let e = DevToolsError::RemoteDebuggingArgument { arg: dbg_port() };
    assert_eq!(e.code(), "remote_debugging_argument");
    assert!(e.to_string().contains("refusing to start"));
    let shell: rightkit_tauri_shell::ShellError = e.into();
    assert_eq!(shell.code(), "permission_denied");
}

#[test]
fn prefix_patterns_cover_webviews_created_per_document() {
    let reg = [content("html-content-*", "main").capability(MAIN_WEBVIEW_CAP)];
    let gate = build_gate(DevToolsPolicy::ContentOnly, "main", &reg, true).unwrap();
    assert_eq!(gate.decide("html-content-7"), WebviewDecision::AllowContent);
    assert_eq!(gate.decide("html-content-"), WebviewDecision::AllowContent);
    assert_eq!(gate.decide("html-other"), WebviewDecision::Deny);
    assert_eq!(gate.decide("main"), WebviewDecision::Deny);
    // A pattern may not reach the main label, nor be empty.
    for bad in ["m*", "ma*", "*", "main*"] {
        let err = build_gate(
            DevToolsPolicy::ContentOnly,
            "main",
            &[content(bad, "w")],
            true,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                DevToolsError::MainWebview { .. } | DevToolsError::InvalidLabel { .. }
            ),
            "{bad}: {err:?}"
        );
    }
    // Wildcards are only legal as one trailing `*`.
    for bad in ["a*b", "*a", "a**"] {
        let err = build_gate(
            DevToolsPolicy::ContentOnly,
            "main",
            &[content(bad, "w")],
            true,
        )
        .unwrap_err();
        assert!(matches!(err, DevToolsError::InvalidLabel { .. }), "{bad}");
    }
}

#[test]
fn a_capability_that_could_cover_any_label_of_a_pattern_is_refused() {
    for cap_pattern in ["*", "html-*", "html-content-3", "html-content-*", "h*"] {
        let cap = format!(
            r#"{{ "identifier": "c", "webviews": ["{cap_pattern}"], "permissions": ["core:default"] }}"#
        );
        let err = build_gate(
            DevToolsPolicy::ContentOnly,
            "main",
            &[content("html-content-*", "w").capability(&cap)],
            true,
        )
        .unwrap_err();
        assert!(
            matches!(err, DevToolsError::ContentHasIpc { .. }),
            "{cap_pattern}"
        );
    }
    assert!(patterns_may_overlap("main", "main"));
    assert!(!patterns_may_overlap("main", "html-content-*"));
    assert!(!patterns_may_overlap("print-*", "html-content-*"));
}

// ---------------------------------------------------------------------------------------------
// Development builds: the `dev-inspect` feature
// ---------------------------------------------------------------------------------------------

fn crate_file(rel: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn dev_inspect_is_never_compiled_in_any_build() {
    assert!(!DEV_INSPECT_COMPILED);
    assert_eq!(DEV_INSPECT_MARKER, "RIGHTKIT-SHELL-DEV-INSPECT-COMPILED");
}

#[test]
fn no_gate_allows_developer_inspection_even_when_the_legacy_switch_is_passed() {
    let dev = build_gate_with(DevToolsPolicy::Denied, "main", &[], false, true).unwrap();
    assert!(!dev.is_dev_inspect());
    for label in ["main", "preview", "anything"] {
        assert_eq!(dev.decide(label), WebviewDecision::Deny, "{label}");
    }
    assert_eq!(dev.content_labels(), Vec::<String>::new());
    // `build_gate` is the production path and cannot produce a dev gate.
    for compiled in [false, true] {
        let gate = build_gate(DevToolsPolicy::Denied, "main", &[], compiled).unwrap();
        assert!(!gate.is_dev_inspect());
        assert_eq!(gate.decide("main"), WebviewDecision::Deny);
    }
    assert!(!DevToolsGate::denied("main").is_dev_inspect());
    let content_only = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content")],
        true,
        false,
    )
    .unwrap();
    assert!(!content_only.is_dev_inspect());
    assert_eq!(content_only.decide("main"), WebviewDecision::Deny);
    assert_eq!(
        content_only.decide("preview"),
        WebviewDecision::AllowContent
    );
}

#[test]
fn the_legacy_switch_still_validates_content_registrations_and_needs_the_feature() {
    // The ignored switch no longer counts as compiling Tauri's devtools in.
    let err = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content")],
        false,
        true,
    )
    .unwrap_err();
    assert_eq!(err, DevToolsError::ContentDevtoolsNotCompiled);
    let ok = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content")],
        true,
        true,
    )
    .unwrap();
    assert_eq!(ok.decide("main"), WebviewDecision::Deny);
    assert_eq!(ok.decide("preview"), WebviewDecision::AllowContent);
    let err = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "main").capability(MAIN_CAP)],
        true,
        true,
    )
    .unwrap_err();
    assert!(
        matches!(err, DevToolsError::ContentHasIpc { .. }),
        "{err:?}"
    );
    let err = build_gate_with(
        DevToolsPolicy::Denied,
        "main",
        &[content("preview", "content")],
        false,
        true,
    )
    .unwrap_err();
    assert_eq!(err, DevToolsError::NotContentOnly);
    // Without dev-inspect the content feature is still required.
    let err = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content")],
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(err, DevToolsError::ContentDevtoolsNotCompiled);
}

#[test]
fn reload_print_and_view_source_are_separate_switches_with_compatible_defaults() {
    let d = Hardening::default();
    assert!(d.block_reload && d.block_print && d.block_view_source);
    let script = d.script();
    assert!(script.contains("\"blockPrint\":true"), "{script:.80}");
    assert!(script.contains("\"blockViewSource\":true"));
    let mail = Hardening {
        block_print: false,
        block_view_source: false,
        ..Hardening::default()
    };
    let script = mail.script();
    assert!(script.contains("\"blockReload\":true"));
    assert!(script.contains("\"blockPrint\":false"));
    assert!(script.contains("\"blockViewSource\":false"));
    // Reload no longer carries the print/view-source chords; Ctrl+U in an editable surface
    // (underline) is exempt, and the inspector branch is untouched by the split.
    assert!(script.contains("cfg.blockPrint && mod && lk === 'p'"));
    assert!(
        script.contains("cfg.blockViewSource && mod && lk === 'u' && !closest(e.target, EDITABLE)")
    );
    assert!(script.contains("hit = 'devtools'"));
    assert!(!script.contains("lk === 'u') || (mod && lk === 'p')"));
}

#[test]
fn dev_inspect_is_not_a_feature_and_cannot_be_enabled() {
    let manifest = crate_file("Cargo.toml");
    let features = manifest
        .split("\n[features]")
        .nth(1)
        .and_then(|t| t.split("\n[dependencies]").next())
        .expect("[features] table");
    let line_of = |name: &str| {
        features
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{name} =")))
            .unwrap_or_else(|| panic!("feature {name}"))
            .to_owned()
    };
    // The feature is gone: no definition, and no other feature names it, so no
    // build can enable it.
    assert!(!features.contains("dev-inspect"), "{features}");
    assert!(!line_of("default").contains("content-devtools"));
}

#[test]
fn only_devtools_rs_names_the_dev_inspect_marker() {
    // A reference from any other module would put the string into every binary that links the shell.
    for entry in
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")).unwrap()
    {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|n| n != "devtools.rs")
        {
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("DEV_INSPECT_MARKER"), "{}", path.display());
            assert!(!text.contains("DEV-INSPECT-COMPILED"), "{}", path.display());
        }
    }
    let devtools = crate_file("src/devtools.rs");
    // No announce/stand-down path remains, and no gate can produce AllowDev.
    assert!(!devtools.contains("announce_dev_inspect"));
    assert!(!devtools.contains("return WebviewDecision::AllowDev"));
}

#[test]
fn the_release_scanner_and_the_shell_agree_on_the_marker() {
    // Present in a repository checkout; a packaged crate has no `packages/` and skips.
    let scan = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("release")
        .join("qa-control-scan.mjs");
    if let Ok(text) = std::fs::read_to_string(scan) {
        assert!(
            text.contains(&format!("DEV_INSPECT_MARKER = \"{DEV_INSPECT_MARKER}\"")),
            "qa-control-scan.mjs must define the same marker"
        );
    }
}

fn reg(name: &str, value: &str) -> RegValue {
    RegValue {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

#[test]
fn a_policy_value_applies_only_under_the_wildcard_or_this_programs_executable_name() {
    use rightkit_tauri_shell::devtools::policy_value_applies as applies;
    assert!(applies("*", "MailRight.exe"));
    assert!(applies("mailright.EXE", "MailRight.exe"));
    assert!(applies(" MailRight.exe ", "MailRight.exe"));
    assert!(!applies("Teams.exe", "MailRight.exe"));
    assert!(!applies("Orthic.MailRight", "MailRight.exe"));
    assert!(!applies("MailRight", "MailRight.exe"));
    // This program's own name is unknown: nothing can be ruled out.
    assert!(applies("Teams.exe", ""));
}

#[test]
fn the_runtime_folder_environment_variable_is_refused_when_it_is_set() {
    use rightkit_tauri_shell::devtools::{runtime_folder_env_name, runtime_folder_env_violation};
    let name = runtime_folder_env_name();
    assert!(
        name.starts_with("WEBVIEW2_") && name.ends_with("_FOLDER"),
        "{name}"
    );
    assert_eq!(runtime_folder_env_violation(None), None);
    assert_eq!(runtime_folder_env_violation(Some("")), None);
    assert_eq!(runtime_folder_env_violation(Some("   ")), None);
    let refused = runtime_folder_env_violation(Some(r"C:\Planted\Runtime")).expect("refused");
    assert_eq!(refused.code(), "browser_runtime_redirected");
    let text = refused.to_string();
    assert!(
        text.contains(&name) && text.contains(r"C:\Planted\Runtime"),
        "{text}"
    );
}

#[test]
fn a_browser_executable_folder_policy_for_this_program_is_refused() {
    use rightkit_tauri_shell::devtools::runtime_folder_violations as violations;
    let values = [
        reg("Teams.exe", r"C:\Other\Runtime"),
        reg("*", r" C:\Planted\Runtime "),
        reg("MailRight.exe", ""),
    ];
    let found = violations(&values, "HKLM policy (64-bit view)", "MailRight.exe");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code(), "browser_runtime_redirected");
    let text = found[0].to_string();
    assert!(
        text.contains(r"C:\Planted\Runtime") && text.contains("value `*`"),
        "{text}"
    );
    // Another program's fixed runtime is not this program's business.
    assert!(violations(&values[..1], "HKCU policy", "MailRight.exe").is_empty());
}

#[test]
fn a_user_data_folder_policy_adds_its_expanded_folder_to_the_tripwire() {
    use rightkit_tauri_shell::devtools::{expand_env_refs, policy_user_data_dirs};
    let env = |name: &str| match name {
        "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".to_owned()),
        _ => None,
    };
    assert_eq!(
        expand_env_refs(r"%LOCALAPPDATA%\wv2\%UNKNOWN%\50%", env),
        r"C:\Users\u\AppData\Local\wv2\%UNKNOWN%\50%"
    );
    assert_eq!(expand_env_refs("%%", env), "%%");
    let values = [
        reg("*", r"%LOCALAPPDATA%\Policy\Data"),
        reg("Teams.exe", r"C:\Teams\Data"),
    ];
    let dirs = policy_user_data_dirs(&values, "MailRight.exe", env);
    assert_eq!(
        dirs,
        vec![std::path::PathBuf::from(
            r"C:\Users\u\AppData\Local\Policy\Data"
        )]
    );

    // A fresh port file in such a folder trips the wire.
    let dir = tempfile::tempdir().unwrap();
    let start = SystemTime::now();
    std::fs::write(dir.path().join(DEBUG_PORT_FILE), "9222\n").unwrap();
    assert!(live_debug_port_file(&[dir.path().to_path_buf()], start).is_some());
}
