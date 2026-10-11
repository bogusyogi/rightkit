//! Pattern excludes, the build-output switch, and exact-path excludes against a real index.
#![cfg(target_os = "macos")]

mod common;
use common::*;
use rightkit_fsindex::{Index, Item, SortKey};
use std::collections::HashMap;
use std::path::Path;

fn build_tree(root: &Path) {
    write(&root.join("target/debug/huge.bin"), 10);
    write(&root.join("node_modules/pkg/index.js"), 10);
    write(&root.join("app/.build/obj.o"), 10);
    write(&root.join("repo/.git/objects/ab/cd"), 10);
    write(&root.join("repo/.git/config"), 10);
    write(&root.join("home/Library/Caches/c.db"), 10);
    write(&root.join("home/Library/Prefs/p.plist"), 10);
    write(&root.join("src/keep_me.txt"), 10);
}

/// Name -> excluded flag for the children of `dir`.
fn kids(idx: &Index, dir: &Path) -> HashMap<String, bool> {
    let items: Vec<Item> = idx.children_sorted_by(dir, SortKey::Name).unwrap();
    items
        .into_iter()
        .map(|i| (i.name.to_string_lossy().into_owned(), i.excluded))
        .collect()
}

#[test]
fn default_patterns_exclude_build_output_and_keep_the_rest() {
    let t = tree();
    build_tree(&t.root);
    let idx = Index::open(static_config(&t.root)).unwrap();

    let root_kids = kids(&idx, &t.root);
    assert!(root_kids["target"]);
    assert!(root_kids["node_modules"]);
    assert!(!root_kids["app"]);
    assert!(!root_kids["src"]);
    assert!(kids(&idx, &t.root.join("app"))[".build"]);
    let git = kids(&idx, &t.root.join("repo/.git"));
    assert!(git["objects"], ".git/objects is excluded");
    assert!(!git["config"], "the rest of .git is kept");
    let lib = kids(&idx, &t.root.join("home/Library"));
    assert!(lib["Caches"]);
    assert!(!lib["Prefs"]);

    assert!(
        idx.search("huge").unwrap().is_empty(),
        "nothing under target is indexed"
    );
    assert_eq!(idx.search("keep_me").unwrap().len(), 1);
}

#[test]
fn include_build_output_drops_the_defaults() {
    let t = tree();
    build_tree(&t.root);
    let mut cfg = static_config(&t.root);
    cfg.include_build_output = true;
    let idx = Index::open(cfg).unwrap();
    assert!(!kids(&idx, &t.root)["target"]);
    assert_eq!(idx.search("huge").unwrap().len(), 1);
}

#[test]
fn caller_patterns_apply_in_addition_to_the_defaults() {
    let t = tree();
    build_tree(&t.root);
    write(&t.root.join("secret_vault/a.txt"), 10);
    let mut cfg = static_config(&t.root);
    cfg.exclude_patterns = vec!["secret*".to_string()];
    let idx = Index::open(cfg).unwrap();
    assert!(kids(&idx, &t.root)["secret_vault"]);
    assert!(kids(&idx, &t.root)["target"], "defaults still apply");
}

#[test]
fn exact_path_excludes_still_apply() {
    let t = tree();
    build_tree(&t.root);
    let mut cfg = static_config(&t.root);
    cfg.exclude = vec![t.root.join("src")];
    let idx = Index::open(cfg).unwrap();
    assert!(kids(&idx, &t.root)["src"]);
    assert!(idx.search("keep_me").unwrap().is_empty());
}

#[test]
fn a_root_placed_under_a_pattern_name_is_indexed() {
    let t = tree();
    write(&t.root.join("target/proj/inside.txt"), 10);
    let idx = Index::open(static_config(&t.root.join("target/proj"))).unwrap();
    assert_eq!(idx.search("inside").unwrap().len(), 1);
}
