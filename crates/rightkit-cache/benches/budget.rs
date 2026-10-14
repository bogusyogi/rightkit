//! Runtime budget for rightkit-cache (docs/RUNTIME-BUDGETS.md): a size-capped disk cache under churn.
//!
//! Load: open a cache capped at 32 MiB in a temp directory, put 100,000 distinct 512-byte entries
//! (the cap holds about 65,000, so about a third are evicted by the LRU), read 100,000 keys back
//! (hits on the newest, misses on the evicted), then hold the cache open and idle for the idle window.
//!
//! Run: `rightkit cargo bench -p rightkit-cache --bench budget`
//! Optional: `-- <entries> <idle_seconds>` (defaults 100000 and 30).

use rightkit_cache::{Config, DiskCache, Key};
use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let entries: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let idle_s: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);

    let tmp = tempfile::tempdir().unwrap();
    let before = process_usage().expect("process usage");
    let cache = DiskCache::open(tmp.path().join("cache"), Config::new(32 * 1024 * 1024)).unwrap();
    let payload = vec![0x5au8; 512];
    let started = Instant::now();
    for i in 0..entries {
        cache
            .put(&Key::new(&["bench", &i.to_string()]), &payload)
            .unwrap();
    }
    let put_s = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let mut hits = 0u64;
    for i in 0..entries {
        if cache.get(&Key::new(&["bench", &i.to_string()])).is_some() {
            hits += 1;
        }
    }
    let get_s = started.elapsed().as_secs_f64();
    let status = cache.status();
    let warm = process_usage().expect("process usage");
    println!(
        "warm-up: {entries} puts in {put_s:.1} s, {entries} gets in {get_s:.1} s ({hits} hits), resident entries {}, bytes {}, evictions {}, resident {:.1} MiB (before open {:.1} MiB)",
        status.stats.entries,
        status.stats.bytes,
        status.stats.evictions,
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    println!("RESULT cache_entries={} entries", status.stats.entries);
    println!("RESULT cache_evictions={} entries", status.stats.evictions);
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-cache",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "entries",
                scale: entries,
                what: "512-byte entries put and read through a 32 MiB cap, then idle",
                scope: "synthetic"
            }
        )
    );
    drop(cache);
}
