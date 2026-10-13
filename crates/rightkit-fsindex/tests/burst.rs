// Needs the macOS backend; other platforms use the Unsupported stub.
#![cfg(target_os = "macos")]

mod common;
use common::*;
use rightkit_fsindex::Index;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    // Drain while the burst is written. The subscriber queue is bounded (16): a batch that finds
    // it full is dropped and only the next one reports `missed_batches`, so a test that reads
    // after the writes loses the final rollup whenever the writes outlast ~16 windows (a slow CI
    // runner), and no later batch is ever emitted. A real consumer keeps reading; so does this.
    #[derive(Default)]
    struct Seen {
        batches: usize,
        max_listed: usize,
        max_rollups: usize,
        saw_final_root_rollup: bool,
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let collector = {
        let (seen, stop, root) = (seen.clone(), stop.clone(), t.root.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                while let Ok(b) = rx.recv_timeout(Duration::from_millis(25)) {
                    let mut s = seen.lock().unwrap();
                    s.batches += 1;
                    s.max_listed = s.max_listed.max(b.changes.len());
                    s.max_rollups = s.max_rollups.max(b.rollups.len());
                    assert!(b.changes.len() <= 256, "paths per batch are bounded");
                    if b.changes_truncated {
                        assert!(b.total_changes as usize > b.changes.len());
                    }
                    if b.rollups
                        .iter()
                        .any(|r| r.path == root && r.size.files == 10_001)
                    {
                        s.saw_final_root_rollup = true;
                    }
                }
            }
        })
    };
    let started = Instant::now();
    for d in 0..100 {
        std::fs::create_dir_all(t.root.join(format!("burst/d{d:03}"))).unwrap();
        for f in 0..100 {
            std::fs::write(t.root.join(format!("burst/d{d:03}/f{f:03}.dat")), [7u8; 64]).unwrap();
        }
    }
    let wrote = started.elapsed();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !seen.lock().unwrap().saw_final_root_rollup && Instant::now() < deadline {
        idx.flush();
        std::thread::sleep(Duration::from_millis(50));
    }
    // One more flush, then let the collector take what it emitted.
    idx.flush();
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::SeqCst);
    collector
        .join()
        .expect("collector saw only bounded batches");
    let s = seen.lock().unwrap();
    let (batches, max_listed, max_rollups) = (s.batches, s.max_listed, s.max_rollups);
    assert!(
        s.saw_final_root_rollup,
        "a batch must carry the final rollup of the root"
    );
    drop(s);
    assert_eq!(idx.folder_size(t.root.join("burst")).unwrap().files, 10_000);
    // Coalesced: far fewer batches than changes (150 ms window, writes took `wrote`).
    let max_batches = (wrote.as_millis() / 100) as usize + 40;
    assert!(batches <= max_batches, "{batches} batches for {wrote:?}");
    assert!(max_rollups <= 2048);
    eprintln!("burst: wrote 10k files in {wrote:?}; {batches} batches, max listed {max_listed}, max rollups {max_rollups}");
    assert_matches_fresh_crawl(&idx, &t.root);
}
