//! `FileTransaction` / `recover_transactions`: the commit protocol, an injected failure at every
//! boundary, real process kills at every boundary, recovery, identity checks, private modes.
//!
//! Layout of the fixture: `<tmp>/root` holds `a.txt` ("old-a") and an empty `sub/`; candidates
//! live in `<tmp>/stage`. The standard transaction replaces `a.txt` and creates `sub/b.txt`.
//! `.rktxn/retained/` is where a committed transaction's changed leftovers are set aside; it is not
//! a transaction directory and the helpers below skip it.

use rightkit_fs::atomic::seams as atomic_seams;
use rightkit_fs::transaction::seams::{self, Step};
use rightkit_fs::{
    recover_transactions, ExpectedPrior, FileKey, FileTransaction, FsError, PreparedTransaction,
    RecoveryOutcome, TransactionEntry, TransactionOptions, TransactionReport,
};
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    stage: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let stage = dir.path().join("stage");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir_all(&stage).unwrap();
    fs::write(root.join("a.txt"), "old-a").unwrap();
    fs::write(stage.join("a.txt"), "new-a").unwrap();
    fs::write(stage.join("b.txt"), "new-b").unwrap();
    Fixture {
        _dir: dir,
        root,
        stage,
    }
}

fn entries(f: &Fixture) -> Vec<TransactionEntry> {
    vec![
        TransactionEntry::new("a.txt", f.stage.join("a.txt")),
        TransactionEntry::create_new("sub/b.txt", f.stage.join("b.txt")),
    ]
}

fn prepared(f: &Fixture) -> PreparedTransaction {
    FileTransaction::prepare(&f.root, entries(f), TransactionOptions::default()).unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn assert_old(f: &Fixture) {
    assert_eq!(read(&f.root.join("a.txt")), "old-a");
    assert!(!f.root.join("sub").join("b.txt").exists(), "b.txt appeared");
}

fn assert_new(f: &Fixture) {
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    assert_eq!(read(&f.root.join("sub").join("b.txt")), "new-b");
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        out.push(path.clone());
        if entry.file_type().unwrap().is_dir() {
            walk(&path, out);
        }
    }
}

/// No staged copy or backup name anywhere under the root, and no transaction directory.
fn assert_no_residue(root: &Path) {
    let mut all = Vec::new();
    walk(root, &mut all);
    let set_aside = root.join(".rktxn").join("retained");
    for path in &all {
        if path.starts_with(&set_aside) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains(".rktxn-"), "left behind: {path:?}");
    }
    let area = root.join(".rktxn");
    if area.exists() {
        for entry in fs::read_dir(&area).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == "retained" {
                continue;
            }
            assert!(
                !entry.file_type().unwrap().is_dir(),
                "a transaction directory is still pending: {:?}",
                entry.path()
            );
        }
    }
}

fn txn_dirs(root: &Path) -> Vec<PathBuf> {
    let area = root.join(".rktxn");
    let mut dirs = Vec::new();
    if let Ok(listing) = fs::read_dir(&area) {
        for entry in listing {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() && entry.file_name() != "retained" {
                dirs.push(entry.path());
            }
        }
    }
    dirs
}

fn only_txn_dir(root: &Path) -> PathBuf {
    let mut dirs = txn_dirs(root);
    assert_eq!(
        dirs.len(),
        1,
        "expected one transaction directory: {dirs:?}"
    );
    dirs.remove(0)
}

/// The staged copy (`..."<n>s"`) or backup (`..."<n>b"`) file of an entry.
fn find_side(root: &Path, suffix: &str) -> PathBuf {
    let mut all = Vec::new();
    walk(root, &mut all);
    let set_aside = root.join(".rktxn").join("retained");
    all.into_iter()
        .find(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            !path.starts_with(&set_aside) && name.contains(".rktxn-") && name.ends_with(suffix)
        })
        .unwrap_or_else(|| panic!("no side file ending in {suffix}"))
}

fn all_steps() -> Vec<Step> {
    vec![
        Step::BeforeJournalStaging,
        Step::AfterJournalStaging,
        Step::BeforeStage(0),
        Step::BeforeStageClaim(0),
        Step::AfterStageClaim(0),
        Step::AfterStage(0),
        Step::BeforeStage(1),
        Step::BeforeStageClaim(1),
        Step::AfterStageClaim(1),
        Step::AfterStage(1),
        Step::BeforeJournalStaged,
        Step::AfterJournalStaged,
        Step::BeforeBackup(0),
        Step::BeforeLink(0),
        Step::AfterBackup(0),
        Step::BeforePublish(0),
        Step::AfterPublish(0),
        Step::BeforePublish(1),
        Step::AfterPublish(1),
        Step::BeforeCommitMarker,
        Step::AfterCommitMarker,
        Step::BeforeCleanup(0),
        Step::AfterCleanup(0),
        Step::SetAsideProbe,
        Step::SetAsideProbe,
        Step::BeforeJournalRemove,
        Step::AfterJournalRemove,
        Step::AfterMarkerRemove,
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// `commit` returns `Err`: no live destination was ever touched.
    NothingChanged,
    /// A report: published, then undone.
    RolledBack,
    /// The commit marker is durable: the new state is final.
    Committed,
}

fn expectation(step: &Step) -> Expect {
    match step {
        Step::BeforeJournalStaging
        | Step::AfterJournalStaging
        | Step::BeforeStage(_)
        | Step::BeforeStageClaim(_)
        | Step::AfterStageClaim(_)
        | Step::AfterStage(_)
        | Step::BeforeJournalStaged
        | Step::AfterJournalStaged => Expect::NothingChanged,
        Step::BeforeBackup(_)
        | Step::BeforeLink(_)
        | Step::AfterBackup(_)
        | Step::BeforePublish(_)
        | Step::AfterPublish(_)
        | Step::BeforeCommitMarker => Expect::RolledBack,
        _ => Expect::Committed,
    }
}

fn fail_at(steps: Vec<Step>) -> seams::StepGuard {
    seams::on_step(move |step| {
        steps
            .contains(step)
            .then(|| std::io::Error::other("injected"))
    })
}

fn is_injected(error: &FsError) -> bool {
    matches!(error, FsError::Io { source, .. } if source.to_string().contains("injected"))
}

/// Leaves a transaction awaiting recovery: entry 0 published (new content, backup kept), entry 1
/// not published, and the rollback itself stopped at entry 0.
fn leave_a_pending_transaction(f: &Fixture) {
    let guard = fail_at(vec![Step::BeforePublish(1), Step::BeforeRestore(0)]);
    let report = prepared(f).commit().unwrap();
    drop(guard);
    match report {
        TransactionReport::RecoveryRequired(stuck) => {
            assert!(!stuck.retained.is_empty());
            assert!(stuck.journal.exists());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
}

#[test]
fn two_files_commit_and_leave_no_residue() {
    let f = fixture();
    let done = match prepared(&f).commit().unwrap() {
        TransactionReport::Committed(done) => done,
        other => panic!("{other:?}"),
    };
    assert_eq!(done.published.len(), 2);
    assert!(done.retained.is_empty(), "{:?}", done.retained);
    assert_new(&f);
    assert_no_residue(&f.root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(f.root.join("a.txt")).unwrap().nlink(), 1);
    }
    // The candidates are copied, never consumed.
    assert_eq!(read(&f.stage.join("a.txt")), "new-a");
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
}

#[test]
fn the_steps_fire_in_protocol_order_and_the_journal_is_durable_before_the_first_live_change() {
    let f = fixture();
    let log: Rc<RefCell<Vec<Step>>> = Rc::new(RefCell::new(Vec::new()));
    let seen: Rc<RefCell<Vec<(&'static str, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let (sink, notes, root) = (Rc::clone(&log), Rc::clone(&seen), f.root.clone());
    atomic_seams::reset_sync_counts();
    let guard = seams::on_step(move |step| {
        sink.borrow_mut().push(step.clone());
        if *step == Step::BeforeBackup(0) {
            let counts = atomic_seams::sync_counts();
            let mut notes = notes.borrow_mut();
            notes.push((
                "the journal exists before the first live change",
                only_txn_dir(&root).join("journal").exists(),
            ));
            notes.push((
                "a.txt is still the old file",
                fs::read_to_string(root.join("a.txt")).unwrap() == "old-a",
            ));
            notes.push((
                "journal and staged copies were flushed",
                counts.file_syncs >= 4,
            ));
            #[cfg(unix)]
            notes.push(("their directories were synced", counts.dir_syncs >= 4));
        }
        if *step == Step::BeforeCommitMarker {
            let mut notes = notes.borrow_mut();
            notes.push((
                "every destination is published before the marker",
                fs::read_to_string(root.join("a.txt")).unwrap() == "new-a"
                    && root.join("sub").join("b.txt").exists(),
            ));
            notes.push((
                "there is no marker yet",
                !only_txn_dir(&root).join("commit").exists(),
            ));
        }
        if *step == Step::AfterCommitMarker {
            notes.borrow_mut().push((
                "the marker exists once committed",
                only_txn_dir(&root).join("commit").exists(),
            ));
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    assert_eq!(*log.borrow(), all_steps());
    for (what, held) in seen.borrow().iter() {
        assert!(*held, "{what}");
    }
    assert!(!seen.borrow().is_empty());
}

#[test]
fn prepare_changes_nothing_on_disk() {
    let f = fixture();
    let prepared = prepared(&f);
    assert!(
        !f.root.join(".rktxn").exists(),
        "preparation created the journal area"
    );
    assert_old(&f);
    assert_eq!(prepared.destinations().len(), 2);
    drop(prepared);
    assert!(!f.root.join(".rktxn").exists());
}

fn prepare_err(f: &Fixture, entries: Vec<TransactionEntry>) -> FsError {
    FileTransaction::prepare(&f.root, entries, TransactionOptions::default()).unwrap_err()
}

#[test]
fn prepare_refuses_escapes_collisions_and_stale_expectations() {
    let f = fixture();
    let cand = f.stage.join("a.txt");
    let bad_name = |err: FsError| assert!(matches!(err, FsError::BadName(_)), "{err:?}");

    bad_name(prepare_err(
        &f,
        vec![TransactionEntry::new("../escape.txt", &cand)],
    ));
    bad_name(prepare_err(
        &f,
        vec![TransactionEntry::new("sub/../../escape.txt", &cand)],
    ));
    bad_name(prepare_err(
        &f,
        vec![TransactionEntry::new(f.root.join("a.txt"), &cand)],
    ));
    bad_name(prepare_err(
        &f,
        vec![TransactionEntry::new(".rktxn/x", &cand)],
    ));
    bad_name(prepare_err(&f, vec![TransactionEntry::new("", &cand)]));
    bad_name(prepare_err(
        &f,
        vec![
            TransactionEntry::new("a.txt", &cand),
            TransactionEntry::new("./a.txt", &cand),
        ],
    ));
    bad_name(prepare_err(
        &f,
        vec![
            TransactionEntry::new("sub/x", &cand),
            TransactionEntry::new("sub", &cand),
        ],
    ));

    let exists = prepare_err(&f, vec![TransactionEntry::create_new("a.txt", &cand)]);
    assert!(matches!(exists, FsError::AlreadyExists(_)), "{exists:?}");
    let expected_absent = prepare_err(
        &f,
        vec![TransactionEntry::new("a.txt", &cand).expect(ExpectedPrior::Absent)],
    );
    assert!(
        matches!(expected_absent, FsError::Conflict { .. }),
        "{expected_absent:?}"
    );
    let other_key = FileKey::of_path(&f.stage.join("b.txt")).unwrap();
    let wrong_identity = prepare_err(
        &f,
        vec![TransactionEntry::new("a.txt", &cand).expect(ExpectedPrior::Identity(other_key))],
    );
    assert!(
        matches!(wrong_identity, FsError::Conflict { .. }),
        "{wrong_identity:?}"
    );
    let create_with_identity = prepare_err(
        &f,
        vec![TransactionEntry::create_new("sub/new.txt", &cand)
            .expect(ExpectedPrior::Identity(other_key))],
    );
    assert_eq!(
        create_with_identity.kind(),
        Some(std::io::ErrorKind::InvalidInput),
        "{create_with_identity:?}"
    );

    let missing_candidate = prepare_err(
        &f,
        vec![TransactionEntry::new("a.txt", f.stage.join("missing"))],
    );
    assert!(missing_candidate.is_not_found(), "{missing_candidate:?}");
    let directory_candidate = prepare_err(&f, vec![TransactionEntry::new("a.txt", &f.stage)]);
    assert!(
        matches!(directory_candidate, FsError::NotAFile(_)),
        "{directory_candidate:?}"
    );
    let directory_destination = prepare_err(&f, vec![TransactionEntry::new("sub", &cand)]);
    assert!(
        matches!(directory_destination, FsError::NotAFile(_)),
        "{directory_destination:?}"
    );

    let none = prepare_err(&f, Vec::new());
    assert_eq!(none.kind(), Some(std::io::ErrorKind::InvalidInput));
    let many: Vec<TransactionEntry> = (0..=256)
        .map(|n| TransactionEntry::new(format!("f{n}.txt"), &cand))
        .collect();
    assert!(matches!(
        prepare_err(&f, many),
        FsError::TooManyEntries(256)
    ));

    assert_old(&f);
    assert!(!f.root.join(".rktxn").exists());
}

#[cfg(unix)]
#[test]
fn prepare_refuses_a_symlinked_parent_and_a_symlinked_destination() {
    let f = fixture();
    std::os::unix::fs::symlink(f.root.join("sub"), f.root.join("link")).unwrap();
    let parent = prepare_err(
        &f,
        vec![TransactionEntry::new("link/x.txt", f.stage.join("a.txt"))],
    );
    assert!(matches!(parent, FsError::Symlink(_)), "{parent:?}");
    std::os::unix::fs::symlink(f.root.join("a.txt"), f.root.join("alias.txt")).unwrap();
    let leaf = prepare_err(
        &f,
        vec![TransactionEntry::new("alias.txt", f.stage.join("a.txt"))],
    );
    assert!(matches!(leaf, FsError::Symlink(_)), "{leaf:?}");
}

#[test]
fn a_destination_replaced_after_prepare_is_a_conflict_and_is_never_overwritten() {
    let f = fixture();
    let key = FileKey::of_path(&f.root.join("a.txt")).unwrap();
    let prepared = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new("a.txt", f.stage.join("a.txt"))
            .expect(ExpectedPrior::Identity(key))],
        TransactionOptions::default(),
    )
    .unwrap();
    // An external writer replaces the file between prepare and commit.
    // (Written beside it and renamed over, so the new file cannot reuse the old inode number.)
    fs::write(f.root.join("intruder.tmp"), "intruder").unwrap();
    fs::rename(f.root.join("intruder.tmp"), f.root.join("a.txt")).unwrap();
    let error = prepared.commit().unwrap_err();
    assert!(matches!(error, FsError::Conflict { .. }), "{error:?}");
    assert_eq!(read(&f.root.join("a.txt")), "intruder");
    assert_no_residue(&f.root);
}

#[test]
fn a_candidate_changed_after_prepare_is_a_conflict() {
    let f = fixture();
    let prepared = prepared(&f);
    fs::write(f.stage.join("a.txt"), "something else entirely").unwrap();
    let error = prepared.commit().unwrap_err();
    assert!(matches!(error, FsError::Conflict { .. }), "{error:?}");
    assert_old(&f);
    assert_no_residue(&f.root);
}

#[test]
fn an_error_at_every_boundary_ends_old_or_new_never_in_between() {
    errors_at_every_boundary(false);
}

/// The same loop where the prior file is kept by exclusive rename aside (a volume without hard
/// links): the destination is absent between the two renames.
#[test]
fn an_error_at_every_boundary_ends_old_or_new_with_the_aside_backup() {
    errors_at_every_boundary(true);
}

fn errors_at_every_boundary(aside: bool) {
    for step in all_steps() {
        let f = fixture();
        let prepared = prepared(&f);
        let _aside = aside.then(seams::force_aside_backup);
        let guard = fail_at(vec![step.clone()]);
        let result = prepared.commit();
        drop(guard);
        match expectation(&step) {
            Expect::NothingChanged => {
                let error = result.unwrap_err();
                assert!(is_injected(&error), "{step:?}: {error:?}");
                assert_old(&f);
                assert_no_residue(&f.root);
                assert!(recover_transactions(&f.root)
                    .unwrap()
                    .transactions
                    .is_empty());
            }
            Expect::RolledBack => {
                match result.unwrap() {
                    TransactionReport::RolledBack(undone) => {
                        assert!(is_injected(&undone.cause), "{step:?}: {:?}", undone.cause);
                    }
                    other => panic!("{step:?}: {other:?}"),
                }
                assert_old(&f);
                assert_no_residue(&f.root);
                assert!(recover_transactions(&f.root)
                    .unwrap()
                    .transactions
                    .is_empty());
            }
            Expect::Committed => {
                match result.unwrap() {
                    TransactionReport::Committed(done) => {
                        assert!(
                            !done.retained.is_empty(),
                            "{step:?}: cleanup cannot have finished"
                        );
                    }
                    other => panic!("{step:?}: {other:?}"),
                }
                assert_new(&f);
                let report = recover_transactions(&f.root).unwrap();
                assert!(report.is_clean(), "{step:?}: {report:?}");
                assert_eq!(report.transactions.len(), 1, "{step:?}");
                // With the marker already gone only an empty directory is left to discard.
                let finished = if step == Step::AfterMarkerRemove {
                    RecoveryOutcome::Discarded
                } else {
                    RecoveryOutcome::Completed
                };
                assert_eq!(report.transactions[0].outcome, finished, "{step:?}");
                assert_new(&f);
                assert_no_residue(&f.root);
            }
        }
    }
}

#[test]
fn a_failed_rollback_needs_recovery_and_blocks_the_next_commit_until_it_runs() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    // A second transaction is refused while one awaits recovery.
    let refused = prepared(&f).commit().unwrap_err();
    assert!(matches!(refused, FsError::Locked { .. }), "{refused:?}");

    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.transactions.len(), 1);
    assert_eq!(report.transactions[0].outcome, RecoveryOutcome::RolledBack);
    assert_old(&f);
    assert_no_residue(&f.root);
    // Recovery is idempotent, and the root takes a transaction again.
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
    assert!(prepared(&f).commit().unwrap().is_committed());
    assert_new(&f);
}

#[test]
fn a_commit_marker_that_cannot_be_confirmed_leaves_the_decision_to_recovery() {
    let f = fixture();
    let prepared = prepared(&f);
    let slot: Rc<RefCell<Option<atomic_seams::SeamGuard>>> = Rc::new(RefCell::new(None));
    let hold = Rc::clone(&slot);
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeCommitMarker {
            *hold.borrow_mut() = Some(atomic_seams::fail_write_after(
                0,
                std::io::ErrorKind::StorageFull,
            ));
        }
        None
    });
    let report = prepared.commit().unwrap();
    drop(guard);
    let _ = slot.borrow_mut().take();
    match report {
        TransactionReport::RecoveryRequired(stuck) => {
            assert_eq!(stuck.cause.kind(), Some(std::io::ErrorKind::StorageFull));
            assert!(stuck.journal.exists());
        }
        other => panic!("{other:?}"),
    }
    // Everything is published but undecided; no marker exists, so recovery rolls back.
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    let report = recover_transactions(&f.root).unwrap();
    assert_eq!(
        report.transactions[0].outcome,
        RecoveryOutcome::RolledBack,
        "{report:?}"
    );
    assert_old(&f);
    assert_no_residue(&f.root);
}

#[test]
fn write_failures_before_publication_change_nothing() {
    // The very first write is the journal.
    let f = fixture();
    let guard = atomic_seams::fail_write_after(0, std::io::ErrorKind::StorageFull);
    let error = prepared(&f).commit().unwrap_err();
    drop(guard);
    assert_eq!(
        error.kind(),
        Some(std::io::ErrorKind::StorageFull),
        "{error:?}"
    );
    assert_old(&f);
    assert_no_residue(&f.root);

    // The staged copy runs out of space after the journal was written.
    let f = fixture();
    let slot: Rc<RefCell<Option<atomic_seams::SeamGuard>>> = Rc::new(RefCell::new(None));
    let hold = Rc::clone(&slot);
    let guard = seams::on_step(move |step| {
        if *step == Step::AfterJournalStaging {
            *hold.borrow_mut() = Some(atomic_seams::fail_write_after(
                0,
                std::io::ErrorKind::StorageFull,
            ));
        }
        None
    });
    let error = prepared(&f).commit().unwrap_err();
    drop(guard);
    let _ = slot.borrow_mut().take();
    assert_eq!(
        error.kind(),
        Some(std::io::ErrorKind::StorageFull),
        "{error:?}"
    );
    assert_old(&f);
    assert_no_residue(&f.root);
    assert_eq!(
        read(&f.stage.join("a.txt")),
        "new-a",
        "the candidate is never consumed"
    );
}

#[test]
fn a_file_that_appears_at_a_create_new_destination_during_commit_is_never_overwritten() {
    let f = fixture();
    let prepared = prepared(&f);
    let root = f.root.clone();
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforePublish(1) {
            fs::write(root.join("sub").join("b.txt"), "intruder").unwrap();
        }
        None
    });
    let report = prepared.commit().unwrap();
    drop(guard);
    match report {
        TransactionReport::RolledBack(undone) => {
            assert!(
                matches!(undone.cause, FsError::AlreadyExists(_)),
                "{:?}",
                undone.cause
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&f.root.join("sub").join("b.txt")), "intruder");
    assert_eq!(read(&f.root.join("a.txt")), "old-a");
    assert_no_residue(&f.root);
}

#[test]
fn a_published_file_edited_by_someone_else_is_retained_not_overwritten() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    // Someone edits the published file in place while the transaction awaits recovery.
    fs::write(f.root.join("a.txt"), "edited by the user").unwrap();
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::Retained { items, .. } => assert!(!items.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&f.root.join("a.txt")), "edited by the user");
    // The old content is still there, in the retained backup.
    assert_eq!(read(&find_side(&f.root, "0b")), "old-a");
    // Recovery stays refused and a new commit stays blocked until a person decides.
    assert!(!recover_transactions(&f.root).unwrap().is_clean());
    let refused = prepared(&f).commit().unwrap_err();
    assert!(matches!(refused, FsError::Locked { .. }), "{refused:?}");
}

#[test]
fn a_damaged_journal_is_retained_and_nothing_is_touched() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    let journal = only_txn_dir(&f.root).join("journal");
    fs::write(&journal, "garbage").unwrap();
    let report = recover_transactions(&f.root).unwrap();
    match &report.transactions[0].outcome {
        RecoveryOutcome::Retained { items, .. } => assert!(!items.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    assert_eq!(read(&find_side(&f.root, "0b")), "old-a");
    assert!(journal.exists());
}

#[test]
fn unknown_material_in_a_transaction_directory_is_retained_never_deleted() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    let stray = only_txn_dir(&f.root).join("stray.txt");
    fs::write(&stray, "not ours").unwrap();
    let report = recover_transactions(&f.root).unwrap();
    // The files were restored, but the directory holds something recovery will not delete.
    assert_old(&f);
    match &report.transactions[0].outcome {
        RecoveryOutcome::Retained { items, .. } => {
            assert!(
                items
                    .iter()
                    .any(|item| item.path.file_name() == stray.file_name()),
                "{items:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&stray), "not ours");
    fs::remove_file(&stray).unwrap();
    let report = recover_transactions(&f.root).unwrap();
    assert_eq!(
        report.transactions[0].outcome,
        RecoveryOutcome::RolledBack,
        "{report:?}"
    );
    assert_no_residue(&f.root);
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
}

#[test]
fn a_staged_copy_modified_before_publication_is_never_published_and_is_retained() {
    let f = fixture();
    let prepared = prepared(&f);
    let root = f.root.clone();
    let guard = seams::on_step(move |step| {
        if *step == Step::AfterJournalStaged {
            use std::io::Write;
            let staged = find_side(&root, "0s");
            let mut file = fs::OpenOptions::new().append(true).open(staged).unwrap();
            file.write_all(b"tampered").unwrap();
        }
        None
    });
    let report = prepared.commit().unwrap();
    drop(guard);
    match report {
        TransactionReport::RecoveryRequired(stuck) => {
            assert!(
                matches!(stuck.cause, FsError::Conflict { .. }),
                "{:?}",
                stuck.cause
            );
        }
        other => panic!("{other:?}"),
    }
    // The destination was never replaced with the tampered copy.
    assert_eq!(read(&f.root.join("a.txt")), "old-a");
    assert!(!recover_transactions(&f.root).unwrap().is_clean());
}

#[test]
fn private_entries_stay_private_and_a_prior_file_keeps_its_mode() {
    let f = fixture();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(f.root.join("a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
        fs::set_permissions(f.stage.join("b.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    }
    let entries = vec![
        TransactionEntry::new("a.txt", f.stage.join("a.txt")),
        TransactionEntry::create_new("sub/b.txt", f.stage.join("b.txt")).private(),
    ];
    let prepared =
        FileTransaction::prepare(&f.root, entries, TransactionOptions::default()).unwrap();
    let staged_private: Rc<RefCell<Option<bool>>> = Rc::new(RefCell::new(None));
    let (note, root) = (Rc::clone(&staged_private), f.root.clone());
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforePublish(1) {
            let staged = find_side(&root, "1s");
            *note.borrow_mut() = Some(rightkit_fs::private::is_owner_only_strict(&staged).unwrap());
        }
        None
    });
    let report = prepared.commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    assert_eq!(
        *staged_private.borrow(),
        Some(true),
        "the staged copy of a private entry was not owner-only before publication"
    );
    assert!(rightkit_fs::private::is_owner_only_strict(&f.root.join("sub").join("b.txt")).unwrap());
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&f.root.join("a.txt")),
            0o640,
            "the prior mode was not kept"
        );
        assert_eq!(mode(&f.root.join("sub").join("b.txt")), 0o600);
        assert_eq!(mode(&f.root.join(".rktxn")), 0o700);
    }
    assert_no_residue(&f.root);
}

#[test]
fn missing_parent_directories_are_created_and_stay_after_a_rollback() {
    let f = fixture();
    let entries = vec![TransactionEntry::create_new(
        "deep/er/c.txt",
        f.stage.join("a.txt"),
    )];
    let prepared =
        FileTransaction::prepare(&f.root, entries, TransactionOptions::default()).unwrap();
    let guard = fail_at(vec![Step::BeforeCommitMarker]);
    let report = prepared.commit().unwrap();
    drop(guard);
    assert!(
        matches!(report, TransactionReport::RolledBack(_)),
        "{report:?}"
    );
    assert!(!f.root.join("deep").join("er").join("c.txt").exists());
    assert!(
        f.root.join("deep").join("er").is_dir(),
        "created parents are not removed"
    );
    assert_no_residue(&f.root);

    let entries = vec![TransactionEntry::create_new(
        "deep/er/c.txt",
        f.stage.join("a.txt"),
    )];
    let done = FileTransaction::prepare(&f.root, entries, TransactionOptions::default())
        .unwrap()
        .commit()
        .unwrap();
    assert!(done.is_committed(), "{done:?}");
    assert_eq!(read(&f.root.join("deep").join("er").join("c.txt")), "new-a");
}

#[test]
fn concurrent_commits_to_one_root_are_serialized() {
    let f = fixture();
    let mut handles = Vec::new();
    for thread in 0..4 {
        let (root, stage) = (f.root.clone(), f.stage.clone());
        handles.push(std::thread::spawn(move || {
            for round in 0..4 {
                let candidate = stage.join(format!("t{thread}-{round}.txt"));
                fs::write(&candidate, format!("t{thread}r{round}")).unwrap();
                let report = FileTransaction::prepare(
                    &root,
                    vec![TransactionEntry::new(format!("t{thread}.txt"), &candidate)],
                    TransactionOptions::default(),
                )
                .unwrap()
                .commit()
                .unwrap();
                assert!(report.is_committed(), "{report:?}");
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    for thread in 0..4 {
        assert_eq!(
            read(&f.root.join(format!("t{thread}.txt"))),
            format!("t{thread}r3")
        );
    }
    assert_no_residue(&f.root);
}

#[test]
fn recovering_a_root_without_transactions_creates_nothing() {
    let f = fixture();
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.transactions.is_empty());
    assert!(report.is_clean());
    assert!(!f.root.join(".rktxn").exists());
}

#[test]
fn a_busy_root_times_out_instead_of_waiting_forever() {
    let f = fixture();
    let first = prepared(&f);
    let second = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new(
            "sub/other.txt",
            f.stage.join("a.txt"),
        )],
        TransactionOptions::default().lock_timeout(Duration::from_millis(100)),
    )
    .unwrap();
    // While the first commit sits at a step, a second commit on another thread cannot get the lock.
    let (tx, rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let guard = seams::on_step(move |step| {
            if *step == Step::AfterJournalStaged {
                tx.send(()).unwrap();
                go_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            }
            None
        });
        let report = first.commit().unwrap();
        drop(guard);
        report.is_committed()
    });
    rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let error = second.commit().unwrap_err();
    assert!(
        matches!(error, FsError::LockTimeout { .. } | FsError::Locked { .. }),
        "{error:?}"
    );
    go_tx.send(()).unwrap();
    assert!(handle.join().unwrap());
    assert_new(&f);
}

#[cfg(feature = "digest")]
#[test]
fn a_content_hash_expectation_is_checked_under_the_lock() {
    let f = fixture();
    let good = rightkit_fs::digest::sha256_bytes(b"old-a");
    let done = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new("a.txt", f.stage.join("a.txt"))
            .expect(ExpectedPrior::Sha256(good))],
        TransactionOptions::default(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert!(done.is_committed(), "{done:?}");

    let f = fixture();
    let wrong = rightkit_fs::digest::sha256_bytes(b"something else");
    let error = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new("a.txt", f.stage.join("a.txt"))
            .expect(ExpectedPrior::Sha256(wrong))],
        TransactionOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, FsError::Conflict { .. }), "{error:?}");
    assert_old(&f);
}

// ---- native Windows contention: a reader that denies delete sharing ----

#[cfg(windows)]
fn reader_without_delete_sharing(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE, no FILE_SHARE_DELETE: the file cannot be replaced or
    // deleted while this handle is open (the replace fails with ERROR_ACCESS_DENIED).
    fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(path)
        .unwrap()
}

#[cfg(windows)]
#[test]
fn a_bounded_access_denied_retry_publishes_once_a_reader_lets_go() {
    use rightkit_fs::{AccessDeniedRetry, RetryPolicy};
    let f = fixture();
    let reader = reader_without_delete_sharing(&f.root.join("a.txt"));
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(reader);
    });
    let options = TransactionOptions::default().access_denied_retry(AccessDeniedRetry::new(
        RetryPolicy::new(80, Duration::from_millis(50)),
    ));
    let report = FileTransaction::prepare(&f.root, entries(&f), options)
        .unwrap()
        .commit()
        .unwrap();
    release.join().unwrap();
    assert!(report.is_committed(), "{report:?}");
    assert_new(&f);
    let recovered = recover_transactions(&f.root).unwrap();
    assert!(recovered.is_clean(), "{recovered:?}");
    assert_no_residue(&f.root);
}

#[cfg(windows)]
#[test]
fn a_reader_that_never_lets_go_blocks_publication_and_recovery_finishes_after_it_does() {
    use rightkit_fs::{AccessDeniedRetry, RetryPolicy};
    let f = fixture();
    let reader = reader_without_delete_sharing(&f.root.join("a.txt"));
    let options = TransactionOptions::default().access_denied_retry(AccessDeniedRetry::new(
        RetryPolicy::new(2, Duration::from_millis(20)),
    ));
    let report = FileTransaction::prepare(&f.root, entries(&f), options)
        .unwrap()
        .commit()
        .unwrap();
    // The replace never happened, whatever the rollback could or could not remove meanwhile.
    assert!(!report.is_committed(), "{report:?}");
    assert_eq!(read(&f.root.join("a.txt")), "old-a");
    drop(reader);
    let recovered = recover_transactions(&f.root).unwrap();
    assert!(recovered.is_clean(), "{recovered:?}");
    assert_old(&f);
    assert_no_residue(&f.root);
}

// ---- real process kills at every boundary ----

const KILL_ENV: &str = "RIGHTKIT_FS_TXN_KILL_HELPER";
/// Set (to anything) to make the helper keep the prior file by rename aside.
const KILL_ASIDE_ENV: &str = "RIGHTKIT_FS_TXN_KILL_ASIDE";
const KILL_MODE_ENV: &str = "RIGHTKIT_FS_TXN_KILL_MODE";

/// Child body for the kill test: an ordinary passing test unless `KILL_ENV` is set. With it, the
/// child runs the real commit and blocks at the named step (writing the ready file) until the
/// parent kills it. The pattern is the one in the lease tests.
#[test]
fn transaction_kill_helper_child() {
    let Ok(spec) = std::env::var(KILL_ENV) else {
        return;
    };
    let mut parts = spec.splitn(3, '|');
    let step_name = parts.next().unwrap().to_owned();
    let root = PathBuf::from(parts.next().unwrap());
    let ready = PathBuf::from(parts.next().unwrap());
    let stage = root.parent().unwrap().join("stage");
    let entries = vec![
        TransactionEntry::new("a.txt", stage.join("a.txt")),
        TransactionEntry::create_new("sub/b.txt", stage.join("b.txt")),
    ];
    let mode = std::env::var(KILL_MODE_ENV).unwrap_or_default();
    let prepared = if mode == "recover" {
        None
    } else {
        Some(FileTransaction::prepare(&root, entries, TransactionOptions::default()).unwrap())
    };
    let rollback = mode == "rollback";
    let _aside = std::env::var_os(KILL_ASIDE_ENV).map(|_| seams::force_aside_backup());
    let _guard = seams::on_step(move |step| {
        if format!("{step:?}") == step_name {
            fs::write(&ready, b"ready").unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        (rollback && *step == Step::BeforePublish(1))
            .then(|| std::io::Error::other("start rollback"))
    });
    // Reached only when the named step never fired; the parent sees the child exit.
    if let Some(prepared) = prepared {
        let _ = prepared.commit();
    } else {
        let _ = recover_transactions(&root);
    }
}

/// Runs the helper child at `step`, waits for it to reach the boundary, kills and reaps it.
fn kill_at(f: &Fixture, step: &Step, aside: bool) {
    kill_in_mode(f, step, aside, "commit");
}

fn kill_in_mode(f: &Fixture, step: &Step, aside: bool, mode: &str) {
    use std::process::{Command, Stdio};
    use std::time::Instant;
    let name = format!("{step:?}");
    let ready = f.root.parent().unwrap().join("ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "transaction_kill_helper_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            KILL_ENV,
            format!("{name}|{}|{}", f.root.display(), ready.display()),
        )
        .env(KILL_MODE_ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if aside {
        command.env(KILL_ASIDE_ENV, "1");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready.exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the helper never reached {name}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn killing_the_process_at_every_boundary_recovers_to_old_or_new() {
    kills_at_every_boundary(false);
}

#[test]
fn killing_the_process_at_every_boundary_recovers_with_the_aside_backup() {
    kills_at_every_boundary(true);
}

fn kills_at_every_boundary(aside: bool) {
    // A kill between the writer's temporary file and its rename leaves that temp name behind (an
    // ordinary `AtomicWriter` crash orphan); `killing_inside_the_claim_window...` covers it.
    for step in all_steps()
        .into_iter()
        .filter(|step| !matches!(step, Step::BeforeStageClaim(_) | Step::AfterStageClaim(_)))
    {
        let f = fixture();
        kill_at(&f, &step, aside);
        let report = recover_transactions(&f.root)
            .unwrap_or_else(|error| panic!("{step:?}: recovery failed: {error}"));
        assert!(report.is_clean(), "{step:?}: {report:?}");
        match expectation(&step) {
            Expect::Committed => assert_new(&f),
            Expect::NothingChanged | Expect::RolledBack => assert_old(&f),
        }
        assert_no_residue(&f.root);
        assert!(
            recover_transactions(&f.root)
                .unwrap()
                .transactions
                .is_empty(),
            "{step:?}: recovery was not idempotent"
        );
    }
}

// ---- pre-review fixes: device numbers, root identity, directory sync order, leftovers ----

/// Rewrite the journal of the only pending transaction line by line and seal it with a fresh CRC,
/// as a journal written by another boot or another root would look.
fn rewrite_journal(txn_dir: &Path, edit: impl Fn(&str) -> String) {
    let path = txn_dir.join("journal");
    let text = fs::read_to_string(&path).unwrap();
    let split = text.rfind("crc ").unwrap();
    let body: String = text[..split]
        .lines()
        .map(|line| format!("{}\n", edit(line)))
        .collect();
    let crc = rightkit_fs::crc32(body.as_bytes());
    fs::write(&path, format!("{body}crc {crc:08x}\n")).unwrap();
}

/// Every recorded device number replaced, as after a reboot that renumbered the volume.
fn renumber_devices(line: &str) -> String {
    renumber(line, true)
}

/// Only the files' recorded device numbers replaced, the root's left alone: a file that is on
/// another device than the root's, or one the journal recorded wrongly.
fn renumber_entries_only(line: &str) -> String {
    renumber(line, false)
}

fn renumber(line: &str, root_too: bool) -> String {
    let swap = |ident: &str| -> String {
        match ident.split_once('.') {
            Some((_, rest)) => format!("deadbeef.{rest}"),
            None => ident.to_string(),
        }
    };
    if let Some(root) = line.strip_prefix("root ") {
        return if root_too {
            format!("root {}", swap(root))
        } else {
            line.to_string()
        };
    }
    if line.starts_with("entry ") {
        let mut tokens: Vec<String> = line.split(' ').map(str::to_string).collect();
        // "entry <n> <mode> <private> <dest> <stage> <backup> <prior> <staged>"
        tokens[7] = swap(&tokens[7]);
        tokens[8] = swap(&tokens[8]);
        return tokens.join(" ");
    }
    line.to_string()
}

/// The journal says the root was on another device than it is now (a reboot may have renumbered
/// it, or this may be another filesystem). That is unproven, so on every platform the transaction
/// is retained as it was: no rollback, no deletion, the journal and the rollback material stay,
/// and a new commit is refused until a person resolves it.
#[test]
fn a_root_on_another_device_than_recorded_is_retained_on_every_platform() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    rewrite_journal(&only_txn_dir(&f.root), renumber_devices);
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::Retained { reason, items } => {
            assert!(reason.contains("different device"), "{reason}");
            assert!(!items.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // Nothing was restored, completed, removed or replaced.
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    assert_eq!(read(&find_side(&f.root, "0b")), "old-a");
    assert!(only_txn_dir(&f.root).join("journal").exists());
    let refused = prepared(&f).commit().unwrap_err();
    assert!(matches!(refused, FsError::Locked { .. }), "{refused:?}");
    // Asking again changes nothing.
    assert!(!recover_transactions(&f.root).unwrap().is_clean());
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
}

/// The same for a committed transaction: the marker alone does not release a journal whose root
/// device cannot be accounted for, and no leftover is removed or set aside.
#[test]
fn a_committed_transaction_on_an_unproven_device_is_not_completed() {
    let f = fixture();
    let guard = fail_at(vec![Step::BeforeCleanup(0)]);
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    let dir = only_txn_dir(&f.root);
    rewrite_journal(&dir, renumber_devices);
    // The marker is bound to the journal's CRC: rebind it to the rewritten journal.
    let text = fs::read_to_string(dir.join("journal")).unwrap();
    let crc = text.rsplit("crc ").next().unwrap().trim().to_string();
    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
    fs::write(
        dir.join("commit"),
        format!(
            "rktxn-commit 1 {id} {crc}
"
        ),
    )
    .unwrap();
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    assert_new(&f);
    assert_eq!(
        read(&find_side(&f.root, "0b")),
        "old-a",
        "the backup was kept"
    );
    assert!(dir.join("journal").exists());
    assert!(dir.join("commit").exists());
}

#[test]
fn a_file_recorded_on_another_device_than_the_root_is_retained_not_guessed() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    rewrite_journal(&only_txn_dir(&f.root), renumber_entries_only);
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    // Nothing was restored, removed or replaced.
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    assert_eq!(read(&find_side(&f.root, "0b")), "old-a");
    assert!(only_txn_dir(&f.root).join("journal").exists());
}

#[test]
fn a_journal_written_for_another_root_directory_is_retained_and_nothing_is_touched() {
    let f = fixture();
    leave_a_pending_transaction(&f);
    rewrite_journal(&only_txn_dir(&f.root), |line| {
        match line
            .strip_prefix("root ")
            .and_then(|rest| rest.split_once('.'))
        {
            Some((volume, index)) => {
                let index = u64::from_str_radix(index, 16).unwrap() ^ 0x5a5a_5a5a;
                format!("root {volume}.{index:x}")
            }
            None => line.to_string(),
        }
    });
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::Retained { reason, items } => {
            assert!(reason.contains("different root"), "{reason}");
            assert!(!items.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // Nothing was restored, removed or replaced.
    assert_eq!(read(&f.root.join("a.txt")), "new-a");
    assert_eq!(read(&find_side(&f.root, "0b")), "old-a");
    assert!(only_txn_dir(&f.root).join("journal").exists());
}

/// A step, whether the journal and the marker exist, and the directory syncs counted so far.
#[cfg(unix)]
type Seen = (Step, bool, bool, u64);

#[cfg(unix)]
#[test]
fn the_journal_unlink_is_flushed_before_the_commit_marker_is_touched() {
    let f = fixture();
    let log: Rc<RefCell<Vec<Seen>>> = Rc::new(RefCell::new(Vec::new()));
    let (sink, root) = (Rc::clone(&log), f.root.clone());
    atomic_seams::reset_sync_counts();
    let guard = seams::on_step(move |step| {
        if matches!(
            step,
            Step::BeforeJournalRemove | Step::AfterJournalRemove | Step::AfterMarkerRemove
        ) {
            let dir = only_txn_dir(&root);
            sink.borrow_mut().push((
                step.clone(),
                dir.join("journal").exists(),
                dir.join("commit").exists(),
                atomic_seams::sync_counts().dir_syncs,
            ));
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    let log = log.borrow();
    assert_eq!(log.len(), 3, "{log:?}");
    let (before, journal_gone, marker_gone) = (&log[0], &log[1], &log[2]);
    assert!(before.1 && before.2, "journal and marker exist at first");
    assert!(
        !journal_gone.1 && journal_gone.2,
        "the journal is gone and the marker is not"
    );
    assert!(
        journal_gone.3 > before.3,
        "the transaction directory was not synced after the journal unlink"
    );
    assert!(!marker_gone.1 && !marker_gone.2, "both records are gone");
    assert!(
        marker_gone.3 > journal_gone.3,
        "the transaction directory was not synced after the marker unlink"
    );
    assert_no_residue(&f.root);
}

fn edit_in_place(path: &Path, extra: &[u8]) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(extra).unwrap();
}

#[test]
fn a_committed_leftover_that_changed_is_set_aside_and_blocks_nothing() {
    let f = fixture();
    let root = f.root.clone();
    // An editor writes through the hard link that is the backup, after the commit is decided.
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeCleanup(0) {
            edit_in_place(&find_side(&root, "0b"), b"-edited");
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    let done = match report {
        TransactionReport::Committed(done) => done,
        other => panic!("{other:?}"),
    };
    assert_new(&f);
    assert_eq!(done.retained.len(), 1, "{:?}", done.retained);
    let moved = done.retained[0].path.clone();
    assert!(
        moved.starts_with(
            rightkit_fs::canonicalize_plain(&f.root)
                .unwrap()
                .join(".rktxn")
                .join("retained")
        ),
        "{moved:?}"
    );
    assert_eq!(read(&moved), "old-a-edited", "the edited file was kept");
    assert_no_residue(&f.root);
    // Nothing is pending, and the root takes the next transaction at once.
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
    let next = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new("a.txt", f.stage.join("b.txt"))],
        TransactionOptions::default(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert!(next.is_committed(), "{next:?}");
    assert_eq!(read(&f.root.join("a.txt")), "new-b");
    assert_eq!(read(&moved), "old-a-edited");
}

#[test]
fn recovery_sets_aside_a_changed_backup_of_a_committed_transaction() {
    let f = fixture();
    let guard = fail_at(vec![Step::BeforeCleanup(0)]);
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    edit_in_place(&find_side(&f.root, "0b"), b"-edited");
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::CompletedSetAside { items } => {
            assert_eq!(items.len(), 1, "{items:?}");
            assert_eq!(read(&items[0].path), "old-a-edited");
        }
        other => panic!("{other:?}"),
    }
    assert_new(&f);
    assert_no_residue(&f.root);
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
    let next = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new("a.txt", f.stage.join("b.txt"))],
        TransactionOptions::default(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert!(next.is_committed(), "{next:?}");
}

/// An editor writes the destination between the commit's check and its backup. The edit is
/// never replaced, no second name of it is left, and the commit rolls back: the staged copy is
/// removed, the root is free again and the next commit works.
fn destination_edited_before_the_backup(aside: bool) {
    let f = fixture();
    let root = f.root.clone();
    let _aside = aside.then(seams::force_aside_backup);
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeLink(0) {
            edit_in_place(&root.join("a.txt"), b"-edited");
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    match report {
        TransactionReport::RolledBack(undone) => {
            assert!(
                matches!(undone.cause, FsError::Conflict { .. }),
                "aside={aside}: {:?}",
                undone.cause
            );
        }
        other => panic!("aside={aside}: {other:?}"),
    }
    // The editor's content is the destination, under its own name, and no backup name is left.
    assert_eq!(read(&f.root.join("a.txt")), "old-a-edited", "aside={aside}");
    assert!(!f.root.join("sub").join("b.txt").exists());
    let mut all = Vec::new();
    walk(&f.root, &mut all);
    assert!(
        !all.iter()
            .any(|path| path.to_string_lossy().ends_with("-0b")),
        "aside={aside}: a backup name was left behind: {all:?}"
    );
    assert_no_residue(&f.root);
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
    // Nothing is locked: the next transaction replaces the edited file.
    let next = prepared(&f).commit().unwrap();
    assert!(next.is_committed(), "aside={aside}: {next:?}");
    assert_new(&f);
}

#[test]
fn a_destination_edited_between_the_check_and_the_link_rolls_back_and_is_never_replaced() {
    destination_edited_before_the_backup(false);
}

#[test]
fn a_destination_edited_before_an_aside_backup_is_renamed_back_and_rolls_back() {
    destination_edited_before_the_backup(true);
}

#[cfg(windows)]
#[test]
fn a_backup_held_open_without_delete_sharing_is_removed_once_the_reader_lets_go() {
    let f = fixture();
    let root = f.root.clone();
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeCleanup(0) {
            let reader = reader_without_delete_sharing(&find_side(&root, "0b"));
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                drop(reader);
            });
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    match report {
        TransactionReport::Committed(done) => {
            assert!(done.retained.is_empty(), "{:?}", done.retained);
        }
        other => panic!("{other:?}"),
    }
    assert_new(&f);
    assert_no_residue(&f.root);
}

/// A set-aside is only non-blocking once both directories were flushed. With either flush
/// failing the journal stays; the leftover is already in `.rktxn/retained/<id>/`, and recovery
/// (flush working again) repeats the flushes, reports it and only then releases the journal.
fn set_aside_flush_failure(failing: Step) {
    let f = fixture();
    let root = f.root.clone();
    let fail = failing.clone();
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeCleanup(0) {
            edit_in_place(&find_side(&root, "0b"), b"-edited");
        }
        (*step == fail).then(|| std::io::Error::other("injected flush failure"))
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    let done = match report {
        TransactionReport::Committed(done) => done,
        other => panic!("{failing:?}: {other:?}"),
    };
    assert_new(&f);
    assert!(
        !done.retained.is_empty(),
        "{failing:?}: the failed flush was not reported"
    );
    // The commit is decided, but the journal and marker stay: the move is not known durable.
    let dir = only_txn_dir(&f.root);
    assert!(
        dir.join("journal").exists(),
        "{failing:?}: journal released"
    );
    assert!(dir.join("commit").exists(), "{failing:?}: marker released");
    // A new transaction is still refused until recovery has finished it.
    let refused = FileTransaction::prepare(
        &f.root,
        vec![TransactionEntry::new("a.txt", f.stage.join("b.txt"))],
        TransactionOptions::default(),
    )
    .unwrap()
    .commit()
    .unwrap_err();
    assert!(matches!(refused, FsError::Locked { .. }), "{refused:?}");

    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{failing:?}: {report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::CompletedSetAside { items } => {
            assert_eq!(items.len(), 1, "{failing:?}: {items:?}");
            assert_eq!(read(&items[0].path), "old-a-edited");
        }
        other => panic!("{failing:?}: {other:?}"),
    }
    assert_new(&f);
    assert_no_residue(&f.root);
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
}

#[test]
fn a_failed_flush_of_the_directory_a_leftover_left_keeps_the_journal() {
    set_aside_flush_failure(Step::SetAsideSyncSource);
}

#[test]
fn a_failed_flush_of_the_retained_directory_keeps_the_journal() {
    set_aside_flush_failure(Step::SetAsideSyncTarget);
}

/// Recovery that again cannot flush keeps the journal too, and still sees the leftover.
#[test]
fn recovery_that_cannot_flush_the_set_aside_keeps_the_journal() {
    let f = fixture();
    let guard = fail_at(vec![Step::BeforeCleanup(0)]);
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    edit_in_place(&find_side(&f.root, "0b"), b"-edited");
    let guard = fail_at(vec![Step::SetAsideSyncTarget]);
    let report = recover_transactions(&f.root).unwrap();
    drop(guard);
    assert!(!report.is_clean(), "{report:?}");
    assert!(only_txn_dir(&f.root).join("journal").exists());
    let guard = fail_at(vec![Step::SetAsideSyncSource]);
    let report = recover_transactions(&f.root).unwrap();
    drop(guard);
    assert!(!report.is_clean(), "still not flushed: {report:?}");
    assert!(only_txn_dir(&f.root).join("journal").exists());
    // Flushes work again: the leftover is reported and the journal goes.
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(matches!(
        report.transactions[0].outcome,
        RecoveryOutcome::CompletedSetAside { .. }
    ));
    assert_no_residue(&f.root);
}

// ---- identity before cleanup authority: staged names, the claim window ----

fn side_names(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut all = Vec::new();
    walk(root, &mut all);
    all.into_iter()
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            name.contains(".rktxn-") && name.ends_with(suffix)
        })
        .collect()
}

/// The staged-identity token of every `entry` line of the only journal.
fn staged_tokens(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("journal"))
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("entry "))
        .map(|line| line.split(' ').nth(8).unwrap().to_string())
        .collect()
}

type Window = (Step, String, bool);

#[test]
fn the_staged_identity_is_journaled_before_the_staged_name_exists() {
    let f = fixture();
    let log: Rc<RefCell<Vec<Window>>> = Rc::new(RefCell::new(Vec::new()));
    let (sink, root) = (Rc::clone(&log), f.root.clone());
    let guard = seams::on_step(move |step| {
        if matches!(
            step,
            Step::BeforeStageClaim(0) | Step::AfterStageClaim(0) | Step::AfterStage(0)
        ) {
            let token = staged_tokens(&only_txn_dir(&root)).remove(0);
            let named = !side_names(&root, "-0s").is_empty();
            sink.borrow_mut().push((step.clone(), token, named));
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    let log = log.borrow();
    assert_eq!(log.len(), 3, "{log:?}");
    let (before, after, named) = (&log[0], &log[1], &log[2]);
    assert_eq!(before.1, "-", "no claim before the claim step");
    assert!(!before.2, "no staged name yet");
    assert_ne!(after.1, "-", "the claim is durable");
    assert!(after.1.ends_with(".0.0"), "a key only: {}", after.1);
    assert!(!after.2, "the claim is journaled before the name exists");
    assert!(named.2, "the staged name exists after the stage step");
    assert_ne!(named.1, "-");
}

#[test]
fn an_error_in_the_claim_window_changes_nothing_and_leaves_no_residue() {
    for step in [Step::BeforeStageClaim(0), Step::AfterStageClaim(0)] {
        let f = fixture();
        let prepared = prepared(&f);
        let guard = fail_at(vec![step.clone()]);
        let error = prepared.commit().unwrap_err();
        drop(guard);
        assert!(is_injected(&error), "{step:?}: {error:?}");
        assert_old(&f);
        assert_no_residue(&f.root);
        assert!(recover_transactions(&f.root)
            .unwrap()
            .transactions
            .is_empty());
    }
}

#[test]
fn killing_inside_the_claim_window_leaves_only_a_writer_temp_and_recovers() {
    for step in [Step::BeforeStageClaim(0), Step::AfterStageClaim(0)] {
        for aside in [false, true] {
            let f = fixture();
            kill_at(&f, &step, aside);
            let report = recover_transactions(&f.root).unwrap();
            assert!(report.is_clean(), "{step:?}: {report:?}");
            assert_old(&f);
            // Only the writer's own temporary name may remain; no staged or backup name.
            for path in side_names(&f.root, "") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(name.contains(".rktmp-"), "{step:?}: left behind: {path:?}");
            }
            assert!(txn_dirs(&f.root).is_empty(), "{step:?}");
            assert!(recover_transactions(&f.root)
                .unwrap()
                .transactions
                .is_empty());
        }
    }
}

fn staged_name_of(root: &Path, leaf_dir: &str, leaf: &str, index: usize) -> PathBuf {
    let id = only_txn_dir(root)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    root.join(leaf_dir)
        .join(format!(".{leaf}.rktxn-{id}-{index}s"))
}

#[test]
fn a_file_at_a_staged_name_with_no_journaled_identity_is_preserved() {
    let f = fixture();
    let root = f.root.clone();
    // Entry 1 has no record yet when the commit fails before it is staged; a foreign file already
    // sits at its staged name.
    let guard = seams::on_step(move |step| match step {
        Step::AfterJournalStaging => {
            fs::write(staged_name_of(&root, "sub", "b.txt", 1), "foreign").unwrap();
            None
        }
        Step::BeforeStage(1) => Some(std::io::Error::other("injected")),
        _ => None,
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    let stuck = match report {
        TransactionReport::RecoveryRequired(stuck) => stuck,
        other => panic!("{other:?}"),
    };
    assert!(
        stuck
            .retained
            .iter()
            .any(|item| item.path.to_string_lossy().ends_with("-1s")),
        "{:?}",
        stuck.retained
    );
    let foreign = side_names(&f.root, "-1s");
    assert_eq!(foreign.len(), 1, "{foreign:?}");
    assert_eq!(read(&foreign[0]), "foreign");
    // Entry 0's own staged copy (journaled before it was named) was removed.
    assert!(side_names(&f.root, "-0s").is_empty());
    assert_old(&f);
    // Recovery keeps it too, and the journal stays.
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    assert_eq!(read(&foreign[0]), "foreign");
    assert!(only_txn_dir(&f.root).join("journal").exists());
    // Once a person removes it, recovery finishes.
    fs::remove_file(&foreign[0]).unwrap();
    let report = recover_transactions(&f.root).unwrap();
    assert_eq!(
        report.transactions[0].outcome,
        RecoveryOutcome::Discarded,
        "{report:?}"
    );
    assert_no_residue(&f.root);
}

#[test]
fn a_file_at_a_staged_name_that_is_not_the_journaled_file_is_preserved() {
    let f = fixture();
    let root = f.root.clone();
    // A foreign file takes entry 0's staged name after the journal exists. The copy's own file
    // (journaled by key) cannot be given that name, the commit stops, and discard must not treat
    // the name as ours.
    let guard = seams::on_step(move |step| {
        if *step == Step::AfterJournalStaging {
            fs::write(staged_name_of(&root, "", "a.txt", 0), "foreign").unwrap();
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    let stuck = match report {
        TransactionReport::RecoveryRequired(stuck) => stuck,
        other => panic!("{other:?}"),
    };
    assert!(
        stuck
            .retained
            .iter()
            .any(|item| item.path.to_string_lossy().ends_with("-0s")),
        "{:?}",
        stuck.retained
    );
    let foreign = side_names(&f.root, "-0s");
    assert_eq!(foreign.len(), 1, "{foreign:?}");
    assert_eq!(read(&foreign[0]), "foreign");
    assert_old(&f);
    let report = recover_transactions(&f.root).unwrap();
    assert!(!report.is_clean(), "{report:?}");
    assert_eq!(read(&foreign[0]), "foreign");
    fs::remove_file(&foreign[0]).unwrap();
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_no_residue(&f.root);
}

// ---- a non-NotFound error while looking for an earlier set-aside keeps the journal ----

#[test]
fn an_error_other_than_not_found_while_looking_for_a_set_aside_keeps_the_journal() {
    let f = fixture();
    let root = f.root.clone();
    // The move happens, then its directory flush fails: the journal stays with the leftover moved.
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeCleanup(0) {
            edit_in_place(&find_side(&root, "0b"), b"-edited");
        }
        (*step == Step::SetAsideSyncTarget).then(|| std::io::Error::other("injected flush"))
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(report.is_committed(), "{report:?}");
    let dir = only_txn_dir(&f.root);
    assert!(dir.join("journal").exists());
    // Recovery cannot tell whether the earlier move happened: the look fails with access denied.
    let guard = seams::on_step(|step| {
        (*step == Step::SetAsideProbe).then(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected probe failure",
            )
        })
    });
    let report = recover_transactions(&f.root).unwrap();
    drop(guard);
    assert!(!report.is_clean(), "{report:?}");
    assert!(
        dir.join("journal").exists(),
        "journal released on an unknown"
    );
    assert!(dir.join("commit").exists());
    // Asking again with a working look repeats the flushes and reports the leftover.
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::CompletedSetAside { items } => {
            assert_eq!(items.len(), 1, "{items:?}");
            assert_eq!(read(&items[0].path), "old-a-edited");
        }
        other => panic!("{other:?}"),
    }
    assert_no_residue(&f.root);
}

// ---- rollback progress survives a stop between the staged removal and the journal removal ----

/// An editor changes the destination before the commit backs it up; the rollback then removes the
/// staged copies and stops (an error standing in for a kill) at `failing`. Replaying recovery must
/// still roll back cleanly, keep the editor's content and leave the root usable.
fn replay_after_rollback_stopped_at(failing: Step) {
    let f = fixture();
    let root = f.root.clone();
    let fail = failing.clone();
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforeLink(0) {
            edit_in_place(&root.join("a.txt"), b"-edited");
        }
        (*step == fail).then(|| std::io::Error::other("injected stop"))
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(
        matches!(report, TransactionReport::RecoveryRequired(_)),
        "{failing:?}: {report:?}"
    );
    assert!(only_txn_dir(&f.root).join("journal").exists());
    assert_eq!(read(&f.root.join("a.txt")), "old-a-edited");
    if failing == Step::BeforeJournalRemove {
        // This is the exact boundary: every staged copy is already gone, the journal is not.
        assert!(side_names(&f.root, "s").is_empty(), "staged copy left");
    }
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{failing:?}: {report:?}");
    assert_eq!(
        report.transactions[0].outcome,
        RecoveryOutcome::RolledBack,
        "{failing:?}"
    );
    assert_eq!(read(&f.root.join("a.txt")), "old-a-edited", "{failing:?}");
    assert!(!f.root.join("sub").join("b.txt").exists());
    assert_no_residue(&f.root);
    assert!(recover_transactions(&f.root)
        .unwrap()
        .transactions
        .is_empty());
    let next = prepared(&f).commit().unwrap();
    assert!(next.is_committed(), "{failing:?}: {next:?}");
    assert_new(&f);
}

#[test]
fn a_replay_after_the_staged_copies_are_gone_still_rolls_back_and_keeps_the_edit() {
    replay_after_rollback_stopped_at(Step::BeforeJournalRemove);
}

#[test]
fn a_replay_after_the_rollback_mark_is_written_still_rolls_back() {
    replay_after_rollback_stopped_at(Step::AfterRollbackMark(0));
}

#[test]
fn a_replay_when_the_rollback_mark_could_not_be_written_still_rolls_back() {
    replay_after_rollback_stopped_at(Step::BeforeRollbackMark(0));
}

/// Without the mark a destination created by somebody else for a create-new entry is the same
/// case: the staged copy proves it never published, so the mark must cover it too.
#[test]
fn a_replay_for_a_create_new_destination_somebody_else_created_still_rolls_back() {
    let f = fixture();
    let root = f.root.clone();
    let guard = seams::on_step(move |step| {
        if *step == Step::BeforePublish(1) {
            fs::write(root.join("sub").join("b.txt"), "intruder").unwrap();
        }
        (*step == Step::BeforeJournalRemove).then(|| std::io::Error::other("injected stop"))
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    assert!(
        matches!(report, TransactionReport::RecoveryRequired(_)),
        "{report:?}"
    );
    assert!(side_names(&f.root, "s").is_empty());
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(read(&f.root.join("sub").join("b.txt")), "intruder");
    assert_eq!(read(&f.root.join("a.txt")), "old-a");
    assert_no_residue(&f.root);
}

// ---- a delete retried after a sharing failure never deletes a file that took the name ----

/// Put a different file at `path` the way another process would, by renaming it over the name
/// (both inodes exist at once, so the newcomer cannot reuse the old inode number).
fn swap_in_foreign(path: &Path) {
    let temp = path.with_file_name("foreign.tmp");
    fs::write(&temp, "foreign").unwrap();
    fs::rename(&temp, path).unwrap();
}

#[test]
fn a_staged_name_that_changes_hands_during_the_delete_backoff_is_not_deleted() {
    let f = fixture();
    let root = f.root.clone();
    // The commit stops before entry 1; discard then deletes entry 0's journaled staged copy. The
    // first delete attempt fails (as a Windows sharing violation would), and while the retry
    // pauses a foreign file takes the staged name.
    let _failing = seams::force_delete_failures(1);
    let guard = seams::on_step(move |step| match step {
        Step::BeforeStage(1) => Some(std::io::Error::other("injected")),
        Step::DeleteBackoff => {
            swap_in_foreign(&staged_name_of(&root, "", "a.txt", 0));
            None
        }
        _ => None,
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    let stuck = match report {
        TransactionReport::RecoveryRequired(stuck) => stuck,
        other => panic!("{other:?}"),
    };
    assert!(
        stuck
            .retained
            .iter()
            .any(|item| item.path.to_string_lossy().ends_with("-0s")),
        "{:?}",
        stuck.retained
    );
    let foreign = side_names(&f.root, "-0s");
    assert_eq!(foreign.len(), 1, "{foreign:?}");
    assert_eq!(read(&foreign[0]), "foreign", "the newcomer was deleted");
    assert!(only_txn_dir(&f.root).join("journal").exists());
    assert_old(&f);
    // Recovery keeps it too (it is not the journaled file), then finishes once it is removed.
    assert!(!recover_transactions(&f.root).unwrap().is_clean());
    assert_eq!(read(&foreign[0]), "foreign");
    fs::remove_file(&foreign[0]).unwrap();
    let report = recover_transactions(&f.root).unwrap();
    assert_eq!(
        report.transactions[0].outcome,
        RecoveryOutcome::Discarded,
        "{report:?}"
    );
    assert_no_residue(&f.root);
}

#[test]
fn a_backup_name_that_changes_hands_during_the_delete_backoff_is_not_deleted() {
    let f = fixture();
    let root = f.root.clone();
    // Cleanup of a committed transaction deletes the backup of a.txt: the first attempt fails and
    // a foreign file takes the backup name during the pause.
    let _failing = seams::force_delete_failures(1);
    let guard = seams::on_step(move |step| {
        if *step == Step::DeleteBackoff {
            swap_in_foreign(&find_side(&root, "0b"));
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(guard);
    let done = match report {
        TransactionReport::Committed(done) => done,
        other => panic!("{other:?}"),
    };
    assert!(
        !done.retained.is_empty(),
        "the refused delete was not reported"
    );
    assert_new(&f);
    assert_eq!(
        read(&find_side(&f.root, "0b")),
        "foreign",
        "the newcomer was deleted"
    );
    let dir = only_txn_dir(&f.root);
    assert!(dir.join("journal").exists());
    assert!(dir.join("commit").exists());
    // Recovery finds a file that is not the recorded backup and sets it aside, never deletes it.
    let report = recover_transactions(&f.root).unwrap();
    assert!(report.is_clean(), "{report:?}");
    match &report.transactions[0].outcome {
        RecoveryOutcome::CompletedSetAside { items } => {
            assert_eq!(items.len(), 1, "{items:?}");
            assert_eq!(read(&items[0].path), "foreign");
        }
        other => panic!("{other:?}"),
    }
    assert_no_residue(&f.root);
}

/// Without a change of hands the retry still deletes the file it identified: the pause alone does
/// not make the delete refuse.
#[test]
fn a_forced_sharing_failure_is_retried_and_the_identified_file_is_removed() {
    let f = fixture();
    let _failing = seams::force_delete_failures(2);
    let report = prepared(&f).commit().unwrap();
    match report {
        TransactionReport::Committed(done) => {
            assert!(done.retained.is_empty(), "{:?}", done.retained);
        }
        other => panic!("{other:?}"),
    }
    assert_new(&f);
    assert_no_residue(&f.root);
}

// Restore boundaries require a failed commit or a pending journal, not the successful commit lane.
#[test]
fn killing_during_rollback_and_recovery_restoration_preserves_prior_files() {
    for aside in [false, true] {
        for mode in ["rollback", "recover"] {
            for step in [Step::BeforeRestore(0), Step::AfterRestore(0)] {
                let f = fixture();
                if mode == "recover" {
                    let _aside = aside.then(seams::force_aside_backup);
                    let guard = fail_at(vec![Step::BeforePublish(1), Step::BeforeRestore(0)]);
                    assert!(matches!(
                        prepared(&f).commit().unwrap(),
                        TransactionReport::RecoveryRequired(_)
                    ));
                    drop(guard);
                }
                kill_in_mode(&f, &step, aside, mode);
                let report = recover_transactions(&f.root).unwrap();
                assert!(report.is_clean(), "{mode} {aside} {step:?}: {report:?}");
                assert_old(&f);
                assert_no_residue(&f.root);
                assert!(recover_transactions(&f.root)
                    .unwrap()
                    .transactions
                    .is_empty());
            }
        }
    }
}

#[test]
fn exclusive_rename_crash_placeholders_are_retained_without_guessing_ownership() {
    for restore_placeholder in [false, true] {
        let f = fixture();
        let _aside = seams::force_aside_backup();
        let guard = fail_at(vec![Step::BeforePublish(0), Step::BeforeRestore(0)]);
        assert!(matches!(
            prepared(&f).commit().unwrap(),
            TransactionReport::RecoveryRequired(_)
        ));
        drop(guard);
        let backup = find_side(&f.root, "0b");
        if restore_placeholder {
            // Restore fallback reserved destination but died before moving the backup.
            fs::write(f.root.join("a.txt"), b"").unwrap();
        } else {
            // Backup fallback reserved backup but died before moving the destination.
            fs::rename(&backup, f.root.join("a.txt")).unwrap();
            fs::write(&backup, b"").unwrap();
        }
        let report = recover_transactions(&f.root).unwrap();
        assert!(!report.is_clean(), "{report:?}");
        assert!(only_txn_dir(&f.root).join("journal").exists());
        let prior = if restore_placeholder {
            backup
        } else {
            f.root.join("a.txt")
        };
        assert_eq!(read(&prior), "old-a");
    }
}

#[test]
fn a_crc_valid_journal_cannot_redirect_side_names_to_sibling_files() {
    for token in [5, 6] {
        let f = fixture();
        let guard = fail_at(vec![Step::BeforePublish(1), Step::BeforeRestore(0)]);
        assert!(matches!(
            prepared(&f).commit().unwrap(),
            TransactionReport::RecoveryRequired(_)
        ));
        drop(guard);
        let sibling = f.root.join("sibling.txt");
        fs::write(&sibling, "foreign").unwrap();
        rewrite_journal(&only_txn_dir(&f.root), |line| {
            if !line.starts_with("entry 0 ") {
                return line.to_owned();
            }
            let mut parts: Vec<String> = line.split(' ').map(str::to_owned).collect();
            parts[token] = "sibling.txt"
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            parts.join(" ")
        });
        let report = recover_transactions(&f.root).unwrap();
        assert!(!report.is_clean(), "{report:?}");
        assert_eq!(read(&sibling), "foreign");
        assert!(only_txn_dir(&f.root).join("journal").exists());
    }
}

#[test]
fn recovery_restores_private_prior_file_without_weakening_its_permissions() {
    let f = fixture();
    rightkit_fs::private::write_private_text(&f.root.join("a.txt"), "old-a").unwrap();
    let guard = fail_at(vec![Step::BeforePublish(1), Step::BeforeRestore(0)]);
    let plan = vec![
        TransactionEntry::new("a.txt", f.stage.join("a.txt")).private(),
        TransactionEntry::create_new("sub/b.txt", f.stage.join("b.txt")).private(),
    ];
    let report = FileTransaction::prepare(&f.root, plan, TransactionOptions::default())
        .unwrap()
        .commit()
        .unwrap();
    drop(guard);
    assert!(matches!(report, TransactionReport::RecoveryRequired(_)));
    assert!(rightkit_fs::private::is_owner_only_strict(&f.root.join("a.txt")).unwrap());
    let recovered = recover_transactions(&f.root).unwrap();
    assert!(recovered.is_clean(), "{recovered:?}");
    assert_old(&f);
    assert!(rightkit_fs::private::is_owner_only_strict(&f.root.join("a.txt")).unwrap());
    assert_no_residue(&f.root);
}

/// Native-host qualification provides a writable directory on a different volume explicitly.
#[test]
fn a_candidate_on_another_volume_is_copied_before_publication() {
    let Some(other_volume) = std::env::var_os("RIGHTKIT_FS_TEST_OTHER_VOLUME") else {
        eprintln!("cross-volume fixture unavailable; set RIGHTKIT_FS_TEST_OTHER_VOLUME for native qualification");
        return;
    };
    let f = fixture();
    let candidate_dir = tempfile::tempdir_in(other_volume).unwrap();
    let candidate = candidate_dir.path().join("candidate.txt");
    fs::write(&candidate, "cross-volume-new").unwrap();
    assert_ne!(
        FileKey::of_path(&f.root).unwrap().volume,
        FileKey::of_path(&candidate).unwrap().volume,
        "RIGHTKIT_FS_TEST_OTHER_VOLUME must identify a different filesystem"
    );
    let prior = FileKey::of_path(&candidate).unwrap();
    let plan = vec![TransactionEntry::new("a.txt", &candidate).private()];
    let report = FileTransaction::prepare(&f.root, plan, TransactionOptions::default())
        .unwrap()
        .commit()
        .unwrap();
    assert!(report.is_committed(), "{report:?}");
    assert_eq!(read(&f.root.join("a.txt")), "cross-volume-new");
    assert_eq!(read(&candidate), "cross-volume-new");
    assert_eq!(FileKey::of_path(&candidate).unwrap(), prior);
    assert!(rightkit_fs::private::is_owner_only_strict(&f.root.join("a.txt")).unwrap());
    assert_no_residue(&f.root);
}

#[test]
fn preparation_refuses_substituted_root_and_destination_parent_directories() {
    for substitute_root in [false, true] {
        let f = fixture();
        let plan = prepared(&f);
        if substitute_root {
            fs::rename(&f.root, f.root.with_file_name("saved-root")).unwrap();
            fs::create_dir_all(f.root.join("sub")).unwrap();
            fs::write(f.root.join("a.txt"), "old-a").unwrap();
        } else {
            fs::rename(f.root.join("sub"), f.root.join("saved-sub")).unwrap();
            fs::create_dir(f.root.join("sub")).unwrap();
        }
        let error = plan.commit().unwrap_err();
        assert!(matches!(error, FsError::Conflict { .. }), "{error:?}");
        assert_eq!(read(&f.root.join("a.txt")), "old-a");
        assert!(!f.root.join("sub/b.txt").exists());
        assert!(
            !f.root.join(".rktxn").exists(),
            "substitution was refused after mutation"
        );
    }
}

#[cfg(feature = "testing")]
#[test]
fn rollback_flush_failure_retains_journal_for_aside_restore_and_create_new_delete() {
    for aside in [false, true] {
        let f = fixture();
        let _aside = aside.then(seams::force_aside_backup);
        let flush_guard = Rc::new(RefCell::new(None));
        let guard_slot = Rc::clone(&flush_guard);
        let hook = seams::on_step(move |step| {
            let restore = if aside {
                Step::BeforeRestore(0)
            } else {
                Step::BeforeRestore(1)
            };
            if *step == restore {
                *guard_slot.borrow_mut() = Some(rightkit_fs::atomic::testing::fail_dir_sync(
                    std::io::ErrorKind::PermissionDenied,
                ));
            }
            let failure = if aside {
                Step::BeforePublish(0)
            } else {
                Step::BeforeCommitMarker
            };
            (*step == failure).then(|| std::io::Error::other("start rollback"))
        });
        let report = prepared(&f).commit().unwrap();
        drop(hook);
        drop(flush_guard.borrow_mut().take());
        assert!(
            matches!(report, TransactionReport::RecoveryRequired(_)),
            "{report:?}"
        );
        assert!(only_txn_dir(&f.root).join("journal").exists());
        let recovered = recover_transactions(&f.root).unwrap();
        assert!(recovered.is_clean(), "{recovered:?}");
        assert_old(&f);
        assert_no_residue(&f.root);
    }
}

#[test]
fn foreign_journal_and_marker_replacements_are_never_deleted_during_cleanup() {
    for marker in [false, true] {
        let f = fixture();
        let root = f.root.clone();
        let hook = seams::on_step(move |step| {
            let target_step = if marker {
                Step::AfterJournalRemove
            } else {
                Step::BeforeJournalRemove
            };
            if *step == target_step {
                let name = if marker { "commit" } else { "journal" };
                swap_in_foreign(&only_txn_dir(&root).join(name));
            }
            None
        });
        let report = prepared(&f).commit().unwrap();
        drop(hook);
        assert!(
            matches!(&report, TransactionReport::Committed(done) if !done.retained.is_empty()),
            "{report:?}"
        );
        let path = only_txn_dir(&f.root).join(if marker { "commit" } else { "journal" });
        assert_eq!(read(&path), "foreign");
        assert_new(&f);
        assert!(!recover_transactions(&f.root).unwrap().is_clean());
        assert_eq!(read(&path), "foreign");
    }
}

#[test]
fn identical_record_content_under_a_new_identity_during_delete_retry_is_retained() {
    for marker in [false, true] {
        let f = fixture();
        let root = f.root.clone();
        let failures = Rc::new(RefCell::new(None));
        let failure_slot = Rc::clone(&failures);
        let hook = seams::on_step(move |step| {
            let start = if marker {
                Step::AfterJournalRemove
            } else {
                Step::BeforeJournalRemove
            };
            if *step == start {
                *failure_slot.borrow_mut() = Some(seams::force_delete_failures(1));
            }
            if *step == Step::DeleteBackoff {
                let path = only_txn_dir(&root).join(if marker { "commit" } else { "journal" });
                let bytes = fs::read(&path).unwrap();
                let other = path.with_file_name("replacement.tmp");
                fs::write(&other, bytes).unwrap();
                fs::rename(other, &path).unwrap();
            }
            None
        });
        let report = FileTransaction::prepare(
            &f.root,
            vec![TransactionEntry::create_new(
                "sub/b.txt",
                f.stage.join("b.txt"),
            )],
            TransactionOptions::default(),
        )
        .unwrap()
        .commit()
        .unwrap();
        drop(hook);
        drop(failures.borrow_mut().take());
        assert!(
            matches!(&report, TransactionReport::Committed(done) if !done.retained.is_empty()),
            "{report:?}"
        );
        let path = only_txn_dir(&f.root).join(if marker { "commit" } else { "journal" });
        assert!(
            path.exists(),
            "replacement with identical content was deleted"
        );
        assert_eq!(read(&f.root.join("sub/b.txt")), "new-b");
    }
}

#[test]
fn generated_record_temp_syntax_does_not_authorize_cleanup() {
    let f = fixture();
    let hook = fail_at(vec![Step::BeforeStage(0), Step::BeforeJournalRemove]);
    assert!(matches!(
        prepared(&f).commit().unwrap(),
        TransactionReport::RecoveryRequired(_)
    ));
    drop(hook);
    let dir = only_txn_dir(&f.root);
    let writer = rightkit_fs::AtomicWriter::create_with(
        &dir.join("journal"),
        rightkit_fs::AtomicWriteOptions::default(),
    )
    .unwrap();
    let temp = writer.temp_path().to_path_buf();
    drop(writer);
    fs::write(&temp, "foreign").unwrap();
    let recovered = recover_transactions(&f.root).unwrap();
    assert!(!recovered.is_clean(), "{recovered:?}");
    assert_eq!(read(&temp), "foreign");
    assert!(dir.join("journal").exists());
    assert_old(&f);
}

// These observe barriers and injected errors; they do not simulate power loss.
#[test]
fn linked_backup_is_flushed_before_the_live_destination_can_be_overwritten() {
    let f = fixture();
    // Establish that this fixture volume actually supports the hard-link branch.
    let trial = f.root.join("hard-link-probe");
    fs::hard_link(f.root.join("a.txt"), &trial).unwrap();
    fs::remove_file(trial).unwrap();
    let baseline = Rc::new(RefCell::new(None));
    let visited = Rc::new(RefCell::new(false));
    let (counts, observed, root) = (Rc::clone(&baseline), Rc::clone(&visited), f.root.clone());
    let hook = seams::on_step(move |step| {
        if *step == Step::BeforeLink(0) {
            *counts.borrow_mut() = Some(atomic_seams::sync_counts());
        }
        if *step == Step::AfterBackup(0) {
            let before = counts.borrow();
            let before = before.as_ref().expect("BeforeLink precedes AfterBackup");
            let after = atomic_seams::sync_counts();
            assert!(
                after.file_syncs > before.file_syncs,
                "prior bytes were not flushed"
            );
            assert!(
                after.dir_syncs > before.dir_syncs,
                "backup name was not flushed"
            );
            assert_eq!(read(&root.join("a.txt")), "old-a");
            *observed.borrow_mut() = true;
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(hook);
    assert!(report.is_committed(), "{report:?}");
    assert!(*visited.borrow());
    assert_new(&f);
    assert_no_residue(&f.root);
}

#[cfg(feature = "testing")]
#[test]
fn linked_backup_flush_failure_refuses_live_publication_and_remains_recoverable() {
    let f = fixture();
    let trial = f.root.join("hard-link-probe");
    fs::hard_link(f.root.join("a.txt"), &trial).unwrap();
    fs::remove_file(trial).unwrap();
    let failure = Rc::new(RefCell::new(None));
    let published = Rc::new(RefCell::new(false));
    let (slot, observed) = (Rc::clone(&failure), Rc::clone(&published));
    let hook = seams::on_step(move |step| {
        if *step == Step::BeforeLink(0) {
            *slot.borrow_mut() = Some(rightkit_fs::atomic::testing::fail_dir_sync(
                std::io::ErrorKind::PermissionDenied,
            ));
        }
        if matches!(
            step,
            Step::AfterBackup(0) | Step::BeforePublish(0) | Step::AfterPublish(0)
        ) {
            *observed.borrow_mut() = true;
        }
        None
    });
    let report = prepared(&f).commit().unwrap();
    drop(hook);
    drop(failure.borrow_mut().take());
    assert!(
        !*published.borrow(),
        "backup failure authorized publication"
    );
    assert_old(&f);
    assert!(
        matches!(report, TransactionReport::RecoveryRequired(_)),
        "{report:?}"
    );
    assert!(only_txn_dir(&f.root).join("journal").exists());
    let recovery = recover_transactions(&f.root).unwrap();
    assert!(recovery.is_clean(), "{recovery:?}");
    assert_old(&f);
    assert_no_residue(&f.root);
}
