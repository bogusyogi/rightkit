//! The shared Ultra copy's activation state (#331 follow-up): a relaunched process
//! selects a Ready copy from the persisted activation record, and only a record that still
//! matches. Every test runs from a fresh state with an isolated suite root and app data folder
//! (`test_support::isolate_env`); the process-wide state is reset by the guard.

use super::*;
use crate::test_support::{
    isolate_env, record_file, write_activation_record, write_ready_ultra, write_record_text,
};

fn subdir() -> &'static str {
    legacy_subdir()
}

/// The record `prepare` writes, with one field replaced.
fn record_with(field: &str, value: serde_json::Value, dir: &Path) -> String {
    let spec = crate::ultra_model::spec();
    let mut record = serde_json::json!({"id": spec.id, "revision": spec.version, "path": dir});
    record[field] = value;
    record.to_string()
}

#[test]
fn unset_ready_store_and_matching_record_resolve_the_store_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = isolate_env(tmp.path());
    let dir = write_ready_ultra();
    write_activation_record(&dir);

    assert_eq!(resolve(subdir()), Some(dir.clone()));
    assert_eq!(
        crate::model_paths::resolve_model_dir(subdir()),
        Some(dir.clone())
    );
    assert_eq!(
        crate::model_paths::subdir_for_backend("parakeet-tdt"),
        subdir()
    );
    assert_eq!(
        resolve("unrelated"),
        None,
        "only the pinned Ultra id resolves"
    );
    // The record selects a path; it never counts as a verification made in this process.
    assert!(!is_verified_active());
    assert_eq!(resolve_verified(subdir()), None);
    assert_eq!(
        crate::model_paths::resolve_model_dir_verified(subdir()),
        None
    );
}

#[test]
fn a_record_that_does_not_match_resolves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = isolate_env(tmp.path());
    let dir = write_ready_ultra();

    // No record at all.
    assert_eq!(resolve(subdir()), None);
    assert_eq!(crate::model_paths::resolve_model_dir(subdir()), None);

    let cases = [
        (
            "mismatched id",
            record_with("id", "another-model".into(), &dir),
        ),
        (
            "mismatched revision",
            record_with("revision", "an-older-pin".into(), &dir),
        ),
        (
            "mismatched path",
            record_with(
                "path",
                serde_json::to_value(dir.join("elsewhere")).unwrap(),
                &dir,
            ),
        ),
        ("path of another type", record_with("path", 7.into(), &dir)),
        ("torn json", {
            let whole = record_with("id", crate::ultra_model::spec().id.into(), &dir);
            whole[..whole.len() / 2].to_string()
        }),
        ("empty file", String::new()),
    ];
    for (name, text) in cases {
        write_record_text(&text);
        assert_eq!(resolve(subdir()), None, "{name}");
        assert_eq!(
            crate::model_paths::resolve_model_dir(subdir()),
            None,
            "{name}"
        );
    }

    // A matching record over a store entry that is no longer Ready by size.
    write_activation_record(&dir);
    assert_eq!(resolve(subdir()), Some(dir.clone()));
    let first = crate::ultra_model::spec().files[0].path;
    std::fs::File::create(dir.join(first))
        .unwrap()
        .set_len(1)
        .unwrap();
    assert_eq!(resolve(subdir()), None, "partial store entry");
}

#[test]
fn deactivate_denies_even_a_matching_record_until_a_prepare_verifies_again() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = isolate_env(tmp.path());
    let dir = write_ready_ultra();
    write_activation_record(&dir);
    assert_eq!(resolve(subdir()), Some(dir.clone()));

    deactivate();
    assert_eq!(resolve(subdir()), None);
    assert_eq!(crate::model_paths::resolve_model_dir(subdir()), None);
    assert!(!is_verified_active());

    // A prepare that finds nothing to activate does not lift the denial.
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(prepare(&|_| panic!("nothing to load")).unwrap(), None);
    let dir = write_ready_ultra();
    write_activation_record(&dir);
    assert_eq!(resolve(subdir()), None, "still denied");

    // A successful verification does.
    activate_for_tests(&dir);
    assert!(is_verified_active());
    assert_eq!(resolve(subdir()), Some(dir.clone()));
    assert_eq!(resolve_verified(subdir()), Some(dir));
}

#[test]
fn an_integrity_failed_prepare_denies_the_copy_and_moves_the_record_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = isolate_env(tmp.path());
    // Right sizes, wrong bytes: Ready by size, fails the full hash, and with no download token
    // the store cannot repair it.
    let dir = write_ready_ultra();
    write_activation_record(&dir);
    assert_eq!(
        resolve(subdir()),
        Some(dir.clone()),
        "trusted before the prepare"
    );

    let failure = prepare(&|_| panic!("a copy that fails verification is never loaded"));
    assert!(failure.is_err(), "{failure:?}");

    assert_eq!(
        resolve(subdir()),
        None,
        "denied for the rest of the process"
    );
    assert_eq!(crate::model_paths::resolve_model_dir(subdir()), None);
    assert!(!is_verified_active());
    assert!(
        !record_file().exists(),
        "the next process cannot trust it either"
    );
    assert!(record_file().with_extension("json.rejected").is_file());
    // reclaim_retired_store_entries reads the record file: nothing to act on now.
    assert!(!activation_recorded_for(
        &record_file(),
        &crate::ultra_model::spec()
    ));
}

#[test]
fn a_failed_native_load_denies_the_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let env = isolate_env(tmp.path());
    env.verify_by_receipt();
    let spec = crate::ultra_model::spec();
    let dir = write_ready_ultra();
    crate::verify_receipt::record(&dir, &spec);

    let failure = prepare(&|_| Err("native load failed".into()));
    assert_eq!(failure.unwrap_err(), "native load failed");
    assert!(!record_file().exists(), "a failed load writes no record");
    write_activation_record(&dir);
    assert_eq!(resolve(subdir()), None, "denied until a prepare succeeds");
}

#[test]
fn a_busy_lease_or_an_unwritable_record_keeps_a_matching_record_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let env = isolate_env(tmp.path());
    env.verify_by_receipt();
    let spec = crate::ultra_model::spec();
    let dir = write_ready_ultra();
    crate::verify_receipt::record(&dir, &spec);
    let suite = store::paths::models_root().parent().unwrap().to_path_buf();
    let peer_lease = store::lease::try_acquire_at_roots(
        &[suite.as_path(), store::paths::legacy_suite_root().as_path()],
        "parakeet-tdt-v3",
        "peer-app",
    )
    .unwrap()
    .expect("the peer takes the lease first");

    // No record: a load would have to run, the lease is busy, the prepare is deferred. That says
    // nothing about the copy, so a record written later is still trusted.
    let deferred = prepare(&|_| panic!("the lease is busy: nothing may load"));
    assert!(deferred.unwrap_err().contains("in use"));
    write_activation_record(&dir);
    assert_eq!(
        resolve(subdir()),
        Some(dir.clone()),
        "contention is not a denial"
    );

    // Record present: no native load will run, so the busy lease does not matter.
    let activated = prepare(&|_| panic!("a recorded load must not run again"));
    assert_eq!(activated.unwrap(), Some(dir.clone()));
    assert!(is_verified_active());
    drop(peer_lease);

    // A record that cannot be written after a successful load is not an integrity failure.
    reset_activation_for_tests();
    std::fs::remove_file(record_file()).unwrap();
    std::fs::create_dir_all(record_file()).unwrap();
    let failure = prepare(&|_| Ok(()));
    assert!(failure
        .unwrap_err()
        .contains("record shared model activation"));
    std::fs::remove_dir_all(record_file()).unwrap();
    write_activation_record(&dir);
    assert_eq!(resolve(subdir()), Some(dir), "not denied");
}

/// The same rule one level down, on the store's own test spec: a recorded load takes no lease and
/// runs no native load; an unrecorded one needs both.
#[test]
fn recorded_loads_take_no_lease_and_unrecorded_ones_report_contention_as_non_integrity() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = isolate_env(tmp.path());
    let root = store::paths::models_root();
    let suite = root.parent().unwrap().to_path_buf();
    let spec = tests::spec();
    let dir = root.join(spec.id);
    tests::write_copy(&dir);
    store::manifest::write_manifest(&dir, &spec.expected_manifest()).unwrap();
    let _peer = store::lease::try_acquire_at_roots(&[suite.as_path()], "parakeet-tdt-v3", "peer")
        .unwrap()
        .expect("peer lease");

    let missing = tmp.path().join("no-source");
    let ready = prepare_variant_checked(
        &root,
        &missing,
        &spec,
        &|_| panic!("recorded: no native load"),
        &|_| true,
    )
    .map_err(|e| e.message);
    assert_eq!(ready, Ok(Some(dir)));

    let deferred = prepare_variant_checked(&root, &missing, &spec, &|_| Ok(()), &|_| false);
    let failure = deferred.expect_err("deferred behind the peer's lease");
    assert!(failure.message.contains("in use"));
    assert!(!failure.integrity);
}
