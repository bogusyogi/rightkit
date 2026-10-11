//! App helpers in Rust: no Node, no shell scripts.
//!
//!   rightapp-tools brand-sync                       assets/brand/brand.json -> src/brand.css
//!   rightapp-tools stage-engine --release|--profile <p>  copy the built engine into src-tauri/binaries
//!   rightapp-tools build-engine                     retired: refuses; use the two-call pair in package.json
//!   rightapp-tools check                            manifest-consistency gate (no install, no network)
//!   rightapp-tools init --slug s --name N [--org o] rename the template into a real app
//!
//! Run through managed RightKit Cargo: `rightkit cargo run -q -p rightapp-tools -- <command>`.
//! No command here calls the RightKit broker: a managed request refuses nested broker calls.
mod brand;
mod check;
mod engine;
mod init;

use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, String>;

/// The app root: nearest ancestor of the working directory holding `placeholders.json`.
pub fn app_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir().map_err(|e| e.to_string())?;
    loop {
        if dir.join("placeholders.json").is_file() && dir.join("package.json").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("run from inside the app (no placeholders.json + package.json found above the working directory)".into());
        }
    }
}

pub fn read(root: &Path, rel: &str) -> Result<String> {
    std::fs::read_to_string(root.join(rel)).map_err(|e| format!("cannot read {rel}: {e}"))
}

pub fn read_json(root: &Path, rel: &str) -> Result<serde_json::Value> {
    serde_json::from_str(&read(root, rel)?).map_err(|e| format!("{rel}: {e}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("brand-sync") => app_root().and_then(|r| brand::sync(&r)),
        Some("stage-engine") => app_root().and_then(|r| engine::stage(&r, &args[1..])),
        Some("build-engine") => engine::build_retired(),
        Some("check") => app_root().and_then(|r| check::run(&r)),
        Some("init") => app_root().and_then(|r| init::run(&r, &args[1..])),
        _ => Err("usage: rightapp-tools brand-sync | stage-engine --release|--profile <p> | check | init --slug <s> --name <N> [--org <o>]".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
