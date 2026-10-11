//! DevTools policy decisions that need no GUI: argument and environment neutralisation, registry
//! policy parsing, policy selection, content-webview registration and the capability check.
//!
//! The switch and variable names are rebuilt from pieces: `right-release` scans shipped binaries
//! for them, and test sources are not binaries, but spelling them out here would invite a copy
//! into production code.

use std::time::{Duration, SystemTime};

use rightkit_shell::devtools::{
    argv_violations, build_gate, build_gate_with, capability_granting_ipc, env_vars_to_scrub,
    forbidden_switches_in, glob_match, is_live_debug_port_file, live_debug_port_file,
    parse_reg_query, patterns_may_overlap, policy_violations, ContentWebview, DevToolsError,
    DevToolsGate, DevToolsPolicy, WebviewDecision, DEBUG_PORT_FILE, DEV_INSPECT_COMPILED,
    DEV_INSPECT_MARKER,
};
use rightkit_shell::{Hardening, Shell};

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

#[test]
fn registry_output_is_parsed_and_a_debugging_policy_is_refused() {
    let output = format!(
        "\r\nHKEY_LOCAL_MACHINE\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments\r\n    *    REG_SZ    {}=9222 --disable-gpu\r\n    viewright.exe    REG_SZ    --disable-gpu\r\n\r\n",
        dbg_port()
    );
    let values = parse_reg_query(&output);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0].name, "*");
    assert_eq!(values[1].value, "--disable-gpu");
    let errors = policy_violations(&values, "HKLM policy");
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        &errors[0],
        DevToolsError::BrowserPolicyInjects { source, switch }
            if source.contains("HKLM policy") && switch.starts_with(&dbg_port())
    ));
    assert!(policy_violations(&parse_reg_query(""), "HKCU policy").is_empty());
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
    let shell: rightkit_shell::ShellError = e.into();
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
fn dev_inspect_is_compiled_only_when_the_feature_is_on() {
    assert_eq!(DEV_INSPECT_COMPILED, cfg!(feature = "dev-inspect"));
    assert_eq!(DEV_INSPECT_MARKER, "RIGHTKIT-SHELL-DEV-INSPECT-COMPILED");
}

#[test]
fn gate_of_a_dev_inspect_build_allows_every_webview_and_a_normal_gate_never_does() {
    let dev = build_gate_with(DevToolsPolicy::Denied, "main", &[], false, true).unwrap();
    assert!(dev.is_dev_inspect());
    for label in ["main", "preview", "anything"] {
        assert_eq!(dev.decide(label), WebviewDecision::AllowDev, "{label}");
    }
    assert_eq!(dev.content_labels(), vec!["*".to_owned()]);
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
fn a_dev_inspect_gate_still_validates_content_registrations() {
    // dev-inspect compiles Tauri's devtools in, so ContentOnly needs no second feature there,
    // but a registration that reaches app IPC is as wrong in development as in a release.
    let ok = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "content")],
        false,
        true,
    )
    .unwrap();
    assert_eq!(ok.decide("main"), WebviewDecision::AllowDev);
    let err = build_gate_with(
        DevToolsPolicy::ContentOnly,
        "main",
        &[content("preview", "main").capability(MAIN_CAP)],
        false,
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

#[cfg(feature = "dev-inspect")]
#[test]
fn a_dev_inspect_build_does_not_refuse_debugging_switches_or_scrub_the_environment() {
    // The process guard returns an empty report whatever the environment holds.
    let report = rightkit_shell::devtools::guard_process_start().unwrap();
    assert!(report.scrubbed_env.is_empty());
}

#[test]
fn dev_inspect_is_never_a_default_feature_and_is_independent_of_content_devtools() {
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
    assert!(
        !line_of("default").contains("dev-inspect"),
        "{}",
        line_of("default")
    );
    assert!(!line_of("default").contains("content-devtools"));
    assert!(!line_of("content-devtools").contains("dev-inspect"));
    assert_eq!(
        line_of("dev-inspect").replace(' ', ""),
        "dev-inspect=[\"tauri/devtools\"]"
    );
    // No other feature enables dev-inspect.
    for line in features.lines() {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with("dev-inspect") {
            continue;
        }
        assert!(!t.contains("dev-inspect"), "{t}");
    }
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
    // The runtime messages never spell the marker.
    let announce = devtools
        .split("fn announce_dev_inspect() -> GuardReport {")
        .nth(1)
        .and_then(|t| t.split("\n}\n").next())
        .expect("announce body");
    assert!(!announce.contains("DEV-INSPECT-COMPILED"));
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
