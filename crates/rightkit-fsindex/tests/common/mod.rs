#![allow(dead_code)]
use rightkit_fsindex::{Config, Index};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Tree {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
}

pub fn tree() -> Tree {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    Tree { dir, root }
}

pub fn config(root: &Path) -> Config {
    let mut c = Config::new(vec![root.to_path_buf()]);
    c.home_dirs = Some(Vec::new());
    c.watch_latency = Duration::from_millis(20);
    c.change_coalesce = Duration::from_millis(100);
    // The re-list gate has its own tests; the general suite keeps the ungated timing.
    c.relist_min_interval = Duration::ZERO;
    // Likewise the rescan cooldown: a kernel drop under parallel test load must repair at once.
    c.rescan_cooldown = Duration::ZERO;
    // And the relist budget: bursty test churn must relist at once.
    c.relist_budget = None;
    c
}

pub fn static_config(root: &Path) -> Config {
    let mut c = config(root);
    c.watch = false;
    c
}

pub fn write(p: &Path, bytes: usize) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    // Deterministic, incompressible-ish content so APFS allocates real blocks.
    let mut x = 0x9e3779b97f4a7c15u64;
    let data: Vec<u8> = (0..bytes)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(p, data).unwrap();
}

/// True when no subtree re-list is pending or running and no full rescan is wanted or
/// running. Call after `flush()`, which makes the worker apply everything delivered so far.
pub fn settled(idx: &Index) -> bool {
    let st = idx.status();
    st.relists_pending == 0 && !st.rescan_pending
}

/// Flush and poll until the index is settled or 20 s pass.
pub fn wait_settled(idx: &Index) {
    let t = Instant::now();
    loop {
        idx.flush();
        if settled(idx) {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "timed out waiting for pending relists or rescan"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll (flushing the change feed) until `f` holds and the index is settled, or 20 s pass.
pub fn wait_for(idx: &Index, what: &str, f: impl Fn(&Index) -> bool) {
    let t = Instant::now();
    loop {
        idx.flush();
        if f(idx) && settled(idx) {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "timed out waiting for: {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn dirs_under(idx: &Index, root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        let p = out[i].clone();
        for c in idx.children_sorted_by_size(&p).unwrap_or_default() {
            if c.kind == rightkit_fsindex::EntryKind::Dir {
                out.push(c.path);
            }
        }
        i += 1;
    }
    out
}

/// Every folder's rollup in `live` must equal a fresh full crawl of the same root.
pub fn assert_matches_fresh_crawl(live: &Index, root: &Path) {
    wait_settled(live);
    let fresh = Index::open(static_config(root)).unwrap();
    let a = dirs_under(live, root);
    let b = dirs_under(&fresh, root);
    assert_eq!(a.len(), b.len(), "folder sets differ");
    for d in a {
        let (x, y) = (
            live.folder_size(&d).unwrap(),
            fresh.folder_size(&d).unwrap(),
        );
        // mtime-independent: compare size fields only
        assert_eq!(
            (x.logical, x.allocated, x.allocated_unique, x.files),
            (y.logical, y.allocated, y.allocated_unique, y.files),
            "rollup mismatch at {}",
            d.display()
        );
    }
}

/// Hits for `query`, not counting a root. A root's name is its absolute path and search is a
/// fuzzy subsequence match, so a short query can match the random temp directory in that path
/// (seen in CI: "inside" matched a root and a count of one came back as two).
#[allow(dead_code)]
pub fn file_hits(idx: &rightkit_fsindex::Index, query: &str) -> usize {
    idx.search(query)
        .unwrap()
        .into_iter()
        .filter(|hit| !Path::new(&hit.item.name).is_absolute())
        .count()
}
