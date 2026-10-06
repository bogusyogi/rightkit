#![cfg(feature = "filename-index")]
//! End-to-end: index a real directory tree on disk and query it.

use rightkit_search::filename::{Entry, Error, FilenameIndex, Kind, Request};
use std::path::Path;

fn scan(index: &mut FilenameIndex, root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        let kind = if meta.is_dir() { Kind::Dir } else { Kind::File };
        index.insert(Entry {
            path: entry.path(),
            kind,
            size: (!meta.is_dir()).then_some(meta.len()),
        });
        if meta.is_dir() {
            scan(index, &entry.path());
        }
    }
}

fn names(page: &rightkit_search::filename::Page) -> Vec<String> {
    page.items
        .iter()
        .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn searches_a_real_tree_by_name_extension_size_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("docs/Reports")).unwrap();
    std::fs::write(root.join("docs/Reports/Q1-Report.PDF"), vec![0u8; 5_000]).unwrap();
    std::fs::write(root.join("docs/Reports/q2-report.txt"), vec![0u8; 100]).unwrap();
    std::fs::write(root.join("docs/notes.md"), b"hello").unwrap();
    std::fs::write(root.join(".hidden-report"), vec![0u8; 50]).unwrap();

    let mut index = FilenameIndex::new();
    scan(&mut index, root);
    assert_eq!(index.len(), 6);

    let page = index.search(&Request::new("REPORT", 10)).unwrap();
    // Ordered by full path, component-wise.
    assert_eq!(
        names(&page),
        [
            ".hidden-report",
            "Reports",
            "Q1-Report.PDF",
            "q2-report.txt"
        ]
    );
    assert_eq!(page.total_matches, 4);
    assert!(
        names(&page).contains(&".hidden-report".to_string()),
        "hidden names are matched"
    );

    let pdf = index
        .search(&Request {
            query: "report".into(),
            extension: Some(".pdf".into()),
            ..Request::default()
        })
        .unwrap();
    assert_eq!(names(&pdf), ["Q1-Report.PDF"]);

    let big = index
        .search(&Request {
            min_size: Some(1_000),
            kind: Some(Kind::File),
            ..Request::default()
        })
        .unwrap();
    assert_eq!(names(&big), ["Q1-Report.PDF"]);

    let dirs = index
        .search(&Request {
            kind: Some(Kind::Dir),
            ..Request::default()
        })
        .unwrap();
    assert_eq!(names(&dirs), ["docs", "Reports"]);
}

#[test]
fn paginates_deterministically_and_validates_bounds() {
    let mut index = FilenameIndex::new();
    for i in 0..25 {
        index.insert(Entry {
            path: format!("/x/file-{i:02}.txt").into(),
            kind: Kind::File,
            size: Some(i),
        });
    }
    let p1 = index
        .search(&Request {
            query: "file".into(),
            limit: 10,
            ..Request::default()
        })
        .unwrap();
    assert_eq!(
        (p1.total_matches, p1.has_more, p1.items.len()),
        (25, true, 10)
    );
    let p3 = index
        .search(&Request {
            query: "file".into(),
            limit: 10,
            offset: 20,
            ..Request::default()
        })
        .unwrap();
    assert_eq!((p3.items.len(), p3.has_more), (5, false));
    assert_eq!(names(&p3).first().unwrap(), "file-20.txt");
    assert!(index
        .search(&Request {
            offset: 100,
            limit: 10,
            ..Request::default()
        })
        .unwrap()
        .items
        .is_empty());

    assert!(matches!(
        index.search(&Request {
            limit: 0,
            ..Request::default()
        }),
        Err(Error::InvalidLimit(0))
    ));
    assert!(matches!(
        index.search(&Request {
            extension: Some("a/b".into()),
            ..Request::default()
        }),
        Err(Error::InvalidExtension(_))
    ));
    assert!(matches!(
        index.search(&Request {
            min_size: Some(9),
            max_size: Some(1),
            ..Request::default()
        }),
        Err(Error::InvalidSizeRange)
    ));
    assert!(matches!(
        index.search(&Request::new("a\0b", 5)),
        Err(Error::InvalidQuery(_))
    ));
}

#[test]
fn remove_tree_drops_a_subtree() {
    let mut index = FilenameIndex::new();
    for p in ["/r/a/one.txt", "/r/a/b/two.txt", "/r/c/three.txt"] {
        index.insert(Entry {
            path: p.into(),
            kind: Kind::File,
            size: None,
        });
    }
    assert_eq!(index.remove_tree(Path::new("/r/a")), 2);
    let page = index.search(&Request::default()).unwrap();
    assert_eq!(names(&page), ["three.txt"]);
}
