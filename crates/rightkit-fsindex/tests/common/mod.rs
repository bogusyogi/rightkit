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

/// Poll (flushing the change feed) until `f` holds or 20 s pass.
pub fn wait_for(idx: &Index, what: &str, f: impl Fn(&Index) -> bool) {
    let t = Instant::now();
    loop {
        idx.flush();
        if f(idx) {
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
