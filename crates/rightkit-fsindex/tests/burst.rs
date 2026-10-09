// Needs the macOS backend; other platforms use the Unsupported stub.
#![cfg(target_os = "macos")]

mod common;
use common::*;
use rightkit_fsindex::Index;
use std::time::{Duration, Instant};

/// ~10k changes in a burst: batches are coalesced and bounded, rollups for
/// affected ancestors arrive with them, and nothing re-walks the tree.
#[test]
fn burst_of_10k_changes_is_coalesced_and_bounded() {
    let t = tree();
    write(&t.root.join("seed/seed.txt"), 1);
    let mut cfg = config(&t.root);
    cfg.max_change_paths = 256;
    let idx = Index::open(cfg).unwrap();
    let rx = idx.subscribe();
    let started = Instant::now();
    for d in 0..100 {
        std::fs::create_dir_all(t.root.join(format!("burst/d{d:03}"))).unwrap();
        for f in 0..100 {
            std::fs::write(t.root.join(format!("burst/d{d:03}/f{f:03}.dat")), [7u8; 64]).unwrap();
        }
    }
    let wrote = started.elapsed();
    let mut batches = 0usize;
    let mut max_listed = 0usize;
    let mut max_rollups = 0usize;
    let mut saw_final_root_rollup = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !saw_final_root_rollup && Instant::now() < deadline {
        idx.flush();
        while let Ok(b) = rx.recv_timeout(Duration::from_millis(250)) {
            batches += 1;
            max_listed = max_listed.max(b.changes.len());
            max_rollups = max_rollups.max(b.rollups.len());
            assert!(b.changes.len() <= 256, "paths per batch are bounded");
            if b.changes_truncated {
                assert!(b.total_changes as usize > b.changes.len());
            }
            if b.rollups
                .iter()
                .any(|r| r.path == t.root && r.size.files == 10_001)
            {
                saw_final_root_rollup = true;
            }
        }
    }
    assert!(
        saw_final_root_rollup,
        "a batch must carry the final rollup of the root"
    );
    assert_eq!(idx.folder_size(t.root.join("burst")).unwrap().files, 10_000);
    // Coalesced: far fewer batches than changes (150 ms window, writes took `wrote`).
    let max_batches = (wrote.as_millis() / 100) as usize + 40;
    assert!(batches <= max_batches, "{batches} batches for {wrote:?}");
    assert!(max_rollups <= 2048);
    eprintln!("burst: wrote 10k files in {wrote:?}; {batches} batches, max listed {max_listed}, max rollups {max_rollups}");
    assert_matches_fresh_crawl(&idx, &t.root);
}
