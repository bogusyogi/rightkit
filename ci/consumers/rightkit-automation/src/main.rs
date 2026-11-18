// rightkit-automation 0.7.1 registry consumer, outside the workspace.
//
// Pins `rightkit-automation =0.7.1` exactly with `paths` and `desktop-context`, plus
// `rightkit-daemon =0.9.0` exactly so the returned `Suite` can be named. No
// path, git, workspace, patch, or test-seam dependencies. Runs on any host:
// nothing here touches a desktop, a window, or the input ladder.
//
// Proves, with public APIs only:
// - `paths::per_run_evidence_dir(label)` returns a `(Suite, PathBuf)` whose
//   `Suite` is the daemon 0.4 type (bound as `rightkit_daemon::Suite`, used
//   through `root()` and `env_derived()`), whose directory is absolute, exists,
//   is `<suite root>/automation`, sits inside the per-run temp suite root, and
//   is removed when the last `Suite` clone drops. The per-run temp suite never
//   reads `RIGHTKIT_SUITE_ROOT` or the per-user app-data root.
// - platform-neutral pure helpers: `policy::credential_label` flags credential
//   field labels and not ordinary ones; `sensitive::sensitive_value` flags a
//   Luhn-valid card number and not plain text.
//
// Not covered: `paths::evidence_dir` (resolves the real per-user suite root,
// so it would touch real user data), the macOS AX and Windows UIA backends,
// the input ladder, capture, focus guard, leases, and evidence journals.
// State: one per-run temp directory under the OS temp dir, removed on drop.
// No network, no assets, no real user data. Run only by the CI consumer lane.
use rightkit_automation::credential_label;
use rightkit_automation::paths::per_run_evidence_dir;
use rightkit_automation::sensitive::sensitive_value;
use std::path::PathBuf;

fn main() {
    #[cfg(windows)]
    windows_published_surface();
    #[cfg(target_os = "macos")]
    macos_probe_surface();
    let evidence: PathBuf;
    let root: PathBuf;
    {
        // Bound with the explicit daemon 0.9 type: a different Suite type fails to compile.
        let (suite, dir): (rightkit_daemon::Suite, PathBuf) =
            per_run_evidence_dir("rk-automation-consumer").expect("per-run evidence dir");
        assert!(
            !suite.env_derived(),
            "a per-run temp suite is never env-derived"
        );
        root = suite.root().to_path_buf();
        assert!(root.is_absolute(), "suite root must be absolute");
        assert!(
            root.starts_with(std::env::temp_dir()),
            "per-run suite must live under the OS temp dir"
        );
        assert!(dir.is_absolute(), "evidence dir must be absolute");
        assert_eq!(
            dir,
            root.join("automation"),
            "evidence dir is <suite>/automation"
        );
        assert!(
            dir.is_dir(),
            "evidence dir must exist while the suite lives"
        );
        evidence = dir;
    }
    assert!(
        !evidence.exists(),
        "evidence dir must be removed when the suite drops"
    );
    assert!(
        !root.exists(),
        "per-run suite root must be removed when the suite drops"
    );

    assert!(credential_label("Password"));
    assert!(credential_label("API key"));
    assert!(!credential_label("Search"));
    assert_eq!(
        sensitive_value("4111 1111 1111 1111"),
        Some("a card number")
    );
    assert_eq!(sensitive_value("hello world"), None);

    println!("rightkit-automation 0.7.1 registry consumer passed");
}

#[cfg(windows)]
fn windows_published_surface() {
    use rightkit_automation::win::uia::{
        ensure_com, focused_element, focused_element_editable, focused_element_is_password,
        focused_field_context, focused_field_raw_text, read_node, semantic, El,
    };
    // Existing published API references only; no COM initialization or UIA call is made.
    let _ensure_com = ensure_com;
    let _focused_element = focused_element;
    let _editable = focused_element_editable;
    let _password = focused_element_is_password;
    let _context = focused_field_context;
    let _raw_text = focused_field_raw_text;
    let _read_node = read_node;
    let _semantic = semantic;
    let _element_size = std::mem::size_of::<El>();
}

#[cfg(target_os = "macos")]
fn macos_probe_surface() {
    use rightkit_automation::desktop::paste_event_probe::{PasteEventProbe, ProbeLimits};
    let limits = ProbeLimits::default();
    assert_eq!(limits.max_events, 8);
    let _start = PasteEventProbe::start;
    // start installs a listen-only event tap; this recipe only checks its public API exists.
}
