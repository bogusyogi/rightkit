//! Real-process gate tests on a copy of the template: the gate passes a clean tree, rejects a
//! Node harness / inexact pin / stale placeholder, and `init` produces an app that still passes.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_rightapp-tools");

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name();
        if matches!(name.to_str(), Some("target" | "node_modules" | ".git" | "dist" | "gen" | "binaries")) {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

fn template_copy(tag: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let dir = std::env::temp_dir().join(format!("tpl-gate-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy_tree(&root, &dir);
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn clean_template_passes_and_node_harnesses_or_loose_pins_fail() {
    let dir = template_copy("gate");
    let ok = run(&dir, &["check"]);
    assert!(ok.status.success(), "{}", text(&ok));

    let pkg_path = dir.join("package.json");
    let original = fs::read_to_string(&pkg_path).unwrap();

    fs::write(&pkg_path, original.replace("\"typescript\"", "\"puppeteer-core\": \"25.9.0\",\n    \"typescript\"")).unwrap();
    let bad = run(&dir, &["check"]);
    assert!(!bad.status.success() && text(&bad).contains("forbidden test harness"), "{}", text(&bad));

    fs::write(&pkg_path, original.replace("\"@rightkit/logs\": \"0.1.4\"", "\"@rightkit/logs\": \"^0.1.4\"")).unwrap();
    let bad = run(&dir, &["check"]);
    assert!(!bad.status.success() && text(&bad).contains("exact registry version"), "{}", text(&bad));

    fs::write(&pkg_path, &original).unwrap();
    fs::write(dir.join("scripts").join("helper.mjs"), "console.log(1)\n").unwrap();
    let bad = run(&dir, &["check"]);
    assert!(!bad.status.success() && text(&bad).contains("write it in Rust"), "{}", text(&bad));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn init_renames_the_template_and_the_result_still_passes() {
    let dir = template_copy("init");
    let out = run(&dir, &["init", "--slug", "acme", "--name", "Acme", "--org", "com.acme"]);
    assert!(out.status.success(), "{}", text(&out));
    let tauri = fs::read_to_string(dir.join("src-tauri/tauri.conf.json")).unwrap();
    assert!(tauri.contains("\"identifier\": \"com.acme.acme\"") && tauri.contains("\"productName\": \"Acme\""), "{tauri}");
    assert!(dir.join("crates/tools/src/init.rs").exists());
    let cargo = fs::read_to_string(dir.join("crates/engine/Cargo.toml")).unwrap();
    assert!(cargo.contains("name = \"acme-engine\"") && !cargo.to_lowercase().contains("rightapp"), "{cargo}");
    let _ = fs::remove_dir_all(&dir);
}
