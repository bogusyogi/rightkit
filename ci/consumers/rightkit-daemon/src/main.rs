// rightkit-daemon 0.4.0 registry consumer, outside the workspace, pinning `rightkit-daemon =0.4.0`
// exactly with default features only (the `local` marker). It avoids `remote`, `remote-loopback`,
// `link`, `relay` and `test-mode`.
//
// Proves the PK07-3 root-cutover surface an app uses to move its data root: `Suite::per_run_temp`
// and `Suite::app_root`, `RootCutover::{new, fs_move, participant, run, status, selected_root,
// recover, retire_old_root}`, `FsMove::Tree` with `MigrateOptions::strict`, a no-op
// `CutoverParticipant` that keeps the default hooks, a refused relative old root (`InvalidRoot`),
// the completed rerun, the completed recovery, the retirement of the old root, `valid_app_name`,
// and the record file name.
// Does not prove: `FsMove::Companions`, participant failure, abort or panic paths, path
// substitution, the db relocation participant, approvals (PK07-4), service registration (PK07-5),
// or any listener or process.
// State: a per-run private temp suite, removed on drop. No network, no processes, no real user data.
use rightkit_daemon::{
    valid_app_name, CutoverError, CutoverParticipant, CutoverPhase, FsMove, MigrateOptions,
    RecoverOutcome, RetireMode, RootCutover, RootSelection, Suite, CUTOVER_RECORD,
};
use std::error::Error;
use std::fs;

struct NoOp;

impl CutoverParticipant for NoOp {
    fn name(&self) -> &str {
        "noop"
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    assert!(valid_app_name("CutoverApp"));
    assert!(!valid_app_name("../escape"));
    assert_eq!(CUTOVER_RECORD, ".rightkit-root-cutover.json");

    let suite = Suite::per_run_temp("consumer")?;
    let app = suite.app_root("CutoverApp")?;
    let old = suite.root().join("old");
    fs::create_dir_all(old.join("sub"))?;
    fs::write(old.join("a.txt"), b"alpha")?;
    fs::write(old.join("sub").join("b.txt"), b"beta")?;
    assert!(RootCutover::status(&app)?.is_none(), "no record before the first cutover");

    // A relative old root is refused before anything is copied or switched.
    let mut refused = RootCutover::new(app.clone(), "relative-old");
    assert!(
        matches!(refused.run(), Err(CutoverError::InvalidRoot { .. })),
        "a relative old root must be refused with InvalidRoot"
    );

    let mut cutover = RootCutover::new(app.clone(), old.clone())
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .participant(NoOp);
    let report = cutover.run()?;
    assert_eq!(report.selected, RootSelection::New);
    assert_eq!(report.generation, 1);
    assert!(!report.resumed, "the first run is not a resume");
    assert!(!report.already_completed, "the first run is not a rerun");
    let migration = report.migration.as_ref().expect("a tree move reports its migration");
    assert!(migration.complete(), "the tree migration completed");
    assert_eq!(fs::read(app.root().join("a.txt"))?, b"alpha");
    assert_eq!(fs::read(app.root().join("sub").join("b.txt"))?, b"beta");
    assert_eq!(fs::read(old.join("a.txt"))?, b"alpha", "the old root stays complete");

    let status = RootCutover::status(&app)?.expect("a record exists after the cutover");
    assert_eq!(status.phase, CutoverPhase::Completed);
    assert_eq!(status.generation, 1);
    assert_eq!(status.selected, RootSelection::New);
    assert!(status.settlement_complete, "the no-op participant settled");
    assert_eq!(status.participants, vec!["noop".to_string()]);
    assert_eq!(RootCutover::selected_root(&app, &old)?, status.new_root);

    // A completed cutover reruns as a no-op, and recovery of a completed cutover is a no-op too.
    let again = cutover.run()?;
    assert!(again.already_completed, "a completed cutover reruns as a no-op");
    let mut recovering = RootCutover::new(app.clone(), old.clone()).participant(NoOp);
    assert!(
        matches!(recovering.recover()?, RecoverOutcome::Completed),
        "recovery of a completed cutover reports Completed"
    );

    // Retiring the old root renames it beside the original; a second call reports it as done.
    let retired = RootCutover::retire_old_root(&app, RetireMode::RenameAside)?;
    assert!(!retired.already, "the first retirement is not a repeat");
    assert!(!old.exists(), "the old root was renamed aside");
    assert_eq!(fs::read(retired.path.join("a.txt"))?, b"alpha");
    let repeat = RootCutover::retire_old_root(&app, RetireMode::RenameAside)?;
    assert!(repeat.already, "a second retirement reports the retired root");

    println!("OK rightkit-daemon 0.4.0 (registry consumer, PK07-3 root cutover)");
    Ok(())
}
