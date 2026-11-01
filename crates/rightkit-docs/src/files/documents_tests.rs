// Concurrency test for documents.rs atomic save path.
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rightkit-docs-files-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn concurrent_same_target_writes_are_serialized_and_complete() {
    let path = fixture("concurrent-docx-save.docx");
    std::fs::write(&path, b"initial").unwrap();
    let path = path.to_str().unwrap().to_string();
    let values: Vec<Vec<u8>> = (0..8)
        .map(|index| format!("complete-write-{index:02}").into_bytes())
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(values.len()));
    let handles = values
        .iter()
        .cloned()
        .map(|value| {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                super::write_raw_bytes(&path, &value).unwrap();
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }
    let result = std::fs::read(&path).unwrap();
    assert!(
        values.iter().any(|value| value == &result),
        "partial or unknown final bytes"
    );
    let temp_prefix = ".concurrent-docx-save.docx.rktmp-";
    let backup_prefix = ".concurrent-docx-save.docx.vr-bak-";
    assert!(
        !std::fs::read_dir(std::path::Path::new(&path).parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(temp_prefix) || name.starts_with(backup_prefix)
            })
    );
}

/// Every row of the shared classification table must classify to its expected format through
/// `format_of`, the same rows the TypeScript layer and the e2e journey read.
#[test]
fn format_classification_matches_shared_cases() {
    let rows = include_str!("../../data/document_format_cases.tsv");
    let mut checked = 0usize;
    for line in rows.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, expected) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("case row needs path<TAB>format: {line:?}"));
        let path = format!("/docs/cls/{name}");
        assert_eq!(
            super::format_of(&path),
            expected,
            "format_of disagrees with data/document_format_cases.tsv for {name:?}"
        );
        checked += 1;
    }
    assert!(checked > 0, "shared classification table has no rows");
}
