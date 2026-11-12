//! One dump that cannot be copied is skipped and reported; the rest are admitted.

use rightkit_crash::admit_crash_artifacts;

#[test]
fn an_uncopyable_dump_is_skipped_and_reported() {
    let base = std::env::temp_dir().join(format!("rk-admit-skip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let crash = base.join("crash");
    let staging = base.join("staging");
    std::fs::create_dir_all(&crash).unwrap();
    std::fs::write(crash.join("dump_a.dmp"), b"AAAA").unwrap();
    std::fs::write(crash.join("dump_b.dmp"), b"BBBB").unwrap();
    // A directory where dump_a.dmp must go: the copy cannot succeed.
    std::fs::create_dir_all(staging.join("dump_a.dmp")).unwrap();

    let admitted = admit_crash_artifacts(&crash, &staging, true).expect("call survives");
    assert_eq!(admitted.skipped, vec!["dump_a.dmp".to_string()]);
    assert_eq!(admitted.native, vec!["dump_b.dmp".to_string()]);
    assert_eq!(std::fs::read(staging.join("dump_b.dmp")).unwrap(), b"BBBB");
    let _ = std::fs::remove_dir_all(&base);
}
