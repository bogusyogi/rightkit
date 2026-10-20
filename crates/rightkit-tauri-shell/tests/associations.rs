//! File-association helpers (Orthic-Labs/rightkit#670): validation, selection, target planning, the
//! roll-up, registry-value decoding and the JSON wire shape. Pure; the OS calls (NSWorkspace, the
//! Windows registry read, Settings) are exercised on a Mac and on Windows.

use std::time::Duration;

use rightkit_tauri_shell::associations::{
    all_groups_set, clamp_timeout, classes_subkey, decode_reg_sz, effective_windows_handler,
    ensure_own_bundle, group_status, handler_matches, mac_targets, normalize_extension,
    select_groups, user_choice_subkey, validate_spec, windows_target, windows_target_extension,
    AssociationGroup, AssociationSpec, AssociationsConfig, SetResult, DEFAULT_SET_TIMEOUT,
    MAX_SET_TIMEOUT, MIN_SET_TIMEOUT,
};
use rightkit_tauri_shell::ShellError;

fn group(id: &str, exts: &[&str], utis: &[&str], prog: Option<&str>) -> AssociationGroup {
    AssociationGroup {
        id: id.into(),
        extensions: exts.iter().map(|s| s.to_string()).collect(),
        utis: utis.iter().map(|s| s.to_string()).collect(),
        windows_prog_id: prog.map(str::to_owned),
    }
}

fn spec() -> AssociationSpec {
    AssociationSpec {
        bundle_id: "app.example".into(),
        groups: vec![
            group(
                "md",
                &[".MD", "markdown", "md"],
                &["net.daringfireball.markdown"],
                Some("Example Markdown"),
            ),
            group("pdf", &["pdf"], &[], Some("Example PDF")),
        ],
        windows_app_name: None,
    }
}

#[test]
fn spec_is_normalised() {
    let s = validate_spec(&spec()).unwrap();
    assert_eq!(s.groups[0].extensions, ["md", "markdown"]);
    assert_eq!(s.bundle_id, "app.example");
    assert_eq!(s.windows_app_name, None);
}

#[test]
fn bad_specs_are_refused() {
    let mut s = spec();
    s.bundle_id = "app example".into();
    assert!(validate_spec(&s).is_err());
    let mut s = spec();
    s.groups.clear();
    assert!(validate_spec(&s).is_err());
    let mut s = spec();
    s.groups.push(group("md", &["x"], &[], None));
    assert!(validate_spec(&s).is_err(), "duplicate id");
    let mut s = spec();
    s.groups.push(group("empty", &[], &[], None));
    assert!(validate_spec(&s).is_err(), "no targets");
    let mut s = spec();
    s.groups[0].extensions.push("a/b".into());
    assert!(validate_spec(&s).is_err());
    let mut s = spec();
    s.groups[0].utis.push("bad uti".into());
    assert!(validate_spec(&s).is_err());
    let mut s = spec();
    s.groups[0].windows_prog_id = Some("a\\b".into());
    assert!(validate_spec(&s).is_err());
    let mut s = spec();
    s.windows_app_name = Some("a&b".into());
    assert!(
        validate_spec(&s).is_err(),
        "the Settings page name is validated"
    );
}

#[test]
fn the_windows_app_name_is_kept_trimmed() {
    let mut s = spec();
    s.windows_app_name = Some("  Example App ".into());
    assert_eq!(
        validate_spec(&s).unwrap().windows_app_name.as_deref(),
        Some("Example App")
    );
}

#[test]
fn extensions_normalise() {
    assert_eq!(normalize_extension(" .Md ").unwrap(), "md");
    let long = "x".repeat(33);
    for bad in ["", ".", "a b", "a.b", "\u{e9}", long.as_str()] {
        assert!(normalize_extension(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn selection_keeps_spec_order_and_rejects_unknown() {
    let s = validate_spec(&spec()).unwrap();
    let all = select_groups(&s, None).unwrap();
    assert_eq!(all.len(), 2);
    let ids = vec!["pdf".to_owned(), "md".to_owned(), "pdf".to_owned()];
    let picked = select_groups(&s, Some(&ids)).unwrap();
    assert_eq!(
        picked.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
        ["md", "pdf"]
    );
    assert!(select_groups(&s, Some(&[])).is_err());
    assert!(select_groups(&s, Some(&["nope".to_owned()])).is_err());
}

#[test]
fn mac_targets_put_utis_first_and_dedupe() {
    let g = group(
        "md",
        &["md", "markdown", "mdx"],
        &["net.daringfireball.markdown"],
        None,
    );
    let resolve = |e: &str| match e {
        "md" | "markdown" => Some("net.daringfireball.markdown".to_owned()),
        _ => None,
    };
    assert_eq!(
        mac_targets(&g, &resolve, &|_| true),
        ["net.daringfireball.markdown"]
    );
    let g = group("t", &["txt", "log"], &["app.example.text"], None);
    let resolve = |e: &str| Some(format!("public.{e}"));
    assert_eq!(
        mac_targets(&g, &resolve, &|_| true),
        ["app.example.text", "public.txt", "public.log"]
    );
}

#[test]
fn unresolvable_utis_are_skipped_and_cannot_block_all_set() {
    let g = group(
        "md",
        &["md"],
        &["net.daringfireball.markdown", "io.typora.markdown"],
        None,
    );
    let resolve = |_: &str| Some("net.daringfireball.markdown".to_owned());
    let no_typora = |u: &str| u != "io.typora.markdown";
    let targets = mac_targets(&g, &resolve, &no_typora);
    assert_eq!(targets, ["net.daringfireball.markdown"]);
    assert_eq!(
        mac_targets(&g, &resolve, &|_| true),
        ["net.daringfireball.markdown", "io.typora.markdown"]
    );
    let none = |_: &str| false;
    assert!(mac_targets(&g, &resolve, &none).is_empty());
    let read = |_: &str| Some("app.example".to_owned());
    assert!(group_status(&g, "app.example", targets, &read).all_set);
}

#[test]
fn windows_targets_round_trip_and_the_user_choice_wins() {
    assert_eq!(windows_target("md"), "extension:.md");
    assert_eq!(windows_target_extension("extension:.md"), Some("md"));
    assert_eq!(windows_target_extension("public.md"), None);
    assert_eq!(
        effective_windows_handler(Some("Other.md".into()), Some("Example Markdown".into()))
            .as_deref(),
        Some("Other.md")
    );
    assert_eq!(
        effective_windows_handler(None, Some("Example Markdown".into())).as_deref(),
        Some("Example Markdown")
    );
    assert_eq!(effective_windows_handler(None, None), None);
}

#[test]
fn handler_comparison_ignores_case() {
    assert!(handler_matches(Some("App.Example"), "app.example"));
    assert!(!handler_matches(Some("com.other"), "app.example"));
    assert!(!handler_matches(None, "app.example"));
}

#[test]
fn group_status_rolls_up() {
    let g = group("md", &["md"], &[], None);
    let read = |t: &str| match t {
        "a" => Some("app.example".to_owned()),
        "b" => Some("com.other".to_owned()),
        _ => None,
    };
    let st = group_status(
        &g,
        "app.example",
        vec!["a".into(), "b".into(), "c".into()],
        &read,
    );
    assert!(!st.all_set);
    assert_eq!(
        st.handlers.iter().map(|h| h.is_default).collect::<Vec<_>>(),
        [true, false, false]
    );
    assert_eq!(st.handlers[2].preferred_handler, None);
    let ok = group_status(&g, "app.example", vec!["a".into()], &read);
    assert!(ok.all_set);
    assert!(
        !group_status(&g, "app.example", vec![], &read).all_set,
        "no targets is not set"
    );
    assert!(all_groups_set(std::slice::from_ref(&ok)));
    assert!(!all_groups_set(&[ok, st]));
    assert!(!all_groups_set(&[]));
}

#[test]
fn only_the_running_app_can_be_set() {
    assert!(ensure_own_bundle(Some("App.Example"), "app.example").is_ok());
    assert!(ensure_own_bundle(Some("com.other"), "app.example").is_err());
    assert!(ensure_own_bundle(None, "app.example").is_err());
}

#[test]
fn timeouts_are_clamped() {
    assert_eq!(clamp_timeout(None), DEFAULT_SET_TIMEOUT);
    assert_eq!(
        clamp_timeout(Some(Duration::from_millis(5))),
        MIN_SET_TIMEOUT
    );
    assert_eq!(
        clamp_timeout(Some(Duration::from_secs(9999))),
        MAX_SET_TIMEOUT
    );
    assert_eq!(
        clamp_timeout(Some(Duration::from_secs(30))),
        Duration::from_secs(30)
    );
}

#[test]
fn registry_paths_are_read_paths_and_values_decode() {
    assert_eq!(
        user_choice_subkey("md"),
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.md\\UserChoice"
    );
    assert_eq!(
        classes_subkey("md"),
        ("Software\\Classes\\.md".to_owned(), ".md".to_owned())
    );
    let utf16 = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let mut with_nul = utf16("Example Markdown");
    with_nul.extend([0, 0, 0]);
    assert_eq!(
        decode_reg_sz(&with_nul).as_deref(),
        Some("Example Markdown")
    );
    assert_eq!(decode_reg_sz(&[0, 0]), None);
    assert_eq!(decode_reg_sz(&[]), None);
    assert_eq!(decode_reg_sz(&[0xD800, 0]), None, "lone surrogate");
}

#[test]
fn wire_shape_is_camel_case() {
    let json = r#"{"bundleId":"app.example","windowsAppName":"Example","groups":[{"id":"md","extensions":["md"],"windowsProgId":"P"}]}"#;
    let s: AssociationSpec = serde_json::from_str(json).unwrap();
    assert_eq!(s.groups[0].windows_prog_id.as_deref(), Some("P"));
    assert_eq!(s.windows_app_name.as_deref(), Some("Example"));
    assert!(s.groups[0].utis.is_empty());
    let out = serde_json::to_value(SetResult {
        groups: vec![],
        all_set: false,
        user_action_required: true,
    })
    .unwrap();
    assert_eq!(out["userActionRequired"], true);
    assert_eq!(out["allSet"], false);
}

#[test]
fn the_native_config_validates_before_it_is_kept() {
    let config = AssociationsConfig::new(spec()).unwrap();
    assert_eq!(config.spec().groups[0].extensions, ["md", "markdown"]);
    let mut bad = spec();
    bad.groups.clear();
    assert!(AssociationsConfig::new(bad).is_err());
}

#[test]
fn hostile_input_is_refused_before_any_os_call() {
    for json in [
        r#"{"bundleId":"","groups":[{"id":"a","extensions":["md"]}]}"#,
        r#"{"bundleId":"app.example","groups":[]}"#,
        r#"{"bundleId":"app.example","groups":[{"id":"a","extensions":["../etc"]}]}"#,
        r#"{"bundleId":"app.example","groups":[{"id":"a","utis":["x y"]}]}"#,
        r#"{"bundleId":"app.example","groups":[{"id":"a"}]}"#,
    ] {
        let s: AssociationSpec = serde_json::from_str(json).expect("spec json");
        assert!(validate_spec(&s).is_err(), "{json}");
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn unsupported_hosts_say_so() {
    assert!(matches!(
        rightkit_tauri_shell::associations::status(&spec(), None),
        Err(ShellError::Unsupported(_))
    ));
}

#[test]
fn errors_are_typed_invalid() {
    let mut s = spec();
    s.bundle_id = String::new();
    assert!(matches!(validate_spec(&s), Err(ShellError::Invalid(_))));
}
