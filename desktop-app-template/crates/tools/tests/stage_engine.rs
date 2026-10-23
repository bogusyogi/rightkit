//! Real-process tests of `stage-engine`. The tools binary is copied to where `rightkit cargo run` leaves
//! it (`<target>/debug/`), a stand-in engine is written where cargo would put it, and the staged copy is
//! checked. Nothing here calls cargo or the broker: staging must work with no process calls at all.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_rightapp-tools");
const HOST: &str = env!("RIGHTAPP_HOST_TRIPLE");
const ENGINE: &str = "fake-engine";

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// An app root with a target tree. `tools_dir` is where the tools binary sits under `target/`.
fn layout(tag: &str, tools_dir: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stage-engine-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("crates/engine")).unwrap();
    fs::write(dir.join("placeholders.json"), "{}").unwrap();
    fs::write(dir.join("package.json"), "{\"name\":\"x\"}").unwrap();
    fs::write(
        dir.join("crates/engine/Cargo.toml"),
        format!("[package]\nname = \"{ENGINE}\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"{ENGINE}\"\npath = \"src/main.rs\"\n"),
    )
    .unwrap();
    let mut tools = dir.join("target");
    for part in tools_dir {
        tools.push(part);
    }
    fs::create_dir_all(&tools).unwrap();
    fs::copy(BIN, tools.join(exe("rightapp-tools"))).unwrap();
    dir
}

fn write_engine(dir: &Path, rel: &[&str]) {
    let mut path = dir.join("target");
    for part in rel {
        path.push(part);
    }
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join(exe(ENGINE)), b"engine-bytes").unwrap();
}

fn run_tools(dir: &Path, tools_dir: &[&str], args: &[&str], triple_env: Option<&str>) -> Output {
    let mut tools = dir.join("target");
    for part in tools_dir {
        tools.push(part);
    }
    let mut cmd = Command::new(tools.join(exe("rightapp-tools")));
    cmd.args(args)
        .current_dir(dir)
        .env_remove("TAURI_ENV_TARGET_TRIPLE");
    if let Some(t) = triple_env {
        cmd.env("TAURI_ENV_TARGET_TRIPLE", t);
    }
    cmd.output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn staged(dir: &Path) -> PathBuf {
    dir.join("src-tauri")
        .join("binaries")
        .join(exe(&format!("{ENGINE}-{HOST}")))
}

#[test]
fn stages_the_release_iterate_engine_from_the_profile_directory() {
    let dir = layout("profile", &["debug"]);
    write_engine(&dir, &["release-iterate"]);
    let out = run_tools(
        &dir,
        &["debug"],
        &["stage-engine", "--profile", "release-iterate"],
        None,
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read(staged(&dir)).unwrap(), b"engine-bytes");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stages_from_the_triple_directory_when_cargo_wrote_one() {
    let dir = layout("triple", &["debug"]);
    write_engine(&dir, &[HOST, "release"]);
    let out = run_tools(&dir, &["debug"], &["stage-engine", "--release"], None);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read(staged(&dir)).unwrap(), b"engine-bytes");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn derives_the_target_root_when_the_tools_binary_is_in_a_triple_directory() {
    let dir = layout("tripledir", &[HOST, "debug"]);
    write_engine(&dir, &["release"]);
    let out = run_tools(&dir, &[HOST, "debug"], &["stage-engine", "--release"], None);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read(staged(&dir)).unwrap(), b"engine-bytes");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_engine_reports_every_path_tried() {
    let dir = layout("missing", &["debug"]);
    let out = run_tools(&dir, &["debug"], &["stage-engine", "--release"], None);
    let msg = text(&out);
    assert!(!out.status.success(), "{msg}");
    assert!(
        msg.contains("tried:") && msg.contains(HOST) && msg.contains("release"),
        "{msg}"
    );
    assert!(msg.contains("pnpm build:engine"), "{msg}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refuses_to_stage_a_cross_target_engine() {
    let dir = layout("cross", &["debug"]);
    write_engine(&dir, &["release"]);
    let other = if HOST.starts_with("x86_64") {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let out = run_tools(
        &dir,
        &["debug"],
        &["stage-engine", "--release"],
        Some(other),
    );
    assert!(
        !out.status.success() && text(&out).contains("cross-target"),
        "{}",
        text(&out)
    );
    assert!(!staged(&dir).exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stage_engine_requires_the_profile_it_was_built_with() {
    let dir = layout("noprofile", &["debug"]);
    let out = run_tools(&dir, &["debug"], &["stage-engine"], None);
    assert!(
        !out.status.success() && text(&out).contains("--release"),
        "{}",
        text(&out)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn build_engine_is_retired_and_names_the_two_calls() {
    let dir = layout("retired", &["debug"]);
    let out = run_tools(&dir, &["debug"], &["build-engine"], None);
    let msg = text(&out);
    assert!(!out.status.success(), "{msg}");
    assert!(
        msg.contains("stage-engine") && msg.contains("build:engine") && msg.contains("exit 125"),
        "{msg}"
    );
    let _ = fs::remove_dir_all(&dir);
}
