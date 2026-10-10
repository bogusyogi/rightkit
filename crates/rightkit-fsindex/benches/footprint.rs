//! Resident-memory benchmark for the column store (no real crawl).
//!
//! Generates N entries (default 2,000,000; pass a count after `--` or set FSINDEX_SYNTH_ENTRIES) directly into the
//! memory-mapped columns, commits, then measures a *fresh child process* that reopens the
//! store: phys_footprint / resident size before and after a name search and a size query.
//! Run: `rightkit cargo bench -p rightkit-fsindex --bench footprint`.

#[cfg(target_os = "macos")]
mod mac {
    use rightkit_fsindex::{Config, Index};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    #[repr(C)]
    struct TaskVmInfo {
        words: [u64; 40],
    }

    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: u32, out: *mut u64, count: *mut u32) -> i32;
    }

    /// (phys_footprint, resident_size) in bytes.
    fn footprint() -> (u64, u64) {
        let mut info = TaskVmInfo { words: [0; 40] };
        let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
        // TASK_VM_INFO = 22; resident_size is at byte 16, phys_footprint at byte 144.
        let rc = unsafe { task_info(mach_task_self(), 22, info.words.as_mut_ptr(), &mut count) };
        assert_eq!(rc, 0, "task_info failed");
        (info.words[18], info.words[2])
    }

    fn mb(b: u64) -> f64 {
        b as f64 / (1024.0 * 1024.0)
    }

    fn config(state: &Path) -> Config {
        let mut c = Config::new(vec![PathBuf::from("/synthetic")]);
        c.state_dir = Some(state.to_path_buf());
        c.watch = false;
        c.home_dirs = Some(Vec::new());
        c.snapshot_max_age = std::time::Duration::from_secs(10 * 365 * 24 * 3600);
        c
    }

    fn dir_size(p: &Path) -> u64 {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        e.metadata().map_or(0, |m| {
                            if m.is_dir() {
                                dir_size(&e.path())
                            } else {
                                std::os::unix::fs::MetadataExt::blocks(&m) * 512
                            }
                        })
                    })
                    .sum()
            })
            .unwrap_or(0)
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().collect();
        if let Some(i) = args.iter().position(|a| a == "--child") {
            let state = PathBuf::from(&args[i + 1]);
            let n: usize = args[i + 2].parse().unwrap();
            let (f0, r0) = footprint();
            let t = Instant::now();
            let idx = Index::open(config(&state)).unwrap();
            let open = t.elapsed();
            println!("aux (link groups, clone groups, clone members): {:?}; resident right after open {:.1} MB", idx.aux_counts(), mb(footprint().1));
            assert!(
                idx.status().resumed_from_snapshot,
                "child must reopen, not rebuild"
            );
            assert_eq!(idx.status().entries, n);
            let (f1, r1) = footprint();
            let t = Instant::now();
            let hits = idx.search("file1a3 png").unwrap();
            let search = t.elapsed();
            let (f2, r2) = footprint();
            let t = Instant::now();
            let size = idx.folder_size("/synthetic").unwrap();
            let kids = idx.children_sorted_by_size("/synthetic").unwrap();
            let sized = t.elapsed();
            let (f3, r3) = footprint();
            idx.trim();
            let (f4, r4) = footprint();
            println!(
                "child: process start        footprint {:7.1} MB  resident {:7.1} MB",
                mb(f0),
                mb(r0)
            );
            println!(
                "child: after reopen ({open:.1?})  footprint {:7.1} MB  resident {:7.1} MB",
                mb(f1),
                mb(r1)
            );
            println!("child: after search ({search:.1?}, {} hits)  footprint {:7.1} MB  resident {:7.1} MB", hits.len(), mb(f2), mb(r2));
            println!("child: after size query ({sized:.1?}, {} kids, unique {} MB)  footprint {:7.1} MB  resident {:7.1} MB", kids.len(), size.allocated_unique >> 20, mb(f3), mb(r3));
            println!(
                "child: after trim()          footprint {:7.1} MB  resident {:7.1} MB",
                mb(f4),
                mb(r4)
            );
            println!("RESULT bytes/entry footprint {:.2}  resident {:.2} (after search+size query); after trim: footprint {:.2} resident {:.2}", (f3 - f0) as f64 / n as f64, (r3 - r0) as f64 / n as f64, f4.saturating_sub(f0) as f64 / n as f64, r4.saturating_sub(r0) as f64 / n as f64);
            return;
        }
        let n: usize = std::env::var("FSINDEX_SYNTH_ENTRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| args.iter().skip(1).find_map(|a| a.parse().ok()))
            .unwrap_or(2_000_000);
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().to_path_buf();
        let t = Instant::now();
        {
            let idx = Index::open_synthetic(config(&state), n).unwrap();
            assert_eq!(idx.status().entries, n);
            println!("generated {n} entries into columns in {:?}", t.elapsed());
        }
        let disk = dir_size(&state);
        println!(
            "on disk (allocated): {:.1} MB = {:.1} bytes/entry",
            mb(disk),
            disk as f64 / n as f64
        );
        let exe = std::env::current_exe().unwrap();
        let st = std::process::Command::new(exe)
            .arg("--child")
            .arg(&state)
            .arg(n.to_string())
            .status()
            .unwrap();
        assert!(st.success());
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
