//! A `service.json` that cannot be read is "unknown, try again", never "no service". A directory
//! at the path makes the read fail with an error that is not `NotFound` (`IsADirectory` on Unix,
//! `PermissionDenied` on Windows) on every platform.

use rightkit_daemon::*;
use std::collections::BTreeMap;
use std::path::Path;

fn suite_with_unreadable_record() -> (tempfile::TempDir, Suite) {
    let d = tempfile::tempdir().unwrap();
    let suite = Suite::at(d.path().join("suite")).unwrap();
    std::fs::create_dir_all(record_path(&suite, "demo")).unwrap();
    (d, suite)
}

#[test]
fn an_absent_record_is_not_an_error() {
    let d = tempfile::tempdir().unwrap();
    let suite = Suite::at(d.path().join("suite")).unwrap();
    assert!(try_adopt_or_spawn(&suite, "demo").unwrap() == AdoptOrSpawn::Spawn);
    let plan = try_plan_host(&suite, "demo", 0, &BTreeMap::new(), None).unwrap();
    assert_eq!(plan, HostPlan::HostMyself);
    let err = Client::connect(&suite, "demo", "alpha").err().unwrap();
    assert!(err.retryable);
    assert!(err.message.contains("no service.json"), "{}", err.message);
}

#[test]
fn a_client_reports_an_unreadable_record_as_retryable_and_says_so() {
    let (_d, suite) = suite_with_unreadable_record();
    let err = Client::connect(&suite, "demo", "alpha").err().unwrap();
    assert_eq!(err.code, ERROR_UNAVAILABLE);
    assert!(err.retryable);
    assert!(
        err.message.contains("could not read service.json"),
        "{}",
        err.message
    );
}

#[test]
fn discovery_helpers_do_not_conclude_there_is_no_host() {
    let (_d, suite) = suite_with_unreadable_record();
    let e = try_adopt_or_spawn(&suite, "demo").unwrap_err();
    assert_ne!(e.kind(), std::io::ErrorKind::NotFound);
    assert!(try_plan_host(&suite, "demo", 0, &BTreeMap::new(), None).is_err());
    // The infallible forms keep their old answer (the enum cannot say "unknown").
    assert_eq!(adopt_or_spawn(&suite, "demo"), AdoptOrSpawn::Spawn);
    assert_eq!(
        plan_host(
            &suite,
            "demo",
            0,
            &BTreeMap::new(),
            Some(Path::new("/nonexistent"))
        ),
        HostPlan::HostMyself
    );
    // Waiting for a winner keeps waiting through an unreadable record and times out.
    assert!(wait_for_claim_winner(&suite, "demo", std::time::Duration::from_millis(100)).is_none());
}
