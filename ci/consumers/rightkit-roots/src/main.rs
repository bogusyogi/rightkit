// rightkit-roots 0.1.0 registry consumer, outside the workspace, pinning `rightkit-roots =0.1.0`
// exactly (no features exist). The crate is run mode, the suite root, app roots and root cutover,
// moved unchanged from rightkit-daemon 0.7.0.
//
// Proves: `mode::{AppId, decide, roots_with, Mode, ModeInput}` against a fake input, the
// per-user default's shape (resolved only), `Suite::{at, per_run_temp, service_dir, data_dir,
// app_root}`, `valid_app_name`, `AppRoot::{lock_writer, migrate_from,
// migration_state}`, `lock::try_lock_exclusive`, `RootCutover::status`, `CUTOVER_RECORD`, and the
// `app_dirs` and `StateDir` re-exports.
// Does not prove: a cutover run, recovery or retirement (PK07 does), migration options, Windows
// reparse points, or the `app_dirs` resolver.
// State: per-run private temp suites, removed on drop. The real per-user root is resolved, never
// created or written. No network, no real user data.
use rightkit_roots::lock::try_lock_exclusive;
use rightkit_roots::mode::{self, AppId, Mode, ModeInput};
use rightkit_roots::{
    valid_app_name, MigrationOutcome, MigrationState, RootCutover, Suite, SuiteError,
    CUTOVER_RECORD, SHARED_DIR_NAME, SUITE_DIR_NAME,
};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

const APP: AppId = AppId::with_dir("rkroots", "RkRoots");

/// A fake input: its per-user default is a fixed suite root, and any environment read panics.
struct Fake(PathBuf);

impl ModeInput for Fake {
    fn env(&self, name: &str) -> Option<String> {
        panic!("mode must not read the environment ({name})")
    }

    fn per_user_default(&self) -> Result<Suite, SuiteError> {
        Suite::at(&self.0)
    }
}

fn ok(what: &str) {
    println!("ok: {what}");
}

fn main() -> Result<(), Box<dyn Error>> {
    // Run mode: one decision, release only, nothing read.
    let scratch = Suite::per_run_temp("roots-consumer")?;
    let fake_suite = scratch.root().join(SUITE_DIR_NAME).join(SHARED_DIR_NAME);
    let input = Fake(fake_suite.clone());
    let decided = mode::decide(&APP, &input)?;
    assert!(matches!(decided, Mode::Release));
    assert!(!decided.is_test());
    ok("mode::decide is Release and reads nothing");

    let roots = mode::roots_with(&APP, &decided, &input)?;
    assert_eq!(roots.suite.root(), fake_suite.as_path());
    assert_eq!(
        roots.app_dir,
        scratch.root().join(SUITE_DIR_NAME).join("RkRoots")
    );
    assert_eq!(APP.id(), "rkroots");
    assert_eq!(APP.dir_name(), "RkRoots");
    ok("mode::roots_with puts the app directory beside shared under Orthic Labs");

    // The real per-user default: resolved from the account database, never created here.
    match Suite::per_user_default() {
        Ok(real) => {
            assert!(real.root().is_absolute());
            assert!(real
                .root()
                .ends_with(PathBuf::from(SUITE_DIR_NAME).join(SHARED_DIR_NAME)));
            assert!(!real.env_derived());
            ok("Suite::per_user_default is absolute and ends in Orthic Labs/shared");
        }
        Err(SuiteError::NoRoot(why)) => ok(&format!("Suite::per_user_default fails closed: {why}")),
        Err(other) => return Err(other.into()),
    }

    // Explicit and per-run suites.
    assert!(matches!(
        Suite::at("relative/root"),
        Err(SuiteError::NotAbsolute(_))
    ));
    ok("Suite::at refuses a relative root");
    assert_eq!(
        scratch.service_dir("speech"),
        scratch.root().join("runtime").join("speech")
    );
    let shots = scratch.data_dir("shots")?;
    assert!(shots.is_dir());
    ok("per-run suite: service_dir and data_dir");

    assert!(valid_app_name("RkRoots"));
    assert!(!valid_app_name("../escape"));
    ok("name rules");

    // App root: inside a non-canonical suite it lives under <root>/apps.
    let app = scratch.app_root("RkRoots")?;
    assert_eq!(app.root(), scratch.root().join("apps").join("RkRoots"));
    assert_eq!(app.name(), "RkRoots");
    {
        let _writer = app.lock_writer()?;
        ok("AppRoot::lock_writer");
    }

    // Migration of a legacy folder into the app root.
    let legacy = scratch.root().join("legacy");
    fs::create_dir_all(legacy.join("sub"))?;
    fs::write(legacy.join("a.txt"), b"alpha")?;
    fs::write(legacy.join("sub").join("b.txt"), b"beta")?;
    match app.migrate_from(&legacy)? {
        MigrationOutcome::Migrated { files, bytes } => {
            assert_eq!(files, 2);
            assert_eq!(bytes, 9);
        }
        other => panic!("expected Migrated, got {other:?}"),
    }
    assert_eq!(fs::read(app.root().join("a.txt"))?, b"alpha");
    assert_eq!(fs::read(legacy.join("a.txt"))?, b"alpha");
    assert_eq!(app.migration_state(&legacy)?, Some(MigrationState::Ready));
    assert!(matches!(
        app.migrate_from(&legacy)?,
        MigrationOutcome::AlreadyMigrated
    ));
    assert!(matches!(
        app.migrate_from(&scratch.root().join("absent"))?,
        MigrationOutcome::NothingToMigrate
    ));
    ok("AppRoot::migrate_from: Migrated, AlreadyMigrated, NothingToMigrate");

    // The exclusive lock file.
    let lock_path = scratch.root().join("consumer.lock");
    let held = try_lock_exclusive(&lock_path)?.expect("first lock is taken");
    assert!(
        try_lock_exclusive(&lock_path)?.is_none(),
        "refused while held"
    );
    drop(held);
    assert!(try_lock_exclusive(&lock_path)?.is_some());
    ok("lock::try_lock_exclusive");

    // Root cutover: nothing recorded before a first cutover.
    assert_eq!(CUTOVER_RECORD, ".rightkit-root-cutover.json");
    assert!(RootCutover::status(&app)?.is_none());
    ok("RootCutover::status before a cutover");

    // The other two root implementations are the rightkit-fs items, re-exported.
    let _: fn(&std::path::Path) -> Result<rightkit_roots::StateDir, _> =
        rightkit_roots::StateDir::open;
    assert_eq!(rightkit_roots::app_dirs::DEFAULT_ORG, SUITE_DIR_NAME);
    ok("app_dirs and StateDir re-exports");

    println!("OK rightkit-roots 0.1.0 (registry consumer)");
    Ok(())
}
