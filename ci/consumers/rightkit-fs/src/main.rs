use rightkit_fs::{
    atomic_write_private, recover_transactions, sweep_stale_temps, sweep_stale_temps_report,
    AtomicWriteOptions, AtomicWriter, ExpectedPrior, FileKey, FileTransaction, FsError,
    KeyedWriteLocks, StateDir, TransactionEntry, TransactionOptions, TransactionReport,
};
use rightkit_fs::transaction::seams::{self, Step};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

fn entries(other: &Path) -> Vec<TransactionEntry> {
    vec![
        TransactionEntry::new("a", other.join("a")).private(),
        TransactionEntry::new("b", other.join("b")).private(),
    ]
}

fn old(root: &Path) {
    atomic_write_private(&root.join("a"), b"old-a").unwrap();
    atomic_write_private(&root.join("b"), b"old-b").unwrap();
}

fn assert_old(root: &Path) {
    assert_eq!(fs::read(root.join("a")).unwrap(), b"old-a");
    assert_eq!(fs::read(root.join("b")).unwrap(), b"old-b");
}

// Two-file transaction: one replacement and one create-new in a subdirectory, both private.
fn commit_pair(root: &Path, stage: &Path) {
    let report = FileTransaction::prepare(
        root,
        vec![
            TransactionEntry::new("a.txt", stage.join("a.txt")).private(),
            TransactionEntry::create_new("sub/b.txt", stage.join("b.txt")).private(),
        ],
        TransactionOptions::default(),
    )
    .expect("two-file transaction refused at prepare")
    .commit()
    .expect("two-file transaction failed at commit");
    assert!(report.is_committed(), "two-file transaction not committed: {report:?}");
    assert_eq!(
        fs::read(root.join("a.txt")).unwrap(),
        b"new-a",
        "a.txt readback after commit"
    );
    assert_eq!(
        fs::read(root.join("sub/b.txt")).unwrap(),
        b"new-b",
        "sub/b.txt readback after commit"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [root.join("a.txt"), root.join("sub/b.txt")] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} must stay owner-only after commit", path.display());
        }
    }
    assert!(
        recover_transactions(root).unwrap().transactions.is_empty(),
        "transaction journal left behind after commit"
    );
}

// A stale identity expectation must refuse without touching the external edit or unrelated files.
fn conflicts_preserve_content(root: &Path, stage: &Path) {
    let unrelated = root.join("unrelated.txt");
    fs::write(&unrelated, b"unrelated-old").unwrap();
    let target = root.join("a.txt");
    let expected = FileKey::of_path(&target).unwrap();
    let prepared = FileTransaction::prepare(
        root,
        vec![
            TransactionEntry::new("a.txt", stage.join("a.txt"))
                .expect(ExpectedPrior::Identity(expected)),
        ],
        TransactionOptions::default(),
    )
    .expect("identity-checked transaction refused at prepare");
    // Public atomic publication creates distinct identity while original still exists.
    let mut editor = AtomicWriter::create_with(&target, AtomicWriteOptions::default()).unwrap();
    editor.write_all(b"external-edit").unwrap();
    editor.commit().unwrap();
    assert!(
        matches!(prepared.commit(), Err(FsError::Conflict { .. })),
        "stale identity expectation must refuse with FsError::Conflict"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        b"external-edit",
        "refused transaction changed the externally edited target"
    );
    assert_eq!(
        fs::read(&unrelated).unwrap(),
        b"unrelated-old",
        "refused transaction changed an unrelated file"
    );
    let refused = FileTransaction::prepare(
        root,
        vec![TransactionEntry::create_new("a.txt", stage.join("b.txt"))],
        TransactionOptions::default(),
    );
    assert!(
        matches!(refused, Err(FsError::AlreadyExists(_))),
        "create-new over an existing destination was not refused"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        b"external-edit",
        "create-new refusal changed the target"
    );
    assert_eq!(
        fs::read(&unrelated).unwrap(),
        b"unrelated-old",
        "create-new refusal changed an unrelated file"
    );
}

// Overlapping writers on one destination (through an alias path) must serialize.
fn keyed_serialization(root: &Path) {
    let locks = KeyedWriteLocks::new();
    let target = root.join("serialized.txt");
    let alias = root.join(".").join("serialized.txt");
    let other_locks = locks.clone();
    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let worker = locks
        .with_destination(&target, |held| {
            let worker = std::thread::spawn(move || {
                let busy =
                    other_locks.try_with_destination(&alias, |_| panic!("overlapping writer"));
                assert!(
                    matches!(busy, Err(FsError::Locked { .. } | FsError::LockTimeout { .. })),
                    "second destination writer must be refused while the first holds the key"
                );
                attempted_tx.send(()).unwrap();
                other_locks
                    .with_destination(&alias, |held| {
                        entered_tx.send(()).unwrap();
                        assert_eq!(fs::read(&alias).unwrap(), b"first");
                        let mut writer = held.atomic_writer(AtomicWriteOptions::default()).unwrap();
                        writer.write_all(b"second").unwrap();
                        writer.commit_checked(|_| Ok(())).unwrap();
                    })
                    .unwrap();
            });
            attempted_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(
                matches!(entered_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
                "second writer entered before the first released the destination"
            );
            let mut writer = held.atomic_writer(AtomicWriteOptions::default()).unwrap();
            writer.write_all(b"first").unwrap();
            writer.commit_checked(|_| Ok(())).unwrap();
            worker
        })
        .unwrap();
    worker.join().unwrap();
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        fs::read(target).unwrap(),
        b"second",
        "serialized second writer did not commit last"
    );
    assert_eq!(
        locks.active_destinations(),
        0,
        "destination lock leaked after both writers finished"
    );
}

// A user import whose name matches a temporary pattern must survive every sweep.
fn native_import_retention(root: &Path) {
    let state = StateDir::open_shared(root).unwrap();
    let target = root.join(".user-record.rktmp-424242-1-0");
    let mut writer = AtomicWriter::create_new_private(&target).unwrap();
    writer.write_all(b"user-record").unwrap();
    writer.commit().unwrap();
    let mut duplicate = AtomicWriter::create_new_private(&target).unwrap();
    duplicate.write_all(b"replacement").unwrap();
    assert!(
        matches!(duplicate.commit(), Err(FsError::AlreadyExists(_))),
        "duplicate native import must be refused with FsError::AlreadyExists"
    );
    assert_eq!(
        sweep_stale_temps(root, Duration::ZERO).unwrap(),
        0,
        "sweep_stale_temps removed a user import"
    );
    assert_eq!(
        state.sweep_temps(Duration::ZERO).unwrap(),
        0,
        "StateDir::sweep_temps removed a user import"
    );
    let report = sweep_stale_temps_report(root).unwrap();
    assert_eq!(report.removed, 0, "sweep_stale_temps_report removed a user import");
    assert!(
        report.retained_live + report.retained_unknown + report.retained_other >= 1,
        "user import candidate was not reported as retained"
    );
    assert_eq!(
        fs::read(target).unwrap(),
        b"user-record",
        "retained user import content changed"
    );
}

fn transaction_checks(base: &Path) {
    let root = base.join("root");
    let stage = base.join("stage");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir(&stage).unwrap();
    fs::write(root.join("a.txt"), b"old-a").unwrap();
    fs::write(stage.join("a.txt"), b"new-a").unwrap();
    fs::write(stage.join("b.txt"), b"new-b").unwrap();
    commit_pair(&root, &stage);
    conflicts_preserve_content(&root, &stage);
    keyed_serialization(&root);
    native_import_retention(&root);
}

fn main() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").unwrap(), "true");
    let temp = std::path::PathBuf::from(std::env::var_os("RUNNER_TEMP").unwrap());
    let plan: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.join("rk-fs-native-state.json")).unwrap()).unwrap();
    let root = Path::new(plan["scratch"].as_str().unwrap())
        .join(format!("rk-fs-consumer-{}", std::process::id()));
    let other = Path::new(plan["other"].as_str().unwrap())
        .join(format!("consumer-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(&other).unwrap();
    assert_ne!(
        FileKey::of_path(&root).unwrap().volume,
        FileKey::of_path(&other).unwrap().volume
    );
    for name in ["a", "b"] {
        let mut writer = AtomicWriter::create_new_private(&other.join(name)).unwrap();
        writer.write_all(format!("new-{name}").as_bytes()).unwrap();
        writer.commit().unwrap();
    }
    let keys = [
        FileKey::of_path(&other.join("a")).unwrap(),
        FileKey::of_path(&other.join("b")).unwrap(),
    ];
    old(&root);
    let staged = Arc::new(AtomicUsize::new(0));
    let count = staged.clone();
    let guard = seams::on_step(move |step| {
        if matches!(step, Step::AfterStage(_)) {
            count.fetch_add(1, Ordering::SeqCst);
        }
        None
    });
    let prepared =
        FileTransaction::prepare(&root, entries(&other), TransactionOptions::default()).unwrap();
    assert_old(&root);
    assert!(prepared.commit().unwrap().is_committed());
    assert_eq!(staged.load(Ordering::SeqCst), 2);
    drop(guard);
    for (index, name) in ["a", "b"].iter().enumerate() {
        assert_eq!(fs::read(root.join(name)).unwrap(), format!("new-{name}").as_bytes());
        assert_eq!(fs::read(other.join(name)).unwrap(), format!("new-{name}").as_bytes());
        assert_eq!(FileKey::of_path(&other.join(name)).unwrap(), keys[index]);
    }
    old(&root);
    let guard = seams::on_step(|step| {
        matches!(step, Step::BeforePublish(1)).then(|| io::Error::other("native rollback fixture"))
    });
    let result = FileTransaction::prepare(&root, entries(&other), TransactionOptions::default())
        .unwrap()
        .commit()
        .unwrap();
    assert!(matches!(result, TransactionReport::RolledBack(_)));
    drop(guard);
    assert_old(&root);
    let aside = seams::force_aside_backup();
    let guard = seams::on_step(|step| {
        matches!(step, Step::BeforePublish(1) | Step::BeforeRestore(0))
            .then(|| io::Error::other("native recovery fixture"))
    });
    let result = FileTransaction::prepare(&root, entries(&other), TransactionOptions::default())
        .unwrap()
        .commit()
        .unwrap();
    assert!(matches!(result, TransactionReport::RecoveryRequired(_)));
    drop(guard);
    drop(aside);
    let recovery = recover_transactions(&root).unwrap();
    assert!(recovery.is_clean());
    assert!(!recovery.transactions.is_empty());
    assert_old(&root);
    assert!(recover_transactions(&root).unwrap().transactions.is_empty());
    let mut fat = serde_json::Value::Null;
    if let Some(mount) = plan["fat"].as_str() {
        let target = Path::new(mount).join(format!("private-{}", std::process::id()));
        let error = match AtomicWriter::create_new_private(&target) {
            Ok(_) => panic!("FAT32 unexpectedly accepted owner-only staging"),
            Err(error) => error,
        };
        assert!(!target.exists());
        let probe = Path::new(mount).join("strict-sync-probe");
        let mut file = fs::File::create(&probe).unwrap();
        file.write_all(b"strict FAT32 probe").unwrap();
        let sync = format!(
            "{:?}",
            rightkit_fs::durable_sync_with(&file, rightkit_fs::SyncPolicy::Strict)
        );
        drop(file);
        fs::remove_file(probe).unwrap();
        fat = serde_json::json!({
            "filesystem": "FAT32",
            "privatePublication": "unsupported-fail-closed",
            "error": error.to_string(),
            "strictFileSync": sync,
            "destinationAbsent": true
        });
    }
    println!(
        "{}",
        serde_json::json!({
            "crossVolume": true,
            "stagedFiles": 2,
            "commitReadback": true,
            "rollback": true,
            "recovery": true,
            "fat": fat
        })
    );
    fs::remove_dir_all(other).unwrap();
    fs::remove_dir_all(root).unwrap();
    // New checks run in their own directory on the native scratch root and clean up after.
    let checks = Path::new(plan["scratch"].as_str().unwrap())
        .join(format!("rk-fs-checks-{}", std::process::id()));
    fs::create_dir(&checks).unwrap();
    transaction_checks(&checks);
    fs::remove_dir_all(checks).unwrap();
    println!("PK01 transaction checks passed");
}
