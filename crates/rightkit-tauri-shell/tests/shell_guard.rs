#![cfg(feature = "host-services")]
//! The shell-command guard (`HostServicesBuilder::guard_shell_commands`): who may run an
//! `rk_shell_*` command once the guard is on.
//!
//! Every `rk_shell_*` command opens with `guard_command(&webview, name)`, which gathers the
//! invoking webview's facts and takes exactly this decision (`guard_coverage.rs` proves the call is
//! the first statement of every command, so a refusal precedes any read or effect). The facts are
//! built by hand, as `host_services.rs` does, so no webview is needed.

use std::cell::Cell;

use rightkit_tauri_shell::caller_gate::{CallerError, CallerFacts, Denial};
use rightkit_tauri_shell::host::{
    shell_command_for, HostServices, ServiceTrust, SHELL_GUARD_EXEMPT,
};
use rightkit_tauri_shell::NavPolicy;

const COMMANDS: [&str; 6] = [
    "rk_shell_clipboard_read",
    "rk_shell_clipboard_write",
    "rk_shell_open_window",
    "rk_shell_path_require",
    "rk_shell_engine_call",
    "rk_shell_quit",
];

fn url(s: &str) -> tauri::Url {
    tauri::Url::parse(s).unwrap()
}

fn guarded(trust: ServiceTrust) -> HostServices {
    HostServices::builder()
        .guard_shell_commands(trust)
        .build()
        .unwrap()
}

/// The effect a guarded command would have run: it ran only when the decision allowed it.
fn run_if_allowed(
    host: &HostServices,
    command: &str,
    facts: &CallerFacts<'_>,
    effects: &Cell<u32>,
) -> Result<(), CallerError> {
    shell_command_for(host, command, facts)?;
    effects.set(effects.get() + 1);
    Ok(())
}

fn facts<'a>(
    label: &'a str,
    content: &'a [String],
    url: Option<&'a tauri::Url>,
    nav: &'a NavPolicy,
) -> CallerFacts<'a> {
    CallerFacts {
        label,
        is_app_window: true,
        content_labels: content,
        url,
        nav,
    }
}

#[test]
fn the_trusted_app_window_runs_an_enabled_command() {
    let host = guarded(ServiceTrust::for_labels(["main"]).with_actions(COMMANDS));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let effects = Cell::new(0);
    for command in COMMANDS {
        run_if_allowed(
            &host,
            command,
            &facts("main", &[], Some(&page), &nav),
            &effects,
        )
        .unwrap();
    }
    assert_eq!(effects.get(), COMMANDS.len() as u32);
}

#[test]
fn a_content_window_is_refused_and_nothing_runs() {
    // Even when its label is the one the guard trusts: the registry of content webviews wins.
    let host = guarded(ServiceTrust::for_labels(["main", "html-content"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let content = vec!["html-content".to_owned()];
    let effects = Cell::new(0);
    for command in COMMANDS {
        for label in ["html-content", "main"] {
            let content_labels: &[String] = if label == "html-content" {
                &content
            } else {
                &[]
            };
            let result = run_if_allowed(
                &host,
                command,
                &facts(label, content_labels, Some(&page), &nav),
                &effects,
            );
            if label == "html-content" {
                assert_eq!(
                    result,
                    Err(CallerError::Forbidden(Denial::ContentWebview)),
                    "{command}"
                );
            } else {
                assert_eq!(result, Ok(()), "{command}");
            }
        }
    }
    // Only the six trusted-window calls ran; the six content-window calls did nothing.
    assert_eq!(effects.get(), COMMANDS.len() as u32);
}

#[test]
fn a_foreign_origin_is_refused_and_nothing_runs() {
    let host = guarded(ServiceTrust::for_labels(["main"]));
    let nav = NavPolicy::default();
    let effects = Cell::new(0);
    for page in [
        "https://evil.example/",
        "data:text/html,<p>x</p>",
        "blob:https://x/1",
        "about:blank",
    ] {
        let page = url(page);
        for command in COMMANDS {
            assert_eq!(
                run_if_allowed(
                    &host,
                    command,
                    &facts("main", &[], Some(&page), &nav),
                    &effects
                ),
                Err(CallerError::Forbidden(Denial::ForeignOrigin)),
                "{page} {command}"
            );
        }
    }
    // An unreadable URL fails closed too.
    assert_eq!(
        run_if_allowed(
            &host,
            "rk_shell_clipboard_read",
            &facts("main", &[], None, &nav),
            &effects
        ),
        Err(CallerError::Forbidden(Denial::UnknownOrigin))
    );
    assert_eq!(effects.get(), 0);
}

#[test]
fn an_app_origin_the_trust_does_not_list_is_refused_and_nothing_runs() {
    let host = guarded(ServiceTrust::for_labels(["main"]).with_origins(["tauri://localhost"]));
    let nav = NavPolicy::default();
    let other_app_origin = url("http://tauri.localhost/");
    let listed = url("tauri://localhost/");
    let effects = Cell::new(0);
    assert_eq!(
        run_if_allowed(
            &host,
            "rk_shell_quit",
            &facts("main", &[], Some(&other_app_origin), &nav),
            &effects
        ),
        Err(CallerError::Forbidden(Denial::UntrustedOrigin))
    );
    assert_eq!(effects.get(), 0);
    run_if_allowed(
        &host,
        "rk_shell_quit",
        &facts("main", &[], Some(&listed), &nav),
        &effects,
    )
    .unwrap();
    assert_eq!(effects.get(), 1);
}

#[test]
fn another_app_window_the_guard_does_not_name_is_refused_and_nothing_runs() {
    let host = guarded(ServiceTrust::for_labels(["main"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let effects = Cell::new(0);
    assert_eq!(
        run_if_allowed(
            &host,
            "rk_shell_clipboard_read",
            &facts("settings", &[], Some(&page), &nav),
            &effects
        ),
        Err(CallerError::Forbidden(Denial::UntrustedLabel))
    );
    // A label that is not a top-level app window at all is refused before the trust is read.
    let mut child = facts("main", &[], Some(&page), &nav);
    child.is_app_window = false;
    assert_eq!(
        run_if_allowed(&host, "rk_shell_clipboard_read", &child, &effects),
        Err(CallerError::Forbidden(Denial::NotAnAppWebview))
    );
    assert_eq!(effects.get(), 0);
}

#[test]
fn a_disabled_command_is_refused_for_the_trusted_window_and_nothing_runs() {
    let host = guarded(ServiceTrust::for_labels(["main"]).with_actions(["rk_shell_quit"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let effects = Cell::new(0);
    for command in COMMANDS.iter().filter(|c| **c != "rk_shell_quit") {
        assert_eq!(
            run_if_allowed(
                &host,
                command,
                &facts("main", &[], Some(&page), &nav),
                &effects
            ),
            Err(CallerError::Forbidden(Denial::ActionNotEnabled)),
            "{command}"
        );
    }
    assert_eq!(effects.get(), 0);
    run_if_allowed(
        &host,
        "rk_shell_quit",
        &facts("main", &[], Some(&page), &nav),
        &effects,
    )
    .unwrap();
    assert_eq!(effects.get(), 1);
}

#[test]
fn open_external_is_exempt_for_every_caller_and_nothing_else_is() {
    // The hardening script calls it from every webview, content webviews included.
    assert_eq!(SHELL_GUARD_EXEMPT, ["rk_shell_open_external"]);
    let host = guarded(ServiceTrust::for_labels(["main"]).with_actions(["rk_shell_quit"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let evil = url("https://evil.example/");
    let content = vec!["html-content".to_owned()];
    let effects = Cell::new(0);
    for f in [
        facts("html-content", &content, Some(&page), &nav),
        facts("popup", &[], Some(&evil), &nav),
        facts("main", &[], None, &nav),
    ] {
        run_if_allowed(&host, "rk_shell_open_external", &f, &effects).unwrap();
        for command in COMMANDS {
            assert!(
                run_if_allowed(&host, command, &f, &effects).is_err(),
                "{command}"
            );
        }
    }
    // Only the three exempt calls ran.
    assert_eq!(effects.get(), 3);
}

#[test]
fn the_exempt_commands_own_url_policy_binds_every_caller() {
    use rightkit_tauri_shell::commands::{external_url_shape_allowed, MAX_EXTERNAL_URL_BYTES};
    assert!(external_url_shape_allowed("https://example.com/a?b=c"));
    assert!(external_url_shape_allowed("mailto:a@example.com"));
    assert!(!external_url_shape_allowed("https://example.com/\nHost: x"));
    assert!(!external_url_shape_allowed("https://example.com/\u{0}"));
    let long = format!("https://example.com/{}", "a".repeat(MAX_EXTERNAL_URL_BYTES));
    assert!(!external_url_shape_allowed(&long));
    // The scheme allowlist (`opener::open_checked`) is the other half.
    for refused in [
        "file:///C:/x",
        "javascript:alert(1)",
        "data:text/html,x",
        "ms-settings:",
    ] {
        assert!(
            !rightkit_tauri_shell::nav::external_scheme_allowed(refused),
            "{refused}"
        );
    }
}

#[test]
fn the_guard_is_off_until_the_app_enables_it() {
    // Existing apps are unchanged: no guard, no per-window decision. (The commands still apply
    // the module opt-outs and their own checks.)
    let host = HostServices::builder().build().unwrap();
    let nav = NavPolicy::default();
    let page = url("https://evil.example/");
    let effects = Cell::new(0);
    run_if_allowed(
        &host,
        "rk_shell_clipboard_read",
        &facts("anything", &[], Some(&page), &nav),
        &effects,
    )
    .unwrap();
    assert_eq!(effects.get(), 1);
}

#[test]
fn a_guard_that_would_trust_every_window_does_not_build() {
    assert!(HostServices::builder()
        .guard_shell_commands(ServiceTrust::for_labels(["*"]))
        .build()
        .is_err());
}
