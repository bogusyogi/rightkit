//! rightkit-fsindex: index a scratch tree (never the disk), run fuzzy name search, and check the size accounting on files
//! this probe creates. macOS (FSEvents backend): hard-link counting, APFS clone counting and the live change feed.
//! Windows: the backend is Windows Search, which only sees folders the service indexes and has no change feed or
//! hard-link/clone accounting, so the probe needs the scratch folder to be indexed.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_fsindex::{Config, EntryKind, Hit, Index, SearchOptions};
use serde_json::{json, Value};

fn write_bytes(path: &Path, len: usize) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
    }
    // Incompressible-ish content so the filesystem allocates real blocks.
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let data: Vec<u8> = (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(path, data).map_err(|e| format!("write {}: {e}", path.display()))
}

fn hit_paths(hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|h| h.item.path.display().to_string()).collect()
}

fn has_name(hits: &[Hit], name: &str) -> bool {
    hits.iter().any(|h| h.item.name.to_string_lossy() == name)
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let root: PathBuf = std::fs::canonicalize(ctx.dir("fsindex")?).map_err(|e| e.to_string())?;
    write_bytes(&root.join("docs/quarterly_invoice_2024.txt"), 4_000)?;
    write_bytes(&root.join("docs/invoice_draft.txt"), 1_000)?;
    write_bytes(&root.join("docs/meeting_notes.md"), 500)?;
    write_bytes(&root.join("photos/holiday_beach.jpg"), 30_000)?;

    #[cfg(windows)]
    {
        let status = rightkit_fsindex::windows_search_status(Some(&root));
        if !status.available() {
            return Err(format!(
                "needs the Windows Search service and the Search.CollatorDSO provider ({:?})",
                status.reason
            ));
        }
        if status.scope_indexed != Some(true) {
            return Err(format!(
                "needs {} to be inside a Windows Search indexed scope (the engine data root is not indexed)",
                root.display()
            ));
        }
    }

    let mut cfg = Config::new(vec![root.clone()]);
    cfg.home_dirs = Some(Vec::new());
    cfg.include_build_output = true; // keep the default scratch-root exclusion out of this run
    cfg.watch_latency = Duration::from_millis(50);
    cfg.change_coalesce = Duration::from_millis(150);
    cfg.relist_min_interval = Duration::ZERO;
    cfg.rescan_cooldown = Duration::ZERO;
    cfg.relist_budget = None;
    #[cfg(windows)]
    {
        cfg.watch = false;
    }
    let index = Index::open(cfg).map_err(|e| format!("Index::open({}): {e}", root.display()))?;

    let status = index.status();
    if status.entries < 6 {
        return Err(format!(
            "the crawl indexed {} entries of a tree with 4 files and 2 folders (unreadable: {:?})",
            status.entries, status.unreadable_dirs
        ));
    }

    // Fuzzy name search, scoped to this tree.
    let opts = SearchOptions { limit: 20, scope: Some(root.clone()) };
    let hits = index.search_with("quarterly invoice", &opts).map_err(|e| format!("search: {e}"))?;
    if hits.first().map(|h| h.item.name.to_string_lossy().into_owned()) != Some("quarterly_invoice_2024.txt".into()) {
        return Err(format!("the best hit for 'quarterly invoice' was not quarterly_invoice_2024.txt: {:?}", hit_paths(&hits)));
    }
    let loose = index.search_with("invoice", &opts).map_err(|e| format!("search: {e}"))?;
    if !has_name(&loose, "quarterly_invoice_2024.txt") || !has_name(&loose, "invoice_draft.txt") || has_name(&loose, "holiday_beach.jpg") {
        return Err(format!("'invoice' did not select exactly the invoice files: {:?}", hit_paths(&loose)));
    }
    let miss = index.search_with("zzqqxxnothing", &opts).map_err(|e| format!("search: {e}"))?;
    if !miss.is_empty() {
        return Err(format!("a nonsense query matched: {:?}", hit_paths(&miss)));
    }
    let beach = index.search_with("beach", &opts).map_err(|e| format!("search: {e}"))?;
    let beach_item = beach.iter().find(|h| h.item.name.to_string_lossy() == "holiday_beach.jpg").ok_or("holiday_beach.jpg not found")?;
    if beach_item.item.kind != EntryKind::File || beach_item.item.size.logical != 30_000 {
        return Err(format!("holiday_beach.jpg indexed as {:?} with {} logical bytes, expected File/30000", beach_item.item.kind, beach_item.item.size.logical));
    }
    let docs = index.folder_size(root.join("docs")).ok_or("folder_size(docs) is None")?;
    if docs.logical != 5_500 || docs.files != 3 {
        return Err(format!("docs rollup is {}B in {} files, expected 5500B in 3", docs.logical, docs.files));
    }

    #[allow(unused_mut)]
    let mut detail = json!({
        "backend": status.backend, "entries": status.entries,
        "best_hit": hit_paths(&hits).first().cloned(), "invoice_hits": loose.len(),
        "docs_logical": docs.logical,
    });

    #[cfg(target_os = "macos")]
    {
        // Hard links: two names for one inode count once.
        write_bytes(&root.join("links/orig.bin"), 200_000)?;
        std::fs::hard_link(root.join("links/orig.bin"), root.join("links/second.bin")).map_err(|e| format!("hard_link: {e}"))?;
        // APFS clones: copy-on-write copies share one extent.
        let clone_dir = root.join("clones");
        write_bytes(&clone_dir.join("base.bin"), 4 << 20)?;
        for n in ["c1.bin", "c2.bin"] {
            let out = std::process::Command::new("/bin/cp")
                .arg("-c")
                .arg(clone_dir.join("base.bin"))
                .arg(clone_dir.join(n))
                .output()
                .map_err(|e| format!("spawn cp -c: {e}"))?;
            if !out.status.success() {
                return Err(format!("needs an APFS data root (cp -c failed: {})", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        // The index is live: everything created after open must appear through the change feed.
        let rx = index.subscribe();
        write_bytes(&root.join("docs/late_arrival_report.txt"), 777)?;
        let start = Instant::now();
        let mut seen = false;
        while start.elapsed() < Duration::from_secs(20) {
            index.flush();
            let h = index.search_with("late_arrival", &opts).map_err(|e| e.to_string())?;
            let s = index.status();
            if has_name(&h, "late_arrival_report.txt") && s.relists_pending == 0 && !s.rescan_pending && has_name(&index.search_with("c2.bin", &opts).map_err(|e| e.to_string())?, "c2.bin") {
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !seen {
            return Err("the live change feed did not index late_arrival_report.txt and the clones within 20s".into());
        }
        let mut batches = 0usize;
        while rx.try_recv().is_ok() {
            batches += 1;
        }
        if batches == 0 {
            return Err("subscribe() delivered no change batch although the index changed live".into());
        }

        let links = index.folder_size(root.join("links")).ok_or("folder_size(links) is None")?;
        if links.logical != 200_000 || links.files != 1 {
            return Err(format!("two hard links of one 200000B file rolled up to {}B in {} files, expected 200000B in 1", links.logical, links.files));
        }
        let secondary = ["orig.bin", "second.bin"]
            .iter()
            .filter(|n| {
                index
                    .search_with(n, &opts)
                    .map(|h| h.iter().any(|x| x.item.name.to_string_lossy() == **n && x.item.is_secondary_link))
                    .unwrap_or(false)
            })
            .count();
        if secondary != 1 {
            return Err(format!("expected exactly one secondary hard link, found {secondary}"));
        }

        let one = index
            .search_with("base.bin", &opts)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|h| h.item.name.to_string_lossy() == "base.bin")
            .ok_or("base.bin not found")?
            .item
            .size
            .allocated;
        let clones = index.folder_size(&clone_dir).ok_or("folder_size(clones) is None")?;
        if clones.allocated != 3 * one {
            return Err(format!("allocated must count every clone in full: {} != 3 * {one}", clones.allocated));
        }
        if clones.allocated_unique > one + one / 20 {
            return Err(format!("three APFS clones are one extent but allocated_unique is {} vs one copy {one}", clones.allocated_unique));
        }
        detail["live_change_batches"] = json!(batches);
        detail["hard_link_logical"] = json!(links.logical);
        detail["clone_allocated"] = json!(clones.allocated);
        detail["clone_allocated_unique"] = json!(clones.allocated_unique);
    }

    Ok(detail)
}
