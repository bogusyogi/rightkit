//! Runtime budget measurement for rightkit-fsindex (macOS). Prints `RESULT` lines that the
//! README "Runtime budget" section quotes.
//!
//! Two runs, both in fresh child processes with the watch on:
//! * `mem`: a synthetic index of N entries (default 1,000,000) is generated, then reopened
//!   from its snapshot. Reports resident memory split into mapped and heap, process anonymous
//!   and file-backed memory, after open, after an idle period, and after `shrink()`.
//! * `churn`: a real tree of F files (default 50,000) is crawled, then a build-like folder
//!   set changes at 400 changes per second for 60 s. Reports process CPU, directory reads and
//!   re-lists per second, and how long the index takes to converge after the churn stops.
//!
//! Run: `rightkit cargo bench -p rightkit-fsindex --bench budget` (both), or
//! `... --bench budget -- mem 1000000` / `-- churn 50000` for one.

#[cfg(target_os = "macos")]
mod mac {
    use rightkit_fsindex::{Config, EntryKind, Index, SortKey, Status};
    use std::collections::{BTreeSet, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct TaskVmInfo {
        words: [u64; 40],
    }

    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: u32, out: *mut u64, count: *mut u32) -> i32;
    }

    #[derive(Clone, Copy, Debug)]
    struct Proc {
        footprint: u64,
        internal: u64,
        external: u64,
    }

    /// TASK_VM_INFO: `internal` (anonymous) at byte 48, `external` (file-backed) at byte 64,
    /// `phys_footprint` at byte 144 (offsets from the SDK's mach/task_info.h).
    fn proc_mem() -> Proc {
        let mut info = TaskVmInfo { words: [0; 40] };
        let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
        let rc = unsafe { task_info(mach_task_self(), 22, info.words.as_mut_ptr(), &mut count) };
        assert_eq!(rc, 0, "task_info failed");
        Proc {
            footprint: info.words[18],
            internal: info.words[6],
            external: info.words[8],
        }
    }

    fn cpu_seconds() -> f64 {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        let s = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        s(ru.ru_utime) + s(ru.ru_stime)
    }

    fn mb(b: u64) -> f64 {
        b as f64 / (1024.0 * 1024.0)
    }

    fn config(root: &Path, state: Option<&Path>) -> Config {
        let mut c = Config::new(vec![root.to_path_buf()]);
        c.state_dir = state.map(Path::to_path_buf);
        c.watch = true;
        c.home_dirs = Some(Vec::new());
        c.snapshot_max_age = Duration::from_secs(10 * 365 * 24 * 3600);
        c
    }

    fn count_files(dir: &Path) -> u64 {
        fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        let p = e.path();
                        if p.is_dir() {
                            count_files(&p)
                        } else {
                            1
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }

    fn report_status(label: &str, st: &Status, p: Proc) {
        println!(
            "{label}: mapped {:7.1} MB  heap {:6.1} MB  per entry {:6.1} B  | process anon {:7.1} MB  file {:7.1} MB  footprint {:7.1} MB",
            mb(st.resident_bytes.mapped),
            mb(st.resident_bytes.heap),
            st.resident_per_entry,
            mb(p.internal),
            mb(p.external),
            mb(p.footprint),
        );
    }

    /// Child: reopen the synthetic snapshot with the watch on and measure.
    fn mem_child(state: &Path, root: &Path, n: usize) {
        let start = proc_mem();
        let idx = Index::open(config(root, Some(state))).unwrap();
        let st = idx.status();
        assert!(st.resumed_from_snapshot, "child must reopen the snapshot");
        assert_eq!(st.entries, n);
        report_status("open (watch on)", &st, proc_mem());
        println!(
            "start: process anon {:.1} MB, file {:.1} MB",
            mb(start.internal),
            mb(start.external)
        );

        let warm = rightkit_child_process::process_usage().expect("process usage");
        let idle_started = Instant::now();
        std::thread::sleep(Duration::from_secs(35));
        let idle_end = rightkit_child_process::process_usage().expect("process usage");
        let idle_elapsed = idle_started.elapsed();
        let idle = rightkit_child_process::IdleObservation {
            elapsed: idle_elapsed,
            start: warm,
            end: idle_end,
            rates: idle_end.rates_since(&warm, idle_elapsed),
        };
        let steady = idx.status();
        report_status("steady after 35 s idle", &steady, proc_mem());

        let hits = idx.search("file1a3 png").unwrap();
        let size = idx.folder_size(root).unwrap();
        let kids = idx.children_sorted_by_size(root).unwrap();
        let after_query = idx.status();
        report_status("after search + size query", &after_query, proc_mem());
        println!(
            "query check: {} hits, {} root children, unique {} MB",
            hits.len(),
            kids.len(),
            size.allocated_unique >> 20
        );

        // Budget figures for the gate: resident is the larger of the opened and the idle-end sets;
        // CPU, wake-ups and disk rates are the 35 s idle window with the watch on.
        print!(
            "{}",
            idle.result_lines(
                &warm,
                &rightkit_child_process::BudgetLabels {
                    krate: "rightkit-fsindex",
                    version: env!("CARGO_PKG_VERSION"),
                    scale_unit: "entries",
                    scale: n as u64,
                    what: "synthetic index reopened from its snapshot with the FSEvents watch on, then idle 35 s",
                    scope: "synthetic",
                }
            )
        );
        idx.shrink();
        let shrunk = idx.status();
        report_status("after shrink()", &shrunk, proc_mem());
        println!(
            "RESULT mem entries={n} open_mapped_mb={:.1} open_heap_mb={:.2} open_per_entry_b={:.2} steady_mapped_mb={:.1} steady_per_entry_b={:.2} shrink_mapped_mb={:.1} shrink_per_entry_b={:.2} proc_anon_mb={:.1} proc_file_mb={:.1} proc_footprint_mb={:.1}",
            mb(st.resident_bytes.mapped),
            mb(st.resident_bytes.heap),
            st.resident_per_entry,
            mb(steady.resident_bytes.mapped),
            steady.resident_per_entry,
            mb(shrunk.resident_bytes.mapped),
            shrunk.resident_per_entry,
            mb(proc_mem().internal),
            mb(proc_mem().external),
            mb(proc_mem().footprint),
        );
    }

    /// Parent: generate the synthetic store, then reopen it in a child.
    fn mem_parent(n: usize) {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let root = tmp.path().join("root");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&root).unwrap();
        let t = Instant::now();
        {
            let mut c = config(&root, Some(&state));
            c.watch = false;
            let idx = Index::open_synthetic(c, n).unwrap();
            assert_eq!(idx.status().entries, n);
        }
        println!("generated {n} entries in {:.1?}", t.elapsed());
        let exe = std::env::current_exe().unwrap();
        let st = Command::new(exe)
            .arg("--mem-child")
            .arg(&state)
            .arg(&root)
            .arg(n.to_string())
            .status()
            .unwrap();
        assert!(st.success(), "mem child failed");
    }

    /// Ground truth: a plain walk of the disk. Records every file path and every directory's
    /// logical byte sum (what `folder_size().logical` must equal for a tree without links).
    fn walk_disk(
        dir: &Path,
        files: &mut BTreeSet<PathBuf>,
        sums: &mut HashMap<PathBuf, u64>,
    ) -> u64 {
        let mut sum = 0;
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                sum += walk_disk(&p, files, sums);
            } else {
                sum += m.len();
                files.insert(p);
            }
        }
        sums.insert(dir.to_path_buf(), sum);
        sum
    }

    /// The index's own view of the tree, through its public listing API.
    fn walk_index(idx: &Index, dir: &Path, files: &mut BTreeSet<PathBuf>) {
        for it in idx.children_sorted_by(dir, SortKey::Name).unwrap() {
            let p = dir.join(&it.name);
            match it.kind {
                EntryKind::Dir => walk_index(idx, &p, files),
                EntryKind::File => {
                    files.insert(p);
                }
                _ => {}
            }
        }
    }

    /// Correctness beside the performance figures: entry recall and precision against the disk,
    /// and per-folder logical size against the disk walk (exact for a tree without links).
    fn audit(idx: &Index, root: &Path) {
        let mut disk_files = BTreeSet::new();
        let mut sums = HashMap::new();
        walk_disk(root, &mut disk_files, &mut sums);
        let mut index_files = BTreeSet::new();
        walk_index(idx, root, &mut index_files);
        let missing = disk_files.difference(&index_files).count();
        let extra = index_files.difference(&disk_files).count();
        let mut bad_dirs = 0usize;
        let mut worst: Option<(PathBuf, u64, u64)> = None;
        for (dir, want) in &sums {
            let got = idx.folder_size(dir).map_or(0, |s| s.logical);
            if got != *want {
                bad_dirs += 1;
                if worst.is_none() {
                    worst = Some((dir.clone(), *want, got));
                }
            }
        }
        println!(
            "RESULT churn_audit files_disk={} files_index={} missing={missing} extra={extra} recall_pct={:.3} precision_pct={:.3} folders_checked={} logical_mismatch_folders={bad_dirs}",
            disk_files.len(),
            index_files.len(),
            100.0 * (disk_files.len() - missing) as f64 / disk_files.len().max(1) as f64,
            100.0 * (index_files.len() - extra) as f64 / index_files.len().max(1) as f64,
            sums.len(),
        );
        if let Some((dir, want, got)) = worst {
            println!(
                "audit first mismatch: {} disk {want} index {got}",
                dir.display()
            );
        }
    }

    /// Child: crawl a real tree, then churn a build-like folder set and measure.
    fn churn_child(root: &Path, files: u64) {
        let out = root.join("build_out");
        let t = Instant::now();
        let idx = Index::open(config(root, None)).unwrap();
        println!("crawl {} files in {:.1?}", files, t.elapsed());
        report_status("open (watch on)", &idx.status(), proc_mem());

        const SECS: u64 = 60;
        const PER_SEC: u64 = 200;
        const DIRS: u64 = 20;
        const KEEP: u64 = 10;
        let s0 = idx.status();
        let cpu0 = cpu_seconds();
        let start = Instant::now();
        for sec in 0..SECS {
            let tick = start + Duration::from_secs(sec);
            if let Some(wait) = tick.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            for i in 0..PER_SEC {
                let p = out
                    .join(format!("d{:02}", i % DIRS))
                    .join(format!("s{sec}_{i}.o"));
                fs::write(p, [7u8; 16]).unwrap();
            }
            if sec >= KEEP {
                for i in 0..PER_SEC {
                    let p = out
                        .join(format!("d{:02}", i % DIRS))
                        .join(format!("s{}_{i}.o", sec - KEEP));
                    let _ = fs::remove_file(p);
                }
            }
        }
        let wall = start.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - cpu0;
        let s1 = idx.status();
        // Creates every second, plus deletes of the files written KEEP seconds earlier.
        let changes = (SECS * PER_SEC + (SECS - KEEP) * PER_SEC) as f64;
        println!(
            "RESULT churn changes_per_s={:.0} cpu_percent={:.1} dir_reads_per_s={:.1} relists_per_s={:.2} backoff_folders_max={} wall_s={:.1}",
            changes / wall,
            cpu / wall * 100.0,
            (s1.dir_reads_total - s0.dir_reads_total) as f64 / wall,
            (s1.relists_total - s0.relists_total) as f64 / wall,
            s1.backoff_folders,
            wall,
        );

        // Convergence: once the churn stops, the index must match the disk again.
        let disk = count_files(&out);
        let stop = Instant::now();
        loop {
            let got = idx.folder_size(&out).map_or(0, |s| s.files);
            if got == disk {
                break;
            }
            assert!(
                stop.elapsed() < Duration::from_secs(180),
                "index did not converge: {got} != {disk}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        println!(
            "RESULT churn_convergence_s={:.2} files_after={disk}",
            stop.elapsed().as_secs_f64()
        );
        audit(&idx, root);
        let end = idx.status();
        report_status("after churn", &end, proc_mem());
        println!(
            "RESULT churn_end mapped_mb={:.1} heap_mb={:.2} per_entry_b={:.2} entries={}",
            mb(end.resident_bytes.mapped),
            mb(end.resident_bytes.heap),
            end.resident_per_entry,
            end.entries,
        );
    }

    /// Parent: build the real tree (plus the churned folders), then run the churn child.
    fn churn_parent(files: u64) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tree");
        fs::create_dir_all(&root).unwrap();
        let t = Instant::now();
        let dirs = (files / 50).max(1);
        for d in 0..dirs {
            let dir = root.join(format!("pkg{d:04}"));
            fs::create_dir_all(&dir).unwrap();
            for f in 0..50 {
                fs::write(dir.join(format!("m{f:02}.rs")), [b'x'; 64]).unwrap();
            }
        }
        for d in 0..20 {
            fs::create_dir_all(root.join("build_out").join(format!("d{d:02}"))).unwrap();
        }
        println!("built {} files in {:.1?}", dirs * 50, t.elapsed());
        let exe = std::env::current_exe().unwrap();
        let st = Command::new(exe)
            .arg("--churn-child")
            .arg(&root)
            .arg((dirs * 50).to_string())
            .status()
            .unwrap();
        assert!(st.success(), "churn child failed");
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("--mem-child") => {
                let n: usize = args[4].parse().unwrap();
                mem_child(Path::new(&args[2]), Path::new(&args[3]), n);
            }
            Some("--churn-child") => {
                let files: u64 = args[3].parse().unwrap();
                churn_child(Path::new(&args[2]), files);
            }
            Some("churn") => {
                let f: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(50_000);
                churn_parent(f);
            }
            Some("mem") => {
                let n: usize = args
                    .get(2)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1_000_000);
                mem_parent(n);
            }
            _ => {
                mem_parent(1_000_000);
                churn_parent(50_000);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    mac::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only");
}
