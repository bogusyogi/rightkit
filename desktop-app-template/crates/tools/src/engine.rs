//! Stage the engine sidecar where Tauri's `bundle.externalBin` expects it:
//! `src-tauri/binaries/<engine>-<target-triple>[.exe]`.
//!
//! Staging makes no process calls. The build is a separate top-level managed call that package.json
//! runs first (`rightkit cargo build -p <engine> --release|--profile release-iterate`), because a
//! managed RightKit request refuses broker calls nested inside it (exit 125). `stage-engine` then
//! finds the executable cargo wrote beside this tools binary and copies it.
use crate::{read, Result};
use std::path::{Path, PathBuf};

/// The host triple this tools binary was compiled for (`$TARGET`, set by build.rs).
const HOST_TRIPLE: &str = env!("RIGHTAPP_HOST_TRIPLE");

fn engine_name(root: &Path) -> Result<String> {
    let toml = read(root, "crates/engine/Cargo.toml")?;
    let bin = toml
        .split("[[bin]]")
        .nth(1)
        .ok_or("engine Cargo.toml has no [[bin]]")?;
    bin.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("name")
                .and_then(|r| r.split('"').nth(1))
                .map(String::from)
        })
        .ok_or_else(|| "engine [[bin]] has no name".into())
}

/// The profile directory cargo writes for the flags given to `stage-engine`:
/// `--release` -> `release`; `--profile <name>` -> `<name>` (`dev` is written as `debug`).
fn profile_dir(args: &[String]) -> Result<String> {
    if args.iter().any(|a| a == "--release") {
        return Ok("release".into());
    }
    let i = args.iter().position(|a| a == "--profile").ok_or(
        "stage-engine needs --release or --profile <name>, the same flags the engine was built with",
    )?;
    match args.get(i + 1).map(String::as_str) {
        Some("dev") => Ok("debug".into()),
        Some(name) if !name.is_empty() && !name.starts_with('-') => Ok(name.to_string()),
        _ => Err("--profile needs a profile name".into()),
    }
}

/// The managed target root, derived from this tools binary. Cargo writes it to
/// `<target>/<profile>/<bin>`, or `<target>/<triple>/<profile>/<bin>` when a target is set.
pub fn target_root(exe: &Path, host_triple: &str) -> Result<PathBuf> {
    let profile = exe.parent().ok_or("tools binary has no directory")?;
    let up = profile
        .parent()
        .ok_or("tools binary is not inside a target directory")?;
    if up.file_name().and_then(|n| n.to_str()) == Some(host_triple) {
        up.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "triple directory has no parent".into())
    } else {
        Ok(up.to_path_buf())
    }
}

/// Where the engine may have been written, in the order tried.
pub fn engine_candidates(target: &Path, triple: &str, profile: &str, engine: &str) -> Vec<PathBuf> {
    let file = format!("{engine}{}", std::env::consts::EXE_SUFFIX);
    vec![
        target.join(triple).join(profile).join(&file),
        target.join(profile).join(&file),
    ]
}

pub fn stage(root: &Path, args: &[String]) -> Result<()> {
    let profile = profile_dir(args)?;
    let engine = engine_name(root)?;
    let triple = match std::env::var("TAURI_ENV_TARGET_TRIPLE") {
        Ok(t) if !t.is_empty() => t,
        _ => HOST_TRIPLE.to_string(),
    };
    if triple != HOST_TRIPLE {
        return Err(format!(
            "cannot stage a {triple} engine from a {HOST_TRIPLE} build: cross-target engine builds are not supported; build the engine on its native host"
        ));
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let target = target_root(&exe, HOST_TRIPLE)?;
    let candidates = engine_candidates(&target, &triple, &profile, &engine);
    let source = candidates.iter().find(|p| p.is_file()).ok_or_else(|| {
        let tried: Vec<String> = candidates.iter().map(|p| format!("  {}", p.display())).collect();
        format!("{engine} was not built for profile {profile}; tried:\n{}\nrun `pnpm build:engine` (or `pnpm build:engine:dev`) first", tried.join("\n"))
    })?;
    let dir = root.join("src-tauri").join("binaries");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let suffix = if triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let dest = dir.join(format!("{engine}-{triple}{suffix}"));
    std::fs::copy(source, &dest).map_err(|e| format!("stage {}: {e}", source.display()))?;
    println!("staged {} -> {}", source.display(), dest.display());
    Ok(())
}

/// The retired one-call builder. It cannot run under the broker, so it refuses and names the pair.
pub fn build_retired() -> Result<()> {
    Err("build-engine cannot run inside a managed RightKit request: nested broker calls are refused (exit 125). \
Use the two top-level calls instead: `pnpm build:engine` (release) or `pnpm build:engine:dev` (release-iterate), \
which run `rightkit cargo build -p <engine>` and then `rightkit cargo run -p rightapp-tools -- stage-engine`."
        .into())
}
