mod common;
use common::*;
use rightkit_fsindex::backend::windows_stub::UnsupportedBackend;
use rightkit_fsindex::{EntryKind, Error, Index, SearchOptions};
use std::sync::Arc;

#[test]
fn crawl_sizes_and_children_sorted() {
    let t = tree();
    write(&t.root.join("a/big.bin"), 300_000);
    write(&t.root.join("a/small.txt"), 10);
    write(&t.root.join("b/mid.bin"), 100_000);
    std::os::unix::fs::symlink("a/big.bin", t.root.join("link")).unwrap();
    let idx = Index::open(static_config(&t.root)).unwrap();
    let a = idx.folder_size(t.root.join("a")).unwrap();
    assert_eq!(a.logical, 300_010);
    assert!(a.allocated >= a.logical && a.allocated_unique <= a.allocated);
    assert_eq!(a.files, 2);
    let root = idx.folder_size(&t.root).unwrap();
    assert_eq!(root.files, 4);
    assert!(root.logical >= 400_010);
    let kids = idx.children_sorted_by_size(&t.root).unwrap();
    let names: Vec<_> = kids
        .iter()
        .map(|k| k.name.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&names[..2], ["a", "b"]);
    assert!(kids
        .windows(2)
        .all(|w| w[0].size.allocated_unique >= w[1].size.allocated_unique));
    assert!(kids.iter().any(|k| k.kind == EntryKind::Symlink));
    assert!(idx.folder_size(t.root.join("nope")).is_none());
}

#[test]
fn size_fields_are_64_bit_and_folders_are_nonzero() {
    // Sparse file: 5 GiB logical, almost nothing allocated.
    let t = tree();
    let f = std::fs::File::create(t.root.join("sparse.bin")).unwrap();
    f.set_len(5 << 30).unwrap();
    drop(f);
    let idx = Index::open(static_config(&t.root)).unwrap();
    let s = idx.folder_size(&t.root).unwrap();
    assert_eq!(s.logical, 5 << 30);
    assert!(
        s.allocated < 1 << 20,
        "sparse file must not count as allocated"
    );
}

#[test]
fn live_create_rename_delete_move() {
    let t = tree();
    write(&t.root.join("seed.txt"), 5);
    let idx = Index::open(config(&t.root)).unwrap();
    write(&t.root.join("d1/report_final.txt"), 1000);
    wait_for(&idx, "create", |i| {
        i.search("report_final").unwrap().len() == 1
    });
    assert_eq!(idx.folder_size(t.root.join("d1")).unwrap().logical, 1000);

    std::fs::rename(
        t.root.join("d1/report_final.txt"),
        t.root.join("d1/renamed_doc.txt"),
    )
    .unwrap();
    wait_for(&idx, "rename", |i| {
        i.search("renamed_doc").unwrap().len() == 1 && i.search("report_final").unwrap().is_empty()
    });

    std::fs::create_dir_all(t.root.join("d2")).unwrap();
    std::fs::rename(t.root.join("d1"), t.root.join("d2/moved")).unwrap();
    wait_for(&idx, "move", |i| {
        i.folder_size(t.root.join("d2/moved")).is_some()
            && i.folder_size(t.root.join("d1")).is_none()
    });
    assert_eq!(idx.folder_size(t.root.join("d2")).unwrap().logical, 1000);

    std::fs::remove_file(t.root.join("d2/moved/renamed_doc.txt")).unwrap();
    wait_for(&idx, "delete", |i| {
        i.search("renamed_doc").unwrap().is_empty()
    });
    assert_eq!(idx.folder_size(&t.root).unwrap().logical, 5);

    std::fs::remove_dir_all(t.root.join("d2")).unwrap();
    wait_for(&idx, "rmdir", |i| {
        i.folder_size(t.root.join("d2")).is_none()
    });
}

#[test]
fn rollups_stay_exact_after_churn() {
    let t = tree();
    for i in 0..20 {
        write(
            &t.root.join(format!("x/y{}/f{i}.dat", i % 4)),
            1000 + i * 37,
        );
    }
    let idx = Index::open(config(&t.root)).unwrap();
    write(&t.root.join("x/y1/new.dat"), 50_000);
    write(&t.root.join("x/y9/deep/er/new2.dat"), 7_000);
    std::fs::remove_file(t.root.join("x/y0/f0.dat")).unwrap();
    std::fs::rename(t.root.join("x/y2"), t.root.join("x/y3/inner")).unwrap();
    write(&t.root.join("x/y1/f1.dat"), 123); // shrink in place
    wait_for(&idx, "churn", |i| {
        i.folder_size(t.root.join("x/y9/deep/er")).is_some()
            && i.folder_size(t.root.join("x/y2")).is_none()
            && i.folder_size(t.root.join("x/y1"))
                .is_some_and(|s| s.logical > 50_000)
    });
    idx.flush();
    assert_matches_fresh_crawl(&idx, &t.root);
}

#[test]
fn hard_links_count_once() {
    let t = tree();
    write(&t.root.join("a/orig.bin"), 200_000);
    std::fs::create_dir_all(t.root.join("b")).unwrap();
    std::fs::hard_link(t.root.join("a/orig.bin"), t.root.join("a/second.bin")).unwrap();
    std::fs::hard_link(t.root.join("a/orig.bin"), t.root.join("b/third.bin")).unwrap();
    let idx = Index::open(static_config(&t.root)).unwrap();
    let r = idx.folder_size(&t.root).unwrap();
    assert_eq!(r.logical, 200_000);
    assert_eq!(r.files, 1);
    let secondaries = ["a/orig.bin", "a/second.bin", "b/third.bin"]
        .iter()
        .filter(|p| {
            idx.search(p.rsplit('/').next().unwrap()).unwrap()[0]
                .item
                .is_secondary_link
        })
        .count();
    assert_eq!(secondaries, 2);
    // The folder holding the owner counts it; folders with only secondary links count 0.
    assert_eq!(
        idx.folder_size(t.root.join("a")).unwrap().logical
            + idx.folder_size(t.root.join("b")).unwrap().logical,
        200_000
    );
}

#[test]
fn hard_link_ownership_moves_on_delete() {
    let t = tree();
    write(&t.root.join("a/one.bin"), 150_000);
    std::fs::create_dir_all(t.root.join("b")).unwrap();
    std::fs::hard_link(t.root.join("a/one.bin"), t.root.join("b/two.bin")).unwrap();
    let idx = Index::open(config(&t.root)).unwrap();
    std::fs::remove_file(t.root.join("a/one.bin")).unwrap();
    wait_for(&idx, "owner removed", |i| {
        i.search("one.bin").unwrap().is_empty()
    });
    wait_for(&idx, "ownership moved", |i| {
        i.folder_size(t.root.join("b")).unwrap().logical == 150_000
    });
    assert_eq!(idx.folder_size(&t.root).unwrap().logical, 150_000);
}

fn is_apfs(p: &std::path::Path) -> bool {
    std::process::Command::new("df")
        .args(["-T", "apfs"])
        .arg(p)
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).lines().count() > 1)
        .unwrap_or(false)
}

#[test]
fn apfs_clones_counted_once() {
    let t = tree();
    if !is_apfs(&t.root) {
        eprintln!("skip: not APFS");
        return;
    }
    write(&t.root.join("orig.bin"), 4 << 20);
    let st = std::process::Command::new("cp")
        .arg("-c")
        .arg(t.root.join("orig.bin"))
        .arg(t.root.join("clone1.bin"))
        .status()
        .unwrap();
    assert!(st.success());
    std::process::Command::new("cp")
        .arg("-c")
        .arg(t.root.join("orig.bin"))
        .arg(t.root.join("clone2.bin"))
        .status()
        .unwrap();
    let idx = Index::open(config(&t.root)).unwrap();
    let r = idx.folder_size(&t.root).unwrap();
    let one = idx.search("orig.bin").unwrap()[0].item.size.allocated;
    assert!(one >= 4 << 20);
    assert_eq!(r.allocated, 3 * one, "allocated counts every clone in full");
    assert!(
        r.allocated_unique <= one + (one / 20),
        "three clones are one extent: unique {} vs one {}",
        r.allocated_unique,
        one
    );
    assert_eq!(r.logical, 3 * (4 << 20));

    // Live: a new clone created after open must not grow allocated_unique.
    std::fs::create_dir_all(t.root.join("sub")).unwrap();
    std::process::Command::new("cp")
        .arg("-c")
        .arg(t.root.join("orig.bin"))
        .arg(t.root.join("sub/clone3.bin"))
        .status()
        .unwrap();
    wait_for(&idx, "clone3", |i| {
        i.folder_size(t.root.join("sub"))
            .is_some_and(|s| s.files == 1)
    });
    let r2 = idx.folder_size(&t.root).unwrap();
    assert_eq!(r2.allocated, 4 * one);
    assert!(r2.allocated_unique <= one + (one / 20));
    // The sub folder alone holds a full-size clone.
    assert!(
        idx.folder_size(t.root.join("sub"))
            .unwrap()
            .allocated_unique
            >= one - (one / 20)
    );
    assert_matches_fresh_crawl(&idx, &t.root);

    // Deleting the original leaves the clones owning the extent.
    std::fs::remove_file(t.root.join("orig.bin")).unwrap();
    wait_for(&idx, "orig gone", |i| {
        i.search("orig.bin").unwrap().is_empty()
    });
    wait_for(&idx, "unique stable", |i| {
        i.folder_size(&t.root).unwrap().allocated == 3 * one
    });
    assert!(idx.folder_size(&t.root).unwrap().allocated_unique <= one + (one / 20));
}

#[test]
fn packages_are_flagged_with_totals() {
    let t = tree();
    write(&t.root.join("Cool.app/Contents/MacOS/cool"), 60_000);
    write(&t.root.join("Photos.photoslibrary/originals/a.jpg"), 40_000);
    write(&t.root.join("plain/f"), 10);
    let idx = Index::open(static_config(&t.root)).unwrap();
    let app = idx.folder_size(t.root.join("Cool.app")).unwrap();
    assert!(app.is_package);
    assert_eq!(app.logical, 60_000);
    assert!(
        idx.folder_size(t.root.join("Photos.photoslibrary"))
            .unwrap()
            .is_package
    );
    assert!(!idx.folder_size(t.root.join("plain")).unwrap().is_package);
    let kids = idx.children_sorted_by_size(&t.root).unwrap();
    assert!(
        kids.iter()
            .find(|k| k.name == "Cool.app")
            .unwrap()
            .is_package
    );
    assert!(!kids.iter().find(|k| k.name == "plain").unwrap().is_package);
}

#[test]
fn fuzzy_and_typo_queries() {
    let t = tree();
    for n in [
        "invoice_2024.pdf",
        "holiday_photo.jpg",
        "Invoice Archive/notes.txt",
        "readme.md",
        "quarterly_report.docx",
    ] {
        write(&t.root.join(n), 10);
    }
    let idx = Index::open(static_config(&t.root)).unwrap();
    let names = |q: &str| {
        idx.search(q)
            .unwrap()
            .into_iter()
            .map(|h| h.item.name.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names("invoice 2024"), ["invoice_2024.pdf"]);
    assert!(names("invc").contains(&"invoice_2024.pdf".to_string()));
    assert!(
        names("invoise").contains(&"invoice_2024.pdf".to_string()),
        "one typo is forgiven"
    );
    assert!(
        names("reprot").contains(&"quarterly_report.docx".to_string())
            || names("quarterly").len() == 1
    );
    assert_eq!(names("^readme")[0], "readme.md");
    assert!(names("zzzqqq").is_empty());
    let scoped = idx
        .search_with(
            "notes",
            &SearchOptions {
                limit: 5,
                scope: Some(t.root.join("Invoice Archive")),
            },
        )
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert!(idx.search("!x").is_err());
}

#[test]
fn name_and_size_mode_never_opens_file_contents() {
    let t = tree();
    for i in 0..5 {
        let p = t.root.join(format!("secret{i}.txt"));
        write(&p, 4096 + i);
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    }
    if unsafe { libc_geteuid() } == 0 {
        eprintln!("running as root: mode 000 proves nothing, relying on the source check");
    }
    let idx = Index::open(config(&t.root)).unwrap();
    assert_eq!(idx.folder_size(&t.root).unwrap().files, 5);
    assert_eq!(idx.folder_size(&t.root).unwrap().logical, 5 * 4096 + 10);
    // Live path too: change a file's metadata (no content access) and re-sync.
    std::fs::set_permissions(
        t.root.join("secret0.txt"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    write(&t.root.join("later.txt"), 100);
    std::fs::set_permissions(
        t.root.join("later.txt"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    wait_for(&idx, "later", |i| i.search("later").unwrap().len() == 1);
    for i in 0..5 {
        let _ = std::fs::set_permissions(
            t.root.join(format!("secret{i}.txt")),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        );
    }
}

extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[test]
fn only_the_content_module_may_open_files() {
    // Static guard for name/size mode: no source file except content.rs (feature-gated),
    // snapshot.rs (our own state file) or the directory-only backend may touch file contents.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let banned = [
        "File::open",
        "fs::read(",
        "read_to_string",
        "OpenOptions",
        "fs::read_to",
    ];
    let allowed = ["content.rs", "snapshot.rs", "names.rs"];
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out)
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p)
            }
        }
    }
    let mut files = Vec::new();
    walk(&dir, &mut files);
    for f in files {
        if allowed.contains(&f.file_name().unwrap().to_str().unwrap()) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        for b in banned {
            assert!(!src.contains(b), "{} uses {b}", f.display());
        }
    }
}

#[test]
fn unreadable_roots_and_dirs_are_reported_not_fatal() {
    let t = tree();
    write(&t.root.join("ok/f"), 10);
    std::fs::create_dir_all(t.root.join("locked")).unwrap();
    write(&t.root.join("locked/hidden"), 10);
    std::fs::set_permissions(
        t.root.join("locked"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let mut cfg = static_config(&t.root);
    cfg.roots.push(t.root.join("does-not-exist"));
    let idx = Index::open(cfg).unwrap();
    let st = idx.status();
    assert_eq!(st.roots.len(), 2);
    assert!(st.roots[0].readable);
    assert!(!st.roots[1].readable && st.roots[1].error.is_some());
    if unsafe { libc_geteuid() } != 0 {
        assert!(
            st.unreadable_dirs.iter().any(|p| p.ends_with("locked")),
            "{:?}",
            st.unreadable_dirs
        );
    }
    assert_eq!(idx.folder_size(t.root.join("ok")).unwrap().files, 1);
    std::fs::set_permissions(
        t.root.join("locked"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
}

#[test]
fn protected_stores_skipped_unless_configured() {
    let t = tree();
    let home = t.root.join("home");
    write(&home.join("Library/Mail/V10/message.emlx"), 100);
    write(&home.join("Documents/note.txt"), 10);
    let mut cfg = static_config(&t.root);
    cfg.home_dirs = Some(vec![home.clone()]);
    cfg.allow_consent_prompts = true;
    let idx = Index::open(cfg.clone()).unwrap();
    assert!(
        idx.search("message.emlx").unwrap().is_empty(),
        "Mail must not be indexed by default"
    );
    assert!(idx
        .status()
        .skipped_dirs
        .iter()
        .any(|p| p.ends_with("Library/Mail")));
    assert_eq!(idx.search("note.txt").unwrap().len(), 1);
    drop(idx);
    cfg.include_protected = true;
    let idx = Index::open(cfg).unwrap();
    assert_eq!(idx.search("message.emlx").unwrap().len(), 1);
}

#[test]
fn snapshot_resumes_and_catches_up() {
    let t = tree();
    let state = tempfile::tempdir().unwrap();
    write(&t.root.join("a/one.txt"), 100);
    let mut cfg = config(&t.root);
    cfg.state_dir = Some(state.path().to_path_buf());
    {
        let idx = Index::open(cfg.clone()).unwrap();
        assert!(!idx.status().resumed_from_snapshot);
        assert_eq!(idx.folder_size(&t.root).unwrap().files, 1);
    }
    write(&t.root.join("a/two.txt"), 200);
    std::fs::remove_file(t.root.join("a/one.txt")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let idx = Index::open(cfg).unwrap();
    assert!(
        idx.status().resumed_from_snapshot,
        "valid snapshot must be reused"
    );
    wait_for(&idx, "catch up", |i| {
        i.search("two.txt").unwrap().len() == 1 && i.search("one.txt").unwrap().is_empty()
    });
    assert_eq!(idx.folder_size(&t.root).unwrap().logical, 200);
}

#[test]
fn unsupported_backend_returns_unsupported() {
    let t = tree();
    match Index::open_with_backend(static_config(&t.root), Arc::new(UnsupportedBackend)) {
        Err(Error::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {:?}", other.map(|_| ())),
    }
}
