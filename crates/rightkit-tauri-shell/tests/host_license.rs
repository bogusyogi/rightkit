#![cfg(feature = "license")]
//! The native licence host (`rightkit_license_*`, Orthic-Labs/rightkit PK05-3 / PK04-2): the caller
//! policy that fronts the client, the input limits, and the closed wire shapes.
//!
//! Everything runs on the plain-Rust cores (`license_for`, `LicenseHostService`) with
//! `CallerFacts` built by hand, exactly what the Tauri commands build from the invoking webview, so
//! no webview is needed. The client is a counting fake behind `LicenseOps`: a refused caller must
//! leave every counter at zero (no lock, no network, no record read). `guard_coverage.rs` proves
//! each command's first statement is the caller check.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_license::{
    LicenseError, LicenseInvalidation, LicenseView, MessageCode, RefreshView, RefreshViewOutcome,
    RejectReason, SecretString, TrialView,
};
use rightkit_tauri_shell::caller_gate::CallerFacts;
use rightkit_tauri_shell::host::license::ChangeCoalescer;
use rightkit_tauri_shell::host::license::{
    hears_license_changes, license_for, MAX_EMAIL_CHARS, MAX_KEY_CHARS,
};
use rightkit_tauri_shell::host::{
    HostServices, LicenseHostService, LicenseOps, ServiceTrust, EVENT_LICENSE_CHANGED,
};
use rightkit_tauri_shell::NavPolicy;
use serde_json::json;

#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    keys: Mutex<Vec<String>>,
    emails: Mutex<Vec<String>>,
}

impl Fake {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn hit(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

impl LicenseOps for Fake {
    fn status(&self) -> LicenseView {
        self.hit();
        LicenseView::disabled()
    }
    fn activate(&self, key: SecretString) -> Result<LicenseView, LicenseError> {
        self.hit();
        self.keys.lock().unwrap().push(key.expose().to_owned());
        Ok(LicenseView::disabled())
    }
    fn refresh(&self) -> Result<RefreshView, LicenseError> {
        self.hit();
        Ok(RefreshView {
            view: LicenseView::disabled(),
            outcome: RefreshViewOutcome::NoCredential,
        })
    }
    fn deactivate(&self) -> Result<LicenseView, LicenseError> {
        self.hit();
        Ok(LicenseView::disabled())
    }
    fn request_trial(&self, email: &str) -> Result<TrialView, LicenseError> {
        self.hit();
        self.emails.lock().unwrap().push(email.to_owned());
        Ok(TrialView {
            accepted: true,
            message: MessageCode::TrialRequested,
        })
    }
    fn invalidation(&self) -> Option<LicenseInvalidation> {
        None
    }
}

const ACTIONS: [&str; 5] = [
    "status",
    "activate",
    "refresh",
    "deactivate",
    "request_trial",
];

fn host(trust: ServiceTrust) -> (HostServices, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let service = LicenseHostService::with_ops(fake.clone());
    let host = HostServices::builder()
        .license(trust, service)
        .build()
        .unwrap();
    (host, fake)
}

fn url(s: &str) -> tauri::Url {
    tauri::Url::parse(s).unwrap()
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

/// Run `action` the way a command does: the caller check first, then the service.
fn run(host: &HostServices, action: &str, f: &CallerFacts<'_>) -> Result<(), LicenseError> {
    let service = license_for(host, action, f)?;
    match action {
        "status" => {
            let _ = service.status();
        }
        "activate" => {
            let _ = service.activate("KEY-1234".into())?;
        }
        "refresh" => {
            let _ = service.refresh()?;
        }
        "deactivate" => {
            let _ = service.deactivate()?;
        }
        "request_trial" => {
            let _ = service.request_trial("a@example.com")?;
        }
        other => panic!("unknown action {other}"),
    }
    Ok(())
}

#[test]
fn the_trusted_window_reaches_every_licence_action_once() {
    let (host, fake) = host(ServiceTrust::for_labels(["settings"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/settings.html");
    for (i, action) in ACTIONS.iter().enumerate() {
        run(&host, action, &facts("settings", &[], Some(&page), &nav)).unwrap();
        assert_eq!(fake.calls(), i + 1, "{action}");
    }
    assert_eq!(*fake.keys.lock().unwrap(), vec!["KEY-1234"]);
    assert_eq!(*fake.emails.lock().unwrap(), vec!["a@example.com"]);
}

#[test]
fn a_content_webview_is_forbidden_for_every_action_and_the_client_is_never_touched() {
    // Its label is the trusted one; the registry of content webviews wins.
    let (host, fake) = host(ServiceTrust::for_labels(["settings"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let content = vec!["settings".to_owned()];
    for action in ACTIONS {
        assert_eq!(
            run(
                &host,
                action,
                &facts("settings", &content, Some(&page), &nav)
            ),
            Err(LicenseError::Forbidden),
            "{action}"
        );
    }
    assert_eq!(fake.calls(), 0);
}

#[test]
fn a_foreign_or_untrusted_origin_is_forbidden_and_the_client_is_never_touched() {
    let (host, fake) =
        host(ServiceTrust::for_labels(["settings"]).with_origins(["tauri://localhost"]));
    let nav = NavPolicy::default();
    let evil = url("https://evil.example/");
    let other_app_origin = url("http://tauri.localhost/");
    for action in ACTIONS {
        for page in [&evil, &other_app_origin] {
            assert_eq!(
                run(&host, action, &facts("settings", &[], Some(page), &nav)),
                Err(LicenseError::Forbidden),
                "{action} {page}"
            );
        }
        assert_eq!(
            run(&host, action, &facts("settings", &[], None, &nav)),
            Err(LicenseError::Forbidden),
            "{action} unreadable url"
        );
    }
    assert_eq!(fake.calls(), 0);
}

#[test]
fn another_window_and_a_non_window_are_forbidden_and_the_client_is_never_touched() {
    let (host, fake) = host(ServiceTrust::for_labels(["settings"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let mut child = facts("settings", &[], Some(&page), &nav);
    child.is_app_window = false;
    for action in ACTIONS {
        assert_eq!(
            run(&host, action, &facts("main", &[], Some(&page), &nav)),
            Err(LicenseError::Forbidden),
            "{action} main"
        );
        assert_eq!(
            run(&host, action, &child),
            Err(LicenseError::Forbidden),
            "{action} child"
        );
    }
    assert_eq!(fake.calls(), 0);
}

#[test]
fn a_disabled_action_is_forbidden_for_the_trusted_window_and_the_client_is_never_touched() {
    let (host, fake) = host(ServiceTrust::for_labels(["settings"]).with_actions(["status"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    for action in ACTIONS.iter().filter(|a| **a != "status") {
        assert_eq!(
            run(&host, action, &facts("settings", &[], Some(&page), &nav)),
            Err(LicenseError::Forbidden),
            "{action}"
        );
    }
    assert_eq!(fake.calls(), 0);
    run(&host, "status", &facts("settings", &[], Some(&page), &nav)).unwrap();
    assert_eq!(fake.calls(), 1);
}

#[test]
fn an_app_with_no_licence_service_answers_unsupported_to_a_trusted_caller_only() {
    let host = HostServices::builder().build().unwrap();
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    assert!(matches!(
        license_for(&host, "status", &facts("main", &[], Some(&page), &nav)),
        Err(LicenseError::Unsupported)
    ));
    // The shell-wide gate still speaks first: a content webview learns nothing but `forbidden`.
    let content = vec!["main".to_owned()];
    assert!(matches!(
        license_for(&host, "status", &facts("main", &content, Some(&page), &nav)),
        Err(LicenseError::Forbidden)
    ));
}

#[test]
fn a_slot_registered_as_another_type_is_unsupported_not_a_licence_service() {
    struct NotALicence;
    let host = HostServices::builder()
        .license(ServiceTrust::for_labels(["settings"]), NotALicence)
        .build()
        .unwrap();
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    assert!(matches!(
        license_for(&host, "status", &facts("settings", &[], Some(&page), &nav)),
        Err(LicenseError::Unsupported)
    ));
}

#[test]
fn a_key_outside_the_limits_is_not_accepted_and_never_reaches_the_client() {
    let fake = Arc::new(Fake::default());
    let service = LicenseHostService::with_ops(fake.clone());
    let refused = Err(LicenseError::Rejected {
        reason: RejectReason::KeyNotAccepted,
    });
    for key in [
        String::new(),
        "A".repeat(MAX_KEY_CHARS + 1),
        "KEY\u{0}1".to_owned(),
        "KEY\n1".to_owned(),
        "KEY\u{7f}".to_owned(),
    ] {
        assert_eq!(service.activate(key), refused);
    }
    assert_eq!(fake.calls(), 0);
    service.activate("A".repeat(MAX_KEY_CHARS)).unwrap();
    assert_eq!(fake.calls(), 1);
    // The client got the key exactly as sent (it trims and checks it itself).
    assert_eq!(fake.keys.lock().unwrap()[0].len(), MAX_KEY_CHARS);
}

#[test]
fn a_trial_address_outside_the_limits_is_declined_without_a_client_call() {
    let fake = Arc::new(Fake::default());
    let service = LicenseHostService::with_ops(fake.clone());
    for email in [
        String::new(),
        format!("{}@example.com", "a".repeat(MAX_EMAIL_CHARS)),
        "a@example.com\r\nBcc: x@example.com".to_owned(),
    ] {
        let view = service.request_trial(&email).unwrap();
        assert!(!view.accepted);
        assert_eq!(view.message, MessageCode::TrialRefused);
    }
    assert_eq!(fake.calls(), 0);
    assert!(service.request_trial("a@example.com").unwrap().accepted);
}

#[test]
fn only_windows_the_policy_trusts_for_status_hear_the_change_event() {
    let (host, _fake) =
        host(ServiceTrust::for_labels(["settings", "main"]).with_actions(["status"]));
    let nav = NavPolicy::default();
    let page = url("tauri://localhost/");
    let evil = url("https://evil.example/");
    let content = vec!["main".to_owned()];
    let policy = host.policy();
    assert!(hears_license_changes(
        policy,
        &facts("settings", &[], Some(&page), &nav)
    ));
    assert!(hears_license_changes(
        policy,
        &facts("main", &[], Some(&page), &nav)
    ));
    // A content webview, a window the service does not trust, a foreign page and an unreadable
    // page never hear it.
    assert!(!hears_license_changes(
        policy,
        &facts("main", &content, Some(&page), &nav)
    ));
    assert!(!hears_license_changes(
        policy,
        &facts("popup", &[], Some(&page), &nav)
    ));
    assert!(!hears_license_changes(
        policy,
        &facts("settings", &[], Some(&evil), &nav)
    ));
    assert!(!hears_license_changes(
        policy,
        &facts("settings", &[], None, &nav)
    ));
    // No `status` action, no event.
    let (quiet, _) = host_without_status();
    assert!(!hears_license_changes(
        quiet.policy(),
        &facts("settings", &[], Some(&page), &nav)
    ));
}

fn host_without_status() -> (HostServices, Arc<Fake>) {
    host(ServiceTrust::for_labels(["settings"]).with_actions(["activate"]))
}

/// A client whose `status()` stamps each read and, like the real one flushing a pending change,
/// signals the invalidation from INSIDE the read on the calling thread.
struct Signalling {
    invalidation: LicenseInvalidation,
    reads: AtomicUsize,
    /// Reads (1-based) that signal re-entrantly before returning.
    signal_in_reads: Vec<usize>,
}

impl Signalling {
    fn new(signal_in_reads: Vec<usize>) -> Arc<Self> {
        Arc::new(Self {
            invalidation: LicenseInvalidation::new(),
            reads: AtomicUsize::new(0),
            signal_in_reads,
        })
    }
}

impl LicenseOps for Signalling {
    fn status(&self) -> LicenseView {
        let stamp = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.signal_in_reads.contains(&stamp) {
            self.invalidation.signal();
        }
        let mut view = LicenseView::disabled();
        view.license_id_suffix = Some(format!("{stamp:04}"));
        view
    }
    fn activate(&self, _key: SecretString) -> Result<LicenseView, LicenseError> {
        Err(LicenseError::Unsupported)
    }
    fn refresh(&self) -> Result<RefreshView, LicenseError> {
        Err(LicenseError::Unsupported)
    }
    fn deactivate(&self) -> Result<LicenseView, LicenseError> {
        Err(LicenseError::Unsupported)
    }
    fn request_trial(&self, _email: &str) -> Result<TrialView, LicenseError> {
        Err(LicenseError::Unsupported)
    }
    fn invalidation(&self) -> Option<LicenseInvalidation> {
        Some(self.invalidation.clone())
    }
}

/// Run `work` on its own thread and fail instead of hanging when it does not finish.
fn finishes_within_seconds(work: impl FnOnce() + Send + 'static) {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        work();
        let _ = done.send(());
    });
    finished
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the listener deadlocked");
}

fn stamps(seen: &Mutex<Vec<String>>) -> Vec<usize> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|s| s.parse().unwrap())
        .collect()
}

#[test]
fn a_signal_from_inside_the_read_does_not_deadlock_and_the_events_stay_ordered() {
    // Read 1 signals again before it returns, exactly as `status()` does when the client had a
    // change pending: the listener re-enters on the same thread while its first round is running.
    let fake = Signalling::new(vec![1]);
    let service = LicenseHostService::with_ops(fake.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    service.on_change(move |view| {
        sink.lock()
            .unwrap()
            .push(view.license_id_suffix.clone().unwrap());
    });
    let invalidation = fake.invalidation.clone();
    finishes_within_seconds(move || invalidation.signal());
    // One read for the outside signal, one owed by the re-entrant signal; each emitted once, in
    // read order, and nothing is left owed.
    assert_eq!(fake.reads.load(Ordering::SeqCst), 2);
    assert_eq!(stamps(&seen), vec![1, 2]);
}

#[test]
fn a_chain_of_reentrant_signals_terminates_and_emits_every_read_in_order() {
    let fake = Signalling::new(vec![1, 2, 3, 4]);
    let service = LicenseHostService::with_ops(fake.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    service.on_change(move |view| {
        sink.lock()
            .unwrap()
            .push(view.license_id_suffix.clone().unwrap());
    });
    let invalidation = fake.invalidation.clone();
    finishes_within_seconds(move || invalidation.signal());
    assert_eq!(stamps(&seen), vec![1, 2, 3, 4, 5]);
}

#[test]
fn signals_from_many_threads_never_deadlock_and_events_are_in_read_order() {
    // Every read also signals (a busy client flushing on each status): the worst case.
    let fake = Signalling::new((1..=40).collect());
    let service = LicenseHostService::with_ops(fake.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    service.on_change(move |view| {
        sink.lock()
            .unwrap()
            .push(view.license_id_suffix.clone().unwrap());
    });
    let invalidation = fake.invalidation.clone();
    finishes_within_seconds(move || {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let invalidation = invalidation.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        invalidation.signal();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    });
    let order = stamps(&seen);
    assert!(!order.is_empty());
    // Strictly increasing: an older read is never emitted after a newer one.
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    // The last read was emitted.
    assert_eq!(*order.last().unwrap(), fake.reads.load(Ordering::SeqCst));
    // Nothing is left owed and the next signal is served: a quiet signal triggers a new read.
    let before = fake.reads.load(Ordering::SeqCst);
    let invalidation = fake.invalidation.clone();
    finishes_within_seconds(move || invalidation.signal());
    let after = fake.reads.load(Ordering::SeqCst);
    assert!(after > before);
    assert_eq!(stamps(&seen).last().copied(), Some(after));
}

#[test]
fn the_coalescer_runs_one_round_per_owed_read_and_returns_when_the_turn_is_taken() {
    let pump = ChangeCoalescer::default();
    let rounds = std::cell::Cell::new(0);
    pump.signal(|| rounds.set(rounds.get() + 1));
    assert_eq!(rounds.get(), 1);
    // Re-entered from inside a round: returns without running a nested round, and the outer
    // loop runs the owed one.
    let outer = std::cell::Cell::new(0);
    pump.signal(|| {
        outer.set(outer.get() + 1);
        if outer.get() == 1 {
            pump.signal(|| panic!("a nested round must not run"));
        }
    });
    assert_eq!(outer.get(), 2);
}

#[test]
fn the_wire_shapes_are_the_closed_ones_the_renderer_client_projects() {
    assert_eq!(EVENT_LICENSE_CHANGED, "rightkit://license-changed");
    // An error is `{ code, message }` (and `reason` only for a rejection), never text.
    assert_eq!(
        serde_json::to_value(LicenseError::Forbidden.to_command_error()).unwrap(),
        json!({"code": "forbidden", "message": "forbidden"})
    );
    assert_eq!(
        serde_json::to_value(LicenseError::Unsupported.to_command_error()).unwrap(),
        json!({"code": "unsupported", "message": "unsupported"})
    );
    assert_eq!(
        serde_json::to_value(
            LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted
            }
            .to_command_error()
        )
        .unwrap(),
        json!({"code": "rejected", "reason": "keyNotAccepted", "message": "keyNotAccepted"})
    );
    // A view names no token, key, secret or device id.
    let text = serde_json::to_string(&LicenseView::disabled())
        .unwrap()
        .to_lowercase();
    for word in ["token", "secret", "deviceid", "\"key\""] {
        assert!(!text.contains(word), "{word} in {text}");
    }
}
