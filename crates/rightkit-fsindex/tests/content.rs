#![cfg(feature = "content")]
mod common;
use common::*;
use rightkit_fsindex::{Error, Index};

fn cfg_with(
    root: &std::path::Path,
    f: impl Fn(&mut rightkit_fsindex::ContentConfig),
) -> rightkit_fsindex::Config {
    let mut c = static_config(root);
    f(&mut c.content);
    c
}

#[test]
fn off_by_default_at_runtime() {
    let t = tree();
    std::fs::write(t.root.join("a.txt"), "needle in a haystack").unwrap();
    let idx = Index::open(static_config(&t.root)).unwrap();
    assert!(matches!(
        idx.content_search("needle", 10),
        Err(Error::ContentDisabled)
    ));
}

#[test]
fn allowlist_size_type_and_exclusions() {
    let t = tree();
    let w = |p: &str, s: &str| {
        std::fs::create_dir_all(t.root.join(p).parent().unwrap()).unwrap();
        std::fs::write(t.root.join(p), s).unwrap();
    };
    w("docs/a.txt", "the quick brown Needle jumps");
    w("docs/bin.dat", "needle but wrong extension");
    w("docs/big.txt", &format!("needle {}", "x".repeat(5000)));
    w("docs/private/b.txt", "needle excluded");
    w("elsewhere/c.txt", "needle outside allowlist");
    let mut z = std::fs::File::create(t.root.join("docs/zero.txt")).unwrap();
    std::io::Write::write_all(&mut z, b"needle\0binary").unwrap();
    drop(z);
    let root = t.root.clone();
    let idx = Index::open(cfg_with(&t.root, |c| {
        c.enabled = true;
        c.allowlist = vec![root.join("docs")];
        c.max_file_size = 1000;
        c.exclude = vec![root.join("docs/private")];
    }))
    .unwrap();
    let hits = idx.content_search("NEEDLE", 10).unwrap();
    let names: Vec<_> = hits
        .iter()
        .map(|h| h.item.name.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["a.txt"],
        "only the small allowlisted text file qualifies"
    );
    assert!(idx.content_search("ne", 10).is_err());
}

#[test]
fn content_follows_live_changes() {
    let t = tree();
    std::fs::create_dir_all(t.root.join("d")).unwrap();
    let root = t.root.clone();
    let mut cfg = config(&t.root);
    cfg.content.enabled = true;
    cfg.content.allowlist = vec![root.clone()];
    let idx = Index::open(cfg).unwrap();
    std::fs::write(t.root.join("d/n.txt"), "alpha beta gamma").unwrap();
    wait_for(&idx, "indexed", |i| {
        i.content_search("beta", 5).unwrap().len() == 1
    });
    std::fs::write(t.root.join("d/n.txt"), "delta epsilon!").unwrap();
    wait_for(&idx, "reindexed", |i| {
        i.content_search("beta", 5).unwrap().is_empty()
            && i.content_search("epsilon", 5).unwrap().len() == 1
    });
    std::fs::remove_file(t.root.join("d/n.txt")).unwrap();
    wait_for(&idx, "removed", |i| {
        i.content_search("epsilon", 5).unwrap().is_empty()
    });
}
