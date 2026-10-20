//! `RootCutover` (PK07-3, PK07-6): ordering, abort, the durable switch, roll-forward, path
//! substitution, collisions and the separate identity-checked cleanup. Participants are plain
//! in-process fakes that log every hook; no network, no database.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rightkit_daemon::{
    AppRoot, CutoverContext, CutoverError, CutoverParticipant, CutoverPhase, CutoverStep, FsMove,
    MigrateOptions, ParticipantError, RecoverOutcome, RetireMode, RootCutover, RootSelection,
    Settlement, Suite, CUTOVER_RECORD,
};

type Log = Arc<Mutex<Vec<String>>>;
type RelocateHook = Box<dyn Fn(&CutoverContext) + Send>;

struct Probe {
    name: String,
    log: Log,
    fail: Arc<Mutex<Option<CutoverStep>>>,
    panic_at: Option<CutoverStep>,
    owned: Vec<PathBuf>,
    on_relocate: Option<RelocateHook>,
}

impl Probe {
    fn new(name: &str, log: &Log) -> Self {
        Probe {
            name: name.to_string(),
            log: log.clone(),
            fail: Arc::new(Mutex::new(None)),
            panic_at: None,
            owned: Vec::new(),
            on_relocate: None,
        }
    }

    /// Fail once at `step`; the next attempt at that step succeeds.
    fn failing(self, step: CutoverStep) -> Self {
        *self.fail.lock().unwrap() = Some(step);
        self
    }

    fn panicking(mut self, step: CutoverStep) -> Self {
        self.panic_at = Some(step);
        self
    }

    fn owning(mut self, rel: &str) -> Self {
        self.owned.push(PathBuf::from(rel));
        self
    }

    fn on_relocate(mut self, hook: impl Fn(&CutoverContext) + Send + 'static) -> Self {
        self.on_relocate = Some(Box::new(hook));
        self
    }

    fn step(
        &mut self,
        step: CutoverStep,
        _cx: &CutoverContext,
        note: &str,
    ) -> Result<(), ParticipantError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{}{}", self.name, step.as_str(), note));
        if self.panic_at == Some(step) {
            panic!("injected panic");
        }
        let mut fail = self.fail.lock().unwrap();
        if *fail == Some(step) {
            *fail = None;
            return Err(ParticipantError::new("injected failure"));
        }
        Ok(())
    }
}

impl CutoverParticipant for Probe {
    fn name(&self) -> &str {
        &self.name
    }

    fn owned_paths(&self) -> Vec<PathBuf> {
        self.owned.clone()
    }

    fn prepare(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        self.step(CutoverStep::Prepare, cx, "")
    }

    fn quiesce(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        self.step(CutoverStep::Quiesce, cx, "")
    }

    fn relocate(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        if let Some(hook) = &self.on_relocate {
            hook(cx);
        }
        self.step(CutoverStep::Relocate, cx, "")
    }

    fn reopen(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        self.step(CutoverStep::Reopen, cx, "")
    }

    fn settle(
        &mut self,
        cx: &CutoverContext,
        settlement: Settlement,
    ) -> Result<(), ParticipantError> {
        let note = match settlement {
            Settlement::Committed => ":committed",
            _ => ":aborted",
        };
        self.step(CutoverStep::Settle, cx, note)
    }
}

struct Env {
    _dir: tempfile::TempDir,
    suite: Suite,
    old: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let old = dir.path().join("old");
    fs::create_dir_all(old.join("sub")).unwrap();
    fs::create_dir_all(old.join("empty")).unwrap();
    fs::write(old.join("a.txt"), b"alpha").unwrap();
    fs::write(old.join("sub").join("b.txt"), b"beta").unwrap();
    Env {
        _dir: dir,
        suite,
        old,
    }
}

fn app(env: &Env) -> AppRoot {
    env.suite.app_root("CutoverApp").unwrap()
}

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn lines(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, rel: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(base.join(rel)).unwrap().flatten() {
            let r = rel.join(entry.file_name());
            let key = r.to_string_lossy().replace('\\', "/");
            if entry.file_type().unwrap().is_dir() {
                out.insert(format!("{key}/"), Vec::new());
                walk(base, &r, out);
            } else {
                out.insert(key, fs::read(base.join(&r)).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}

fn tree() -> FsMove {
    FsMove::Tree(MigrateOptions::strict())
}

#[test]
fn a_cutover_runs_the_hooks_in_order_copies_the_tree_and_selects_the_new_root() {
    let env = env();
    let app = app(&env);
    let before = snapshot(&env.old);
    let log = log();
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .fs_move(tree())
        .participant(Probe::new("one", &log))
        .participant(Probe::new("two", &log));

    let report = cutover.run().expect("cutover");
    assert_eq!(report.selected, RootSelection::New);
    assert_eq!(report.generation, 1);
    assert!(!report.already_completed);
    assert!(report.migration.is_some());
    assert_eq!(
        lines(&log),
        [
            "one:prepare",
            "two:prepare",
            "one:quiesce",
            "two:quiesce",
            "one:relocate",
            "two:relocate",
            "one:reopen",
            "two:reopen",
            "one:settle:committed",
            "two:settle:committed",
        ]
    );

    // Files, nested files and the empty directory are in the new root; the old root is untouched.
    let new = app.root();
    assert_eq!(fs::read(new.join("a.txt")).unwrap(), b"alpha");
    assert_eq!(fs::read(new.join("sub").join("b.txt")).unwrap(), b"beta");
    assert!(new.join("empty").is_dir());
    assert_eq!(snapshot(&env.old), before);

    let status = RootCutover::status(&app).unwrap().expect("record");
    assert_eq!(status.phase, CutoverPhase::Completed);
    assert_eq!(status.selected, RootSelection::New);
    assert!(!status.needs_recovery());
    assert_eq!(
        RootCutover::selected_root(&app, &env.old).unwrap(),
        status.new_root
    );

    // Idempotent: a completed cutover runs nothing.
    let again = cutover.run().expect("rerun");
    assert!(again.already_completed);
    assert_eq!(lines(&log).len(), 10);
}

#[test]
fn without_a_record_the_legacy_root_is_selected() {
    let env = env();
    let app = app(&env);
    assert!(RootCutover::status(&app).unwrap().is_none());
    assert_eq!(RootCutover::selected_root(&app, &env.old).unwrap(), env.old);
}

#[test]
fn a_failure_before_the_switch_settles_prepared_participants_in_reverse_and_keeps_the_old_root() {
    let env = env();
    let app = app(&env);
    let before = snapshot(&env.old);
    let log = log();
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .fs_move(tree())
        .participant(Probe::new("one", &log))
        .participant(Probe::new("two", &log).failing(CutoverStep::Quiesce));

    let err = cutover.run().unwrap_err();
    assert!(
        matches!(
            &err,
            CutoverError::Participant { name, step: CutoverStep::Quiesce, .. } if name == "two"
        ),
        "{err}"
    );
    assert_eq!(
        lines(&log),
        [
            "one:prepare",
            "two:prepare",
            "one:quiesce",
            "two:quiesce",
            "two:settle:aborted",
            "one:settle:aborted",
        ]
    );
    // Nothing was copied, the old root is as it was and stays the authority.
    assert!(!app.root().join("a.txt").exists());
    assert_eq!(snapshot(&env.old), before);
    let status = RootCutover::status(&app).unwrap().unwrap();
    assert_eq!(status.phase, CutoverPhase::Aborted);
    assert_eq!(status.selected, RootSelection::Old);
    assert!(status.error.as_deref().unwrap_or("").contains("quiesce"));
    assert_eq!(
        RootCutover::selected_root(&app, &env.old).unwrap(),
        fs::canonicalize(&env.old).unwrap()
    );

    // The same cutover again, once the fault is gone: the next generation completes.
    let report = cutover.run().expect("rerun after abort");
    assert_eq!(report.generation, 2);
    assert_eq!(fs::read(app.root().join("a.txt")).unwrap(), b"alpha");
    assert_eq!(
        RootCutover::status(&app).unwrap().unwrap().phase,
        CutoverPhase::Completed
    );
}

#[test]
fn a_participant_whose_prepare_failed_is_not_settled() {
    let env = env();
    let log = log();
    let mut cutover = RootCutover::new(app(&env), &env.old)
        .participant(Probe::new("one", &log))
        .participant(Probe::new("two", &log).failing(CutoverStep::Prepare));
    assert!(cutover.run().is_err());
    assert_eq!(
        lines(&log),
        ["one:prepare", "two:prepare", "one:settle:aborted"]
    );
}

#[test]
fn a_panicking_participant_is_a_failed_hook_not_an_unwind() {
    let env = env();
    let log = log();
    let mut cutover = RootCutover::new(app(&env), &env.old)
        .fs_move(tree())
        .participant(Probe::new("one", &log))
        .participant(Probe::new("two", &log).panicking(CutoverStep::Relocate));
    let err = cutover.run().unwrap_err();
    assert!(
        matches!(
            &err,
            CutoverError::Participant { step: CutoverStep::Relocate, error, .. }
                if error.message().contains("panicked")
        ),
        "{err}"
    );
    assert!(lines(&log).contains(&"one:settle:aborted".to_string()));
    assert!(lines(&log).contains(&"two:settle:aborted".to_string()));
}

#[test]
fn a_failed_reopen_leaves_the_switch_durable_and_recover_rolls_forward() {
    let env = env();
    let app = app(&env);
    let before = snapshot(&env.old);
    let log = log();
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .fs_move(tree())
        .participant(Probe::new("one", &log))
        .participant(Probe::new("two", &log).failing(CutoverStep::Reopen));

    let err = cutover.run().unwrap_err();
    assert!(
        matches!(&err, CutoverError::ReopenPending { name, generation: 1, .. } if name == "two"),
        "{err}"
    );
    // The new root is the authority now; no participant was settled as aborted.
    let status = RootCutover::status(&app).unwrap().unwrap();
    assert_eq!(status.phase, CutoverPhase::Switched);
    assert_eq!(status.selected, RootSelection::New);
    assert!(status.needs_recovery());
    assert!(!lines(&log).iter().any(|l| l.ends_with(":aborted")));
    assert_eq!(snapshot(&env.old), before);

    // A restart: fresh participants, fresh cutover value; recover finishes it.
    let log2 = self::log();
    let mut restarted = RootCutover::new(app.clone(), &env.old)
        .participant(Probe::new("one", &log2))
        .participant(Probe::new("two", &log2));
    match restarted.recover().expect("recover") {
        RecoverOutcome::Resumed(report) => {
            assert!(report.resumed);
            assert_eq!(report.generation, 1);
        }
        other => panic!("expected Resumed, got {other:?}"),
    }
    assert_eq!(
        lines(&log2),
        [
            "one:reopen",
            "two:reopen",
            "one:settle:committed",
            "two:settle:committed"
        ]
    );
    assert_eq!(
        RootCutover::status(&app).unwrap().unwrap().phase,
        CutoverPhase::Completed
    );
    assert!(matches!(
        restarted.recover().unwrap(),
        RecoverOutcome::Completed
    ));
}

#[test]
fn recover_leaves_a_cutover_that_never_switched_alone() {
    let env = env();
    let app = app(&env);
    let log = log();
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .participant(Probe::new("one", &log).failing(CutoverStep::Relocate));
    assert!(cutover.run().is_err());
    let before = lines(&log).len();
    assert!(matches!(
        cutover.recover().unwrap(),
        RecoverOutcome::Nothing
    ));
    assert_eq!(lines(&log).len(), before, "recover must not run hooks");
    assert_eq!(
        RootCutover::status(&app).unwrap().unwrap().selected,
        RootSelection::Old
    );
}

#[test]
fn an_old_root_replaced_before_the_switch_aborts_the_cutover() {
    let env = env();
    let app = app(&env);
    let log = log();
    let old = env.old.clone();
    let aside = env.old.with_file_name("old-moved");
    let swap = {
        let (old, aside) = (old.clone(), aside.clone());
        move |_: &CutoverContext| {
            fs::rename(&old, &aside).unwrap();
            fs::create_dir_all(&old).unwrap();
            fs::write(old.join("impostor.txt"), b"not the old root").unwrap();
        }
    };
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .participant(Probe::new("one", &log).on_relocate(swap));
    let err = cutover.run().unwrap_err();
    assert!(matches!(err, CutoverError::PathSubstituted(_)), "{err}");
    assert!(lines(&log).contains(&"one:settle:aborted".to_string()));
    assert!(matches!(
        RootCutover::status(&app),
        Err(CutoverError::PathSubstituted(_))
    ));
    assert!(matches!(
        RootCutover::selected_root(&app, &env.old),
        Err(CutoverError::PathSubstituted(_))
    ));
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(app.root().join(CUTOVER_RECORD)).unwrap()).unwrap();
    assert_eq!(record["phase"], "aborted");
    // The substituted directory was never touched.
    assert_eq!(
        fs::read(old.join("impostor.txt")).unwrap(),
        b"not the old root"
    );
}

#[test]
fn companions_publish_through_one_transaction_and_a_rerun_accepts_identical_files() {
    let env = env();
    let app = app(&env);
    fs::write(env.old.join("settings.json"), b"{\"a\":1}").unwrap();
    fs::create_dir_all(env.old.join("cache")).unwrap();
    fs::write(env.old.join("cache").join("c.bin"), [1u8, 2, 3]).unwrap();
    let companions = vec![
        PathBuf::from("settings.json"),
        PathBuf::from("cache").join("c.bin"),
    ];
    // A previous attempt already put one of them in place, byte for byte.
    fs::write(app.root().join("settings.json"), b"{\"a\":1}").unwrap();

    let mut cutover =
        RootCutover::new(app.clone(), &env.old).fs_move(FsMove::Companions(companions));
    let report = cutover.run().expect("companions");
    assert_eq!(report.companions.len(), 2);
    assert_eq!(
        fs::read(app.root().join("cache").join("c.bin")).unwrap(),
        [1u8, 2, 3]
    );
    // The tree was not copied: only the named files travel.
    assert!(!app.root().join("a.txt").exists());
}

#[test]
fn a_companion_destination_with_other_content_is_a_collision_and_nothing_is_overwritten() {
    let env = env();
    let app = app(&env);
    fs::write(env.old.join("settings.json"), b"old settings").unwrap();
    fs::write(env.old.join("other.json"), b"other").unwrap();
    fs::write(app.root().join("settings.json"), b"different").unwrap();
    let mut cutover = RootCutover::new(app.clone(), &env.old).fs_move(FsMove::Companions(vec![
        PathBuf::from("other.json"),
        PathBuf::from("settings.json"),
    ]));
    let err = cutover.run().unwrap_err();
    assert!(matches!(err, CutoverError::Collision(_)), "{err}");
    assert_eq!(
        fs::read(app.root().join("settings.json")).unwrap(),
        b"different"
    );
    assert!(
        !app.root().join("other.json").exists(),
        "the collision is found before anything is published"
    );
    assert_eq!(
        RootCutover::status(&app).unwrap().unwrap().selected,
        RootSelection::Old
    );
}

#[test]
fn companions_must_be_plain_relative_files_not_on_a_participant_path() {
    let absolute = if cfg!(windows) {
        PathBuf::from("C:\\Windows\\win.ini")
    } else {
        PathBuf::from("/etc/hosts")
    };
    for bad in [PathBuf::from("../escape.txt"), absolute] {
        let env = env();
        let log = log();
        let mut cutover = RootCutover::new(app(&env), &env.old)
            .fs_move(FsMove::Companions(vec![bad.clone()]))
            .participant(Probe::new("db", &log));
        let err = cutover.run().unwrap_err();
        assert!(
            matches!(err, CutoverError::BadCompanion(_)),
            "{bad:?}: {err}"
        );
        assert!(lines(&log).is_empty(), "validation runs before any hook");
    }

    {
        let env = env();
        let app = app(&env);
        let before = snapshot(&env.old);
        let log = log();
        let mut directory = RootCutover::new(app.clone(), &env.old)
            .fs_move(FsMove::Companions(vec![PathBuf::from("empty")]))
            .participant(Probe::new("db", &log));
        let err = directory.run().unwrap_err();
        assert!(matches!(err, CutoverError::BadCompanion(_)), "{err}");
        assert_eq!(
            lines(&log),
            [
                "db:prepare",
                "db:quiesce",
                "db:relocate",
                "db:settle:aborted"
            ]
        );
        assert_eq!(snapshot(&env.old), before);
        assert!(!app.root().join("a.txt").exists());
        assert_eq!(
            RootCutover::status(&app).unwrap().unwrap().phase,
            CutoverPhase::Aborted
        );
    }

    let env = env();
    let log = log();
    let mut conflicted = RootCutover::new(app(&env), &env.old)
        .fs_move(FsMove::Companions(vec![PathBuf::from("sub").join("b.txt")]))
        .participant(Probe::new("db", &log).owning("sub"));
    let err = conflicted.run().unwrap_err();
    assert!(
        matches!(err, CutoverError::OwnedPathConflict { .. }),
        "{err}"
    );
    assert!(lines(&log).is_empty(), "validation runs before any hook");
}

#[test]
fn a_tree_move_leaves_out_the_paths_a_participant_owns() {
    let env = env();
    let app = app(&env);
    fs::create_dir_all(env.old.join("db")).unwrap();
    fs::write(
        env.old.join("db").join("data.sqlite"),
        b"raw database bytes",
    )
    .unwrap();
    fs::create_dir_all(env.old.join("db2")).unwrap();
    fs::write(env.old.join("db2").join("x"), b"x").unwrap();
    let new_root = app.root().to_path_buf();
    let relocate = move |_: &CutoverContext| {
        fs::create_dir_all(new_root.join("db")).unwrap();
        fs::write(
            new_root.join("db").join("data.sqlite"),
            b"relocated by the owner",
        )
        .unwrap();
    };
    let log = log();
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .fs_move(tree())
        .participant(Probe::new("db", &log).owning("db").on_relocate(relocate));
    cutover.run().expect("cutover");
    assert_eq!(
        fs::read(app.root().join("db").join("data.sqlite")).unwrap(),
        b"relocated by the owner",
        "the tree move must not have copied the owned path"
    );
    assert!(
        app.root().join("db2").join("x").exists(),
        "an excluded path covers whole components only"
    );
    assert!(app.root().join("a.txt").exists());
}

#[test]
fn a_second_cutover_for_the_same_root_is_busy_not_interleaved() {
    let env = env();
    let app = app(&env);
    let seen: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let hook = {
        let (app, old, seen) = (app.clone(), env.old.clone(), seen.clone());
        move |_: &CutoverContext| {
            let mut second = RootCutover::new(app.clone(), &old);
            let busy = matches!(second.run(), Err(CutoverError::Busy(_)));
            *seen.lock().unwrap() = Some(busy);
        }
    };
    let log = log();
    let mut cutover =
        RootCutover::new(app, &env.old).participant(Probe::new("one", &log).on_relocate(hook));
    cutover.run().expect("cutover");
    assert_eq!(*seen.lock().unwrap(), Some(true));
}

#[test]
fn recovery_refuses_missing_participants_without_completing_the_generation() {
    let env = env();
    let app = app(&env);
    let log = log();
    let mut initial = RootCutover::new(app.clone(), &env.old)
        .participant(Probe::new("database", &log).failing(CutoverStep::Reopen));
    assert!(matches!(
        initial.run(),
        Err(CutoverError::ReopenPending { .. })
    ));
    let mut missing = RootCutover::new(app.clone(), &env.old);
    assert!(matches!(
        missing.recover(),
        Err(CutoverError::RecordMismatch(_))
    ));
    assert_eq!(
        RootCutover::status(&app).unwrap().unwrap().phase,
        CutoverPhase::Switched
    );
    let mut restored =
        RootCutover::new(app.clone(), &env.old).participant(Probe::new("database", &log));
    assert!(matches!(
        restored.recover().unwrap(),
        RecoverOutcome::Resumed(_)
    ));
    assert_eq!(RootCutover::status(&app).unwrap().unwrap().generation, 1);
}

#[test]
fn a_completed_record_copied_to_another_directory_never_selects_that_directory() {
    let env = env();
    let app = app(&env);
    RootCutover::new(app.clone(), &env.old).run().unwrap();
    let replacement = env.suite.app_root("OtherApp").unwrap();
    fs::copy(
        app.root().join(CUTOVER_RECORD),
        replacement.root().join(CUTOVER_RECORD),
    )
    .unwrap();
    assert!(matches!(
        RootCutover::selected_root(&replacement, &env.old),
        Err(CutoverError::RecordMismatch(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_cutover_record_symlink_is_refused_without_following_its_target() {
    let env = env();
    let app = app(&env);
    let outside = env.old.join("unrelated.json");
    fs::write(&outside, b"not a cutover record").unwrap();
    std::os::unix::fs::symlink(&outside, app.root().join(CUTOVER_RECORD)).unwrap();
    assert!(RootCutover::status(&app).is_err());
    assert_eq!(fs::read(&outside).unwrap(), b"not a cutover record");
}

#[test]
fn an_unreadable_or_newer_record_fails_closed() {
    let env = env();
    let app = app(&env);
    let record = app.root().join(CUTOVER_RECORD);
    for body in ["not json", "{\"version\":99}", "{\"version\":1}"] {
        fs::write(&record, body).unwrap();
        assert!(
            matches!(
                RootCutover::status(&app).unwrap_err(),
                CutoverError::RecordUnreadable(_)
            ),
            "{body}"
        );
        assert!(matches!(
            RootCutover::selected_root(&app, &env.old).unwrap_err(),
            CutoverError::RecordUnreadable(_)
        ));
        let mut cutover = RootCutover::new(app.clone(), &env.old);
        assert!(matches!(
            cutover.run().unwrap_err(),
            CutoverError::RecordUnreadable(_)
        ));
    }
}

#[test]
fn retiring_the_old_root_is_a_separate_identity_checked_step() {
    let env = env();
    let app = app(&env);

    // Not before the cutover exists or completes.
    assert!(RootCutover::retire_old_root(&app, RetireMode::RenameAside).is_err());
    let log = log();
    let mut failing = RootCutover::new(app.clone(), &env.old)
        .participant(Probe::new("one", &log).failing(CutoverStep::Relocate));
    assert!(failing.run().is_err());
    assert!(matches!(
        RootCutover::retire_old_root(&app, RetireMode::RenameAside).unwrap_err(),
        CutoverError::NotCompleted(CutoverPhase::Aborted)
    ));
    assert!(env.old.join("a.txt").exists());

    failing.run().expect("second attempt");
    assert!(env.old.join("a.txt").exists(), "completing never retires");

    let retired = RootCutover::retire_old_root(&app, RetireMode::RenameAside).expect("retire");
    assert!(!retired.already);
    assert!(!env.old.exists());
    assert_eq!(fs::read(retired.path.join("a.txt")).unwrap(), b"alpha");
    let again = RootCutover::retire_old_root(&app, RetireMode::RenameAside).unwrap();
    assert!(again.already);
    assert_eq!(again.path, retired.path);
    assert_eq!(
        RootCutover::status(&app).unwrap().unwrap().retired,
        Some(retired.path)
    );
}

#[test]
fn retire_refuses_a_path_that_is_no_longer_the_directory_that_was_cut_over() {
    let env = env();
    let app = app(&env);
    let mut cutover = RootCutover::new(app.clone(), &env.old).fs_move(tree());
    cutover.run().expect("cutover");

    // Someone moves the real old root away and puts another directory at its path.
    let real = env.old.with_file_name("real-old");
    fs::rename(&env.old, &real).unwrap();
    fs::create_dir_all(&env.old).unwrap();
    fs::write(env.old.join("precious.txt"), b"someone else's data").unwrap();

    let err = RootCutover::retire_old_root(&app, RetireMode::RenameAside).unwrap_err();
    assert!(matches!(err, CutoverError::PathSubstituted(_)), "{err}");
    assert_eq!(
        fs::read(env.old.join("precious.txt")).unwrap(),
        b"someone else's data"
    );
    assert!(real.join("a.txt").exists());
}

#[test]
fn nested_or_missing_old_roots_are_refused_before_anything_is_written() {
    let env = env();
    let app = app(&env);
    let inside = app.root().join("inner");
    fs::create_dir_all(&inside).unwrap();
    for bad in [
        app.root().to_path_buf(),
        inside,
        env.old.join("missing"),
        PathBuf::from("relative/old"),
    ] {
        let mut cutover = RootCutover::new(app.clone(), &bad);
        let err = cutover.run().unwrap_err();
        assert!(
            matches!(err, CutoverError::InvalidRoot { .. }),
            "{bad:?}: {err}"
        );
    }
    assert!(RootCutover::status(&app).unwrap().is_none());
}

#[test]
fn an_excluded_path_is_not_copied_and_does_not_make_the_move_incomplete() {
    let env = env();
    let app = app(&env);
    fs::create_dir_all(env.old.join("db")).unwrap();
    fs::write(env.old.join("db").join("data"), b"d").unwrap();
    fs::write(env.old.join("db-notes.txt"), b"n").unwrap();
    let report = app
        .migrate_from_with(&env.old, MigrateOptions::strict().exclude("db"))
        .expect("migrate");
    assert!(report.complete());
    assert!(!app.root().join("db").exists());
    assert!(app.root().join("db-notes.txt").exists());
    assert!(app.root().join("a.txt").exists());
}

#[test]
fn a_ready_copy_never_hides_changes_in_the_still_authoritative_old_root() {
    for removed in [false, true] {
        let env = env();
        let app = app(&env);
        app.migrate_from(&env.old).unwrap();
        if removed {
            fs::remove_file(env.old.join("a.txt")).unwrap();
        } else {
            fs::write(env.old.join("a.txt"), b"new authoritative bytes").unwrap();
        }
        let mut cutover = RootCutover::new(app.clone(), &env.old).fs_move(tree());
        assert!(matches!(cutover.run(), Err(CutoverError::Migrate(_))));
        assert_eq!(
            RootCutover::selected_root(&app, &env.old).unwrap(),
            fs::canonicalize(&env.old).unwrap()
        );
        assert_eq!(fs::read(app.root().join("a.txt")).unwrap(), b"alpha");
    }
}

#[test]
fn a_differently_cased_exclusion_never_silently_omits_a_tree_entry() {
    let env = env();
    let app = app(&env);
    fs::write(env.old.join("DB"), b"must not be omitted").unwrap();
    let mut cutover = RootCutover::new(app.clone(), &env.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict().exclude("db")));
    assert!(matches!(cutover.run(), Err(CutoverError::Migrate(_))));
    assert_eq!(
        RootCutover::selected_root(&app, &env.old).unwrap(),
        fs::canonicalize(&env.old).unwrap()
    );
    assert_eq!(
        fs::read(env.old.join("DB")).unwrap(),
        b"must not be omitted"
    );
}
