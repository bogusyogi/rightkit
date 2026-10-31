//! Per-folder re-list gate under a sustained burst. macOS only (FSEvents).
#![cfg(target_os = "macos")]

mod common;
use common::*;
use rightkit_fsindex::Index;
use std::time::{Duration, Instant};

#[test]
fn a_burst_on_one_folder_is_relisted_a_bounded_number_of_times() {
    let t = tree();
    write(&t.root.join("busy/seed.txt"), 1);
    let mut cfg = config(&t.root);
    cfg.relist_min_interval = Duration::from_millis(500);
    cfg.relist_max_interval = Duration::from_secs(2);
    let idx = Index::open(cfg).unwrap();
    let before = idx.status().relists_total;

    // One new file every 20 ms for 4 s: about 200 changes in one folder.
    let started = Instant::now();
    let mut written = 0usize;
    while started.elapsed() < Duration::from_secs(4) {
        write(&t.root.join(format!("busy/f{written:04}.dat")), 16);
        written += 1;
        std::thread::sleep(Duration::from_millis(20));
    }

    let expected = written as u64 + 1; // the seed file too
    wait_for(&idx, "every burst file indexed", |x| {
        x.folder_size(t.root.join("busy"))
            .is_some_and(|s| s.files == expected)
    });

    let st = idx.status();
    let relists = st.relists_total - before;
    assert!(relists >= 1, "the folder was re-listed");
    assert!(
        relists <= 10,
        "a sustained burst must be re-listed a bounded number of times, got {relists} for {written} changes"
    );
    assert!(st.relist_rate_per_s >= 0.0);
}

#[test]
fn excluded_folders_never_enter_the_gate_and_status_reports_the_gate() {
    let t = tree();
    write(&t.root.join("src/seed.txt"), 1);
    write(&t.root.join("target/debug/seed.o"), 1);
    // `config()` sets `relist_min_interval` to zero, which disables the gate: it then tracks no
    // folder state at all, so `gate_entries` would read 0 whatever was excluded and this test
    // would prove nothing. Enable the gate, as the burst test above does.
    let mut cfg = config(&t.root);
    cfg.relist_min_interval = Duration::from_millis(500);
    cfg.relist_max_interval = Duration::from_secs(2);
    let idx = Index::open(cfg).unwrap();

    // 40 distinct folders under the default-excluded `target`, then one change in `src`.
    for i in 0..40 {
        write(&t.root.join(format!("target/debug/d{i}/f.o")), 8);
    }
    write(&t.root.join("src/new.txt"), 8);
    wait_for(&idx, "src change indexed", |x| {
        x.folder_size(t.root.join("src"))
            .is_some_and(|s| s.files == 2)
    });
    std::thread::sleep(Duration::from_millis(500));

    let st = idx.status();
    // Only `src` (and possibly its parent) can have been gated; none of the 40 excluded ones.
    assert!(st.gate_entries >= 1, "gate_entries={}", st.gate_entries);
    assert!(st.gate_entries <= 3, "gate_entries={}", st.gate_entries);
    assert!(st.backoff_folders <= st.gate_entries);
}
