use rightkit_fs::{
    atomic_write_private, recover_transactions, AtomicWriter, FileKey, FileTransaction,
    TransactionEntry, TransactionOptions, TransactionReport,
};
use rightkit_fs::transaction::seams::{self, Step};
use std::{fs, io::{self, Write}, path::Path, sync::{Arc, atomic::{AtomicUsize, Ordering}}};

fn entries(other: &Path) -> Vec<TransactionEntry> {
    vec![TransactionEntry::new("a", other.join("a")).private(),
         TransactionEntry::new("b", other.join("b")).private()]
}
fn old(root: &Path) {
    atomic_write_private(&root.join("a"), b"old-a").unwrap();
    atomic_write_private(&root.join("b"), b"old-b").unwrap();
}
fn assert_old(root: &Path) {
    assert_eq!(fs::read(root.join("a")).unwrap(), b"old-a");
    assert_eq!(fs::read(root.join("b")).unwrap(), b"old-b");
}
fn main() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").unwrap(), "true");
    let temp = std::path::PathBuf::from(std::env::var_os("RUNNER_TEMP").unwrap());
    let plan: serde_json::Value = serde_json::from_slice(&fs::read(temp.join("rk-fs-native-state.json")).unwrap()).unwrap();
    let root = Path::new(plan["scratch"].as_str().unwrap()).join(format!("rk-fs-consumer-{}", std::process::id()));
    let other = Path::new(plan["other"].as_str().unwrap()).join(format!("consumer-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(&other).unwrap();
    assert_ne!(FileKey::of_path(&root).unwrap().volume, FileKey::of_path(&other).unwrap().volume);
    for name in ["a", "b"] {
        let mut writer = AtomicWriter::create_new_private(&other.join(name)).unwrap();
        writer.write_all(format!("new-{name}").as_bytes()).unwrap();
        writer.commit().unwrap();
    }
    let keys = [FileKey::of_path(&other.join("a")).unwrap(), FileKey::of_path(&other.join("b")).unwrap()];
    old(&root);
    let staged = Arc::new(AtomicUsize::new(0));
    let count = staged.clone();
    let guard = seams::on_step(move |step| { if matches!(step, Step::AfterStage(_)) { count.fetch_add(1, Ordering::SeqCst); } None });
    let prepared = FileTransaction::prepare(&root, entries(&other), TransactionOptions::default()).unwrap();
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
    let guard = seams::on_step(|step| matches!(step, Step::BeforePublish(1)).then(|| io::Error::other("native rollback fixture")));
    let result = FileTransaction::prepare(&root, entries(&other), TransactionOptions::default()).unwrap().commit().unwrap();
    assert!(matches!(result, TransactionReport::RolledBack(_)));
    drop(guard);
    assert_old(&root);
    let aside = seams::force_aside_backup();
    let guard = seams::on_step(|step| matches!(step, Step::BeforePublish(1) | Step::BeforeRestore(0)).then(|| io::Error::other("native recovery fixture")));
    let result = FileTransaction::prepare(&root, entries(&other), TransactionOptions::default()).unwrap().commit().unwrap();
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
        let sync = format!("{:?}", rightkit_fs::durable_sync_with(&file, rightkit_fs::SyncPolicy::Strict));
        drop(file);
        fs::remove_file(probe).unwrap();
        fat = serde_json::json!({"filesystem":"FAT32", "privatePublication":"unsupported-fail-closed", "error":error.to_string(), "strictFileSync":sync, "destinationAbsent":true});
    }
    println!("{}", serde_json::json!({"crossVolume":true,"stagedFiles":2,"commitReadback":true,"rollback":true,"recovery":true,"fat":fat}));
    fs::remove_dir_all(other).unwrap();
    fs::remove_dir_all(root).unwrap();
}
