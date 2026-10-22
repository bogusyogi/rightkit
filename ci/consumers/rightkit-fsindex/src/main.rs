// rightkit-fsindex 0.5.0 consumed by exact registry pin, outside the workspace.
//
// Proves, on every platform: Config::new defaults, WriteBudgetConfig, the pure Windows Search SQL
// helpers, parse_usn_records on short buffers, and the stub backend refusing with Error::Unsupported.
// On macOS also opens an Index over a tiny temp tree (static: no watcher, no state directory) and reads
// search, folder_size, children_sorted_by_size and write_budget_status. Off macOS and Windows the
// default Index::open is refused as Unsupported. The Windows live backends are not opened.
// Does not prove: change feed, snapshot resume, content search, FSEvents or Windows backends.
// State: one temp directory with three small files, removed at the end. No network, no real user data.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rightkit_fsindex::backend::windows_stub::UnsupportedBackend;
use rightkit_fsindex::{
    escape_sql_literal, like_pattern, parse_usn_records, Config, Error, Index, WriteBudgetConfig,
};

fn main() -> Result<(), String> {
    // Config defaults.
    let config = Config::new(vec![PathBuf::from("root")]);
    assert_eq!(config.roots, vec![PathBuf::from("root")]);
    assert!(config.state_dir.is_none() && config.write_budget.is_none());
    assert!(config.watch && !config.include_protected && !config.include_build_output);
    assert_eq!(config.relist_min_interval, Duration::from_secs(5));
    assert_eq!(config.rescan_cooldown, Duration::from_secs(30));

    // Write budget: a per-hour cap, no footprint cap until one is added.
    let budget = WriteBudgetConfig::new(1 << 30);
    assert_eq!(
        (budget.max_bytes_per_hour, budget.max_footprint_bytes),
        (1 << 30, None)
    );
    let capped = budget.with_max_footprint(1 << 40);
    assert_eq!(capped.max_footprint_bytes, Some(1 << 40));

    // Pure Windows Search SQL helpers: quotes doubled, controls blanked, wildcards neutralised.
    assert_eq!(escape_sql_literal("it's"), "it''s");
    assert_eq!(escape_sql_literal("a\nb"), "a b");
    assert_eq!(like_pattern("50%_[x]"), "50___x_");
    assert_eq!(like_pattern("o'brien"), "o''brien");

    // USN record parsing: a buffer holding only the cursor is a valid empty batch; shorter is refused.
    assert!(parse_usn_records(&[0u8; 4]).is_err());
    let empty = parse_usn_records(&7u64.to_le_bytes()).map_err(|e| format!("{e:?}"))?;
    assert!(empty.records.is_empty());
    assert_eq!(empty.next, 7);
    assert!(
        parse_usn_records(&[0u8; 12]).is_err(),
        "a truncated record header is refused"
    );

    // The stub backend refuses typed, on every platform.
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rightkit-fsindex-consumer-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("a")).map_err(|e| e.to_string())?;
    let outcome = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    outcome?;

    println!("OK rightkit-fsindex 0.5.0 (registry consumer)");
    Ok(())
}

fn run(dir: &std::path::Path) -> Result<(), String> {
    std::fs::write(dir.join("a/report_final.txt"), vec![7u8; 1000]).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("a/notes.txt"), vec![7u8; 10]).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("top.bin"), vec![7u8; 100]).map_err(|e| e.to_string())?;

    let mut config = Config::new(vec![dir.to_path_buf()]);
    config.home_dirs = Some(Vec::new());
    config.watch = false;

    match Index::open_with_backend(config.clone(), Arc::new(UnsupportedBackend)) {
        Err(Error::Unsupported(_)) => {}
        other => return Err(format!("expected Unsupported, got {:?}", other.map(|_| ()))),
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    match Index::open(config) {
        Err(Error::Unsupported(_)) => {}
        other => return Err(format!("expected Unsupported, got {:?}", other.map(|_| ()))),
    }

    #[cfg(target_os = "macos")]
    {
        config.write_budget = Some(WriteBudgetConfig::new(1 << 30).with_max_footprint(1 << 40));
        let index = Index::open(config).map_err(|e| e.to_string())?;
        let root = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;

        let folder = index
            .folder_size(root.join("a"))
            .ok_or("folder a not indexed")?;
        assert_eq!(folder.logical, 1010);
        assert_eq!(folder.files, 2);
        assert!(folder.allocated >= folder.logical || folder.allocated_unique <= folder.allocated);
        assert!(index.folder_size(root.join("absent")).is_none());

        let hits = index.search("report_final").map_err(|e| e.to_string())?;
        assert!(hits.iter().any(|hit| hit.item.name == "report_final.txt"));

        let children = index
            .children_sorted_by_size(&root)
            .ok_or("root not indexed")?;
        assert!(children.iter().any(|item| item.name == "a"));
        assert!(children.iter().any(|item| item.name == "top.bin"));

        let status = index.write_budget_status();
        assert_eq!(status.max_bytes_per_hour, Some(1 << 30));
        assert_eq!(status.max_footprint_bytes, Some(1 << 40));
        assert!(!status.footprint_capped);
        assert!(!index.would_exceed_write_budget(1));
        assert!(index.would_exceed_write_budget(1 << 40));
        assert!(!status.would_exceed(1));
    }
    #[cfg(windows)]
    let _ = config;
    Ok(())
}
