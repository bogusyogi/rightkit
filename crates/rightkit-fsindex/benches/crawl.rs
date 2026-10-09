//! Crawl benchmark on a temp tree: getattrlistbulk crawl (1..8 threads) versus a
//! plain `read_dir` + `metadata` walk. Run: `rightkit cargo bench -p rightkit-fsindex`.
//! Set FSINDEX_BENCH_DIRS / FSINDEX_BENCH_FILES_PER_DIR to resize (default 2000 x 25).

use rightkit_fsindex::{Config, Index};
use std::path::Path;
use std::time::Instant;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn walk_std(p: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                let (f, b) = walk_std(&e.path());
                files += f;
                bytes += b;
            } else {
                files += 1;
                bytes += m.len();
            }
        }
    }
    (files, bytes)
}

fn main() {
    let dirs = env("FSINDEX_BENCH_DIRS", 2000);
    let per = env("FSINDEX_BENCH_FILES_PER_DIR", 25);
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let t = Instant::now();
    for d in 0..dirs {
        let dir = root.join(format!("g{}/d{d}", d % 20));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..per {
            std::fs::write(dir.join(format!("file_{f}.dat")), [1u8; 100]).unwrap();
        }
    }
    println!(
        "built {} dirs / {} files in {:?}",
        dirs,
        dirs * per,
        t.elapsed()
    );

    let t = Instant::now();
    let (f, b) = walk_std(&root);
    println!(
        "std read_dir+metadata walk : {:>9.1?}  ({f} files, {b} bytes)",
        t.elapsed()
    );

    for threads in [1usize, 4] {
        let be = rightkit_fsindex::backend::default_backend();
        let t = Instant::now();
        let ls = be.crawl(&root, &Default::default(), threads);
        let n: usize = ls.iter().map(|l| l.ents.len()).sum();
        println!(
            "raw backend crawl, {threads} thread(s): {:>9.1?}  ({n} entries, no index build)",
            t.elapsed()
        );
    }
    for threads in [1usize, 2, 4, 8] {
        let mut cfg = Config::new(vec![root.clone()]);
        cfg.watch = false;
        cfg.home_dirs = Some(Vec::new());
        cfg.crawl_threads = threads;
        let t = Instant::now();
        let idx = Index::open(cfg).unwrap();
        let el = t.elapsed();
        let s = idx.folder_size(&root).unwrap();
        println!(
            "fsindex crawl, {threads} thread(s)   : {:>9.1?}  ({} files, {} bytes, {} entries)",
            el,
            s.files,
            s.logical,
            idx.status().entries
        );
    }
}
