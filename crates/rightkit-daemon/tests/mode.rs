//! M21/M23 runtime decision: fixtures N1-N5 of the D4 proposal.
//!
//! N1 runs in the default build (no `test-mode`). N2-N5 and the positive cases need the capability:
//! `cargo test -p rightkit-daemon --features test-mode --test mode`. CI must run both.

#![allow(dead_code, unused_imports)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rightkit_daemon::mode::{decide, roots_with, AppId, Mode, ModeInput, Refusal};
use rightkit_daemon::Suite;

const APP: AppId = AppId::with_dir("viewright", "ViewRight");

/// A fake process: a fixed environment, a fixed "real" per-user root, fixed protected folders.
struct Fake {
    env: HashMap<String, String>,
    real: PathBuf,
    protected: Vec<PathBuf>,
    panic_on_read: bool,
}

impl Fake {
    fn new(real: PathBuf, protected: Vec<PathBuf>) -> Fake {
        Fake {
            env: HashMap::new(),
            real,
            protected,
            panic_on_read: false,
        }
    }

    fn set(mut self, k: &str, v: impl AsRef<str>) -> Fake {
        self.env.insert(k.to_string(), v.as_ref().to_string());
        self
    }
}

impl ModeInput for Fake {
    fn env(&self, name: &str) -> Option<String> {
        assert!(!self.panic_on_read, "decide read the selector {name}");
        self.env.get(name).cloned()
    }

    fn per_user_default(&self) -> Result<Suite, rightkit_daemon::SuiteError> {
        Suite::at(&self.real)
    }

    fn protected_paths(&self, _real: &Suite) -> Vec<PathBuf> {
        self.protected.clone()
    }
}

/// A fake home: `<tmp>/home/{Documents,Pictures,Orthic Labs/shared}` and a sibling `<tmp>/qa`.
struct World {
    tmp: tempfile::TempDir,
    real_suite: PathBuf,
    orthic: PathBuf,
    documents: PathBuf,
    pictures: PathBuf,
    qa: PathBuf,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let home = base.join("home");
    let orthic = home.join("Orthic Labs");
    let real_suite = orthic.join("shared");
    let documents = home.join("Documents");
    let pictures = home.join("Pictures");
    let qa = base.join("qa");
    for d in [&real_suite, &documents, &pictures, &qa] {
        std::fs::create_dir_all(d).unwrap();
    }
    World {
        tmp,
        real_suite,
        orthic,
        documents,
        pictures,
        qa,
    }
}

impl World {
    fn fake(&self) -> Fake {
        Fake::new(
            self.real_suite.clone(),
            vec![
                self.orthic.clone(),
                self.documents.clone(),
                self.pictures.clone(),
            ],
        )
    }
}

/// An absolute path with a literal `..` component. On Windows, `join("..")` on a verbatim
/// (`\\?\`) path collapses the component away, so the verbatim prefix is stripped first and the
/// component written as text.
fn parent_dir_case(qa: &Path) -> String {
    let raw = s(qa);
    let raw = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    let sep = std::path::MAIN_SEPARATOR;
    format!("{raw}{sep}..{sep}qa")
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

// ---------------------------------------------------------------------------------------------
// N1: a build without the capability reads nothing.

#[cfg(not(feature = "test-mode"))]
#[test]
fn n1_release_reads_nothing() {
    let w = world();
    let mut fake = w
        .fake()
        .set("RIGHTKIT_UNDER_TEST", "1")
        .set("RIGHTKIT_QA_DATA_DIR", s(&w.qa))
        .set("RIGHTKIT_SUITE_ROOT", s(&w.qa));
    fake.panic_on_read = true;
    let mode = decide(&APP, &fake).expect("release decision");
    assert!(matches!(mode, Mode::Release));
    const { assert!(!rightkit_daemon::mode::TEST_CAPABILITY) };

    let roots = roots_with(&APP, &mode, &fake).unwrap();
    assert_eq!(roots.suite.root(), w.real_suite);
    assert_eq!(roots.app_dir, w.orthic.join("ViewRight"));
}

#[cfg(not(feature = "test-mode"))]
#[test]
fn n1_test_selector_drops_the_selector_without_the_capability() {
    let mode = Mode::Release;
    let value = rightkit_daemon::test_selector!(mode, "RIGHTKIT_QA_DATA_DIR");
    assert_eq!(value, None);
}

// ---------------------------------------------------------------------------------------------
// Capability build: N2-N5.

#[cfg(feature = "test-mode")]
mod capable {
    use super::*;

    const ACTIVATION: &str = "RIGHTKIT_UNDER_TEST";
    const ROOT: &str = "RIGHTKIT_QA_DATA_DIR";

    fn make_root(dir: &Path, app: &str) {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            dir.join(".rightkit-app.json"),
            format!("{{\"app\":\"{app}\"}}"),
        )
        .unwrap();
    }

    /// Counts how many stores a host would have opened: only an `Ok` decision reaches one.
    fn stores_opened(result: &Result<Mode, Refusal>) -> usize {
        usize::from(result.is_ok())
    }

    #[test]
    fn no_selector_is_a_release_run_that_resolves_from_the_account_database() {
        let w = world();
        let fake = w.fake();
        let mode = decide(&APP, &fake).unwrap();
        assert!(matches!(mode, Mode::Release));
        let roots = roots_with(&APP, &mode, &fake).unwrap();
        assert_eq!(roots.suite.root(), w.real_suite);
    }

    #[test]
    fn a_valid_bound_root_is_a_test_run_inside_the_root() {
        let w = world();
        make_root(&w.qa, "viewright");
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        let mode = decide(&APP, &fake).unwrap();
        let root = mode.test_root().expect("test run");
        assert_eq!(root.path(), w.qa);
        let roots = roots_with(&APP, &mode, &fake).unwrap();
        assert!(roots.suite.root().starts_with(&w.qa));
        assert!(roots.app_dir.starts_with(&w.qa));
        assert!(!roots.app_dir.starts_with(&w.orthic));
    }

    #[test]
    fn a_disabled_activation_value_is_not_activation() {
        let w = world();
        for off in ["0", "false", "FALSE", " "] {
            let fake = w.fake().set(ACTIVATION, off);
            assert!(matches!(decide(&APP, &fake).unwrap(), Mode::Release));
        }
    }

    #[test]
    fn selector_reads_only_in_a_test_run() {
        let w = world();
        make_root(&w.qa, "viewright");
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        let mode = decide(&APP, &fake).unwrap();
        // The process environment has no such variable: a test run reads it and finds nothing.
        assert_eq!(
            rightkit_daemon::test_selector!(mode, "RIGHTKIT_N1_DOES_NOT_EXIST"),
            None
        );
        let release = Mode::Release;
        assert_eq!(rightkit_daemon::test_selector!(release, "PATH"), None);
    }

    // N2
    #[test]
    fn n2_activation_without_a_usable_root_refuses_before_any_store_opens() {
        let w = world();
        let file = w.tmp.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let cases: Vec<(Fake, Refusal)> = vec![
            (
                w.fake().set(ACTIVATION, "1"),
                Refusal::ActivationWithoutRoot,
            ),
            (
                w.fake().set(ACTIVATION, "1").set(ROOT, ""),
                Refusal::ActivationWithoutRoot,
            ),
            (
                w.fake().set(ACTIVATION, "1").set(ROOT, "   "),
                Refusal::ActivationWithoutRoot,
            ),
            (
                w.fake().set(ACTIVATION, "1").set(ROOT, "relative/qa"),
                Refusal::RootNotAbsolute,
            ),
            (
                w.fake()
                    .set(ACTIVATION, "1")
                    .set(ROOT, parent_dir_case(&w.qa)),
                Refusal::RootNotAbsolute,
            ),
            (
                w.fake().set(ACTIVATION, "1").set(ROOT, s(&file)),
                Refusal::RootNotDirectory,
            ),
            (
                w.fake()
                    .set(ACTIVATION, "1")
                    .set(ROOT, s(&w.tmp.path().join("no-parent").join("qa"))),
                Refusal::RootNotDirectory,
            ),
            // A root without activation is just as partial.
            (w.fake().set(ROOT, s(&w.qa)), Refusal::RootWithoutActivation),
        ];
        let mut opened = 0;
        for (fake, want) in cases {
            let got = decide(&APP, &fake);
            opened += stores_opened(&got);
            assert_eq!(got.unwrap_err(), want);
        }
        assert_eq!(opened, 0);
    }

    // N3
    #[test]
    fn n3_a_root_that_overlaps_real_user_data_refuses() {
        let w = world();
        let inner = w.orthic.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        let docs_inner = w.documents.join("qa-here");
        std::fs::create_dir_all(&docs_inner).unwrap();
        let pics_inner = w.pictures.join("deep").join("qa");
        std::fs::create_dir_all(&pics_inner).unwrap();
        let parent_of_home = w.tmp.path().to_path_buf();
        for (root, label) in [
            (w.real_suite.clone(), "equal to the real suite root"),
            (w.orthic.clone(), "the Orthic Labs folder"),
            (inner.clone(), "inside the Orthic Labs folder"),
            (parent_of_home, "an ancestor of the real roots"),
            (w.documents.clone(), "the Documents folder"),
            (docs_inner, "inside Documents"),
            (pics_inner, "inside Pictures"),
        ] {
            make_root(&root, "viewright");
            let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&root));
            assert_eq!(
                decide(&APP, &fake).unwrap_err(),
                Refusal::RootOverlapsUserData,
                "{label}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn n3_a_symlinked_root_refuses_and_a_symlinked_ancestor_cannot_reach_real_data() {
        use std::os::unix::fs::symlink;
        let w = world();
        // Leaf symlink to a safe directory: refused as a symlink.
        make_root(&w.qa, "viewright");
        let leaf = w.tmp.path().join("leaf-link");
        symlink(&w.qa, &leaf).unwrap();
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&leaf));
        assert_eq!(decide(&APP, &fake).unwrap_err(), Refusal::RootHasSymlink);

        // Symlinked ancestor resolving into the real root: refused as overlap.
        let sub = w.real_suite.join("sub");
        make_root(&sub, "viewright");
        let link = w.tmp.path().join("anc-link");
        symlink(&w.real_suite, &link).unwrap();
        let via = link.join("sub");
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&via));
        assert_eq!(
            decide(&APP, &fake).unwrap_err(),
            Refusal::RootOverlapsUserData
        );
    }

    #[test]
    fn n3_a_cloud_synchronised_folder_refuses() {
        let w = world();
        let root = w.tmp.path().join("OneDrive - Org").join("qa");
        make_root(&root, "viewright");
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&root));
        assert_eq!(
            decide(&APP, &fake).unwrap_err(),
            Refusal::RootOnRemovableOrCloud
        );
    }

    #[cfg(unix)]
    #[test]
    fn n3_a_group_readable_root_refuses() {
        use std::os::unix::fs::PermissionsExt;
        let w = world();
        make_root(&w.qa, "viewright");
        std::fs::set_permissions(&w.qa, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        assert_eq!(decide(&APP, &fake).unwrap_err(), Refusal::RootNotPrivate);
    }

    // N4
    #[test]
    fn n4_an_unbound_root_or_a_binding_for_another_app_refuses() {
        let w = world();
        // Valid directory, no binding.
        std::fs::create_dir_all(&w.qa).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&w.qa, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        assert_eq!(
            decide(&APP, &fake).unwrap_err(),
            Refusal::RootNotCreatedByRunner
        );

        // Unparsable binding.
        std::fs::write(w.qa.join(".rightkit-app.json"), b"not json").unwrap();
        assert_eq!(
            decide(&APP, &fake).unwrap_err(),
            Refusal::RootNotCreatedByRunner
        );

        // Binding without an app field.
        std::fs::write(w.qa.join(".rightkit-app.json"), b"{}").unwrap();
        assert_eq!(
            decide(&APP, &fake).unwrap_err(),
            Refusal::RootNotCreatedByRunner
        );

        // Binding for another app.
        make_root(&w.qa, "heardright");
        assert_eq!(
            decide(&APP, &fake).unwrap_err(),
            Refusal::BindingAppMismatch
        );
    }

    // N5
    #[test]
    fn n5_reset_stays_inside_the_root() {
        let w = world();
        make_root(&w.qa, "viewright");
        std::fs::create_dir_all(w.qa.join("ViewRight").join("deep")).unwrap();
        std::fs::write(w.qa.join("ViewRight").join("deep").join("f"), b"x").unwrap();
        std::fs::write(w.qa.join("loose.txt"), b"x").unwrap();
        let sentinel = w.real_suite.join("sentinel.txt");
        std::fs::write(&sentinel, b"keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&w.real_suite, w.qa.join("escape")).unwrap();

        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        let mode = decide(&APP, &fake).unwrap();
        mode.test_root().unwrap().reset().unwrap();

        assert!(!w.qa.join("ViewRight").exists());
        assert!(!w.qa.join("loose.txt").exists());
        assert!(std::fs::symlink_metadata(w.qa.join("escape")).is_err());
        assert!(w.qa.join(".rightkit-app.json").exists(), "binding survives");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        assert!(w.real_suite.is_dir());
    }

    #[test]
    fn n5_reset_reverifies_the_binding() {
        let w = world();
        make_root(&w.qa, "viewright");
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        let mode = decide(&APP, &fake).unwrap();
        std::fs::write(w.qa.join("keep-me.txt"), b"x").unwrap();
        std::fs::remove_file(w.qa.join(".rightkit-app.json")).unwrap();
        assert_eq!(
            mode.test_root().unwrap().reset().unwrap_err(),
            Refusal::RootNotCreatedByRunner
        );
        assert!(w.qa.join("keep-me.txt").exists());
    }

    #[test]
    fn no_protected_paths_cannot_prove_disjointness_and_refuses() {
        let w = world();
        make_root(&w.qa, "viewright");
        let fake = Fake::new(w.real_suite.clone(), Vec::new())
            .set(ACTIVATION, "1")
            .set(ROOT, s(&w.qa));
        assert_eq!(decide(&APP, &fake).unwrap_err(), Refusal::UserRootUnknown);
    }

    #[test]
    fn a_validated_test_root_passes_the_build_tool_guard_for_its_own_suite_only() {
        let w = world();
        make_root(&w.qa, "viewright");
        let fake = w.fake().set(ACTIVATION, "1").set(ROOT, s(&w.qa));
        let mode = decide(&APP, &fake).unwrap();
        let suite = roots_with(&APP, &mode, &fake).unwrap().suite;
        rightkit_daemon::refuse_build_tool_real_data_for_mode(&suite, &mode).unwrap();
        // The release mode gets the plain rule (the world is under the temporary directory, so
        // it passes either way; the table with a root outside it is in `suite.rs`).
        rightkit_daemon::refuse_build_tool_real_data_for_mode(&suite, &Mode::Release).unwrap();
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // the marker is a const by design; the test documents it
    fn the_capability_marker_is_present() {
        assert!(rightkit_daemon::mode::TEST_CAPABILITY);
        assert_eq!(
            &rightkit_daemon::mode::TEST_MODE_MARKER[..],
            rightkit_daemon::mode::TEST_MODE_MARKER_TEXT.as_bytes()
        );
    }

    /// `ModeInput` over a fixed environment; the real root and protected paths come from the
    /// operating system, as in a real run.
    #[cfg(windows)]
    struct EnvOnly(HashMap<String, String>);

    #[cfg(windows)]
    impl ModeInput for EnvOnly {
        fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    /// QA-isolation regression (Windows): a launcher that points `USERPROFILE` and `LOCALAPPDATA`
    /// at a QA home must not move the real per-user root, and the QA home is a valid test root.
    /// The environment is changed only on a child process (never `set_var` in this one): the
    /// parent re-runs `child_under_qa_environment` in a subprocess with that environment.
    #[cfg(windows)]
    #[test]
    fn a_qa_home_environment_does_not_move_the_real_root() {
        let clean = Suite::per_user_default().unwrap();
        let profile = clean.root().ancestors().nth(4).unwrap().to_path_buf();
        let tmp = tempfile::tempdir().unwrap();
        let qa = std::fs::canonicalize(tmp.path()).unwrap().join("qa");
        rightkit_fs::private::ensure_private_dir(&qa).unwrap();
        for dir in ["Documents", "Pictures", "Desktop", r"AppData\Local"] {
            std::fs::create_dir_all(qa.join(dir)).unwrap();
        }
        std::fs::write(qa.join(".rightkit-app.json"), r#"{"app":"viewright"}"#).unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "capable::child_under_qa_environment",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("USERPROFILE", &qa)
            .env("LOCALAPPDATA", qa.join(r"AppData\Local"))
            .env("RK_CHILD_EXPECT_ROOT", clean.root())
            .env("RK_CHILD_PROFILE", &profile)
            .env("RK_CHILD_QA", &qa)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The child half of the test above; a no-op unless the parent started it.
    #[cfg(windows)]
    #[test]
    fn child_under_qa_environment() {
        let Some(expect) = std::env::var_os("RK_CHILD_EXPECT_ROOT") else {
            return;
        };
        let profile = PathBuf::from(std::env::var_os("RK_CHILD_PROFILE").unwrap());
        let qa = PathBuf::from(std::env::var_os("RK_CHILD_QA").unwrap());
        // The environment really is the QA one.
        assert_eq!(std::env::var_os("USERPROFILE").unwrap(), qa.as_os_str());

        assert_eq!(
            Suite::per_user_default().unwrap().root(),
            Path::new(&expect),
            "per_user_default followed the environment"
        );
        let input = |root: &Path| {
            EnvOnly(HashMap::from([
                (ACTIVATION.to_string(), "1".to_string()),
                (ROOT.to_string(), s(root).to_string()),
            ]))
        };
        let mode = decide(&APP, &input(&qa)).expect("a bound private QA home is a test root");
        assert_eq!(mode.test_root().expect("test run").path(), qa);

        // Real user locations, found from the account and only read, still refuse.
        let mut real = vec![profile.clone(), profile.join(r"AppData\Local")];
        real.extend(
            ["Documents", "Pictures", "Desktop"]
                .into_iter()
                .map(|d| profile.join(d))
                .filter(|p| p.is_dir()),
        );
        real.push(Path::new(&expect).parent().unwrap().to_path_buf());
        for root in real.into_iter().filter(|p| p.is_dir()) {
            assert_eq!(
                decide(&APP, &input(&root)).unwrap_err(),
                Refusal::RootOverlapsUserData,
                "{}",
                root.display()
            );
        }
    }
}
