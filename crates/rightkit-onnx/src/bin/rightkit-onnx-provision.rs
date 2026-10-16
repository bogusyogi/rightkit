//! `rightkit-onnx-provision --dest <bundle dir> --cache <archive cache dir>
//! [--target windows-x64|macos-arm64|android-arm64] [--catalog <native-libraries.json>]`
//!
//! Downloads the catalog ONNX Runtime archive (plus DirectML on Windows), verifies it, extracts
//! the runtime files and writes `manifest.json`. Run once per build, not at app run time.

use std::path::PathBuf;
use std::process::ExitCode;

use rightkit_onnx::provision::{
    http_fetch, load_catalog, provision_runtime, provision_runtime_with, ProvisionTarget,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rightkit-onnx-provision: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut dest = None;
    let mut cache = None;
    let mut target = None;
    let mut catalog = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--dest" => dest = Some(PathBuf::from(value("--dest")?)),
            "--cache" => cache = Some(PathBuf::from(value("--cache")?)),
            "--target" => {
                let text = value("--target")?;
                target = Some(ProvisionTarget::parse(&text).ok_or_else(|| {
                    format!(
                        "unknown target {text:?}; use windows-x64, macos-arm64 or android-arm64"
                    )
                })?);
            }
            "--catalog" => catalog = Some(PathBuf::from(value("--catalog")?)),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let dest = dest.ok_or("--dest is required")?;
    let cache = cache.ok_or("--cache is required")?;
    let target = target
        .or_else(ProvisionTarget::host)
        .ok_or("no --target given and this host has no provisionable target")?;
    let outcome = match catalog {
        None => provision_runtime(target, &dest, &cache),
        Some(path) => {
            let catalog = load_catalog(&path).map_err(|e| e.to_string())?;
            provision_runtime_with(&catalog, target, &dest, &cache, &mut http_fetch)
        }
    }
    .map_err(|e| e.to_string())?;
    println!(
        "{} {} {} in {}",
        if outcome.reused {
            "reused"
        } else {
            "provisioned"
        },
        outcome.runtime.platform,
        outcome.runtime.onnxruntime.version,
        outcome.runtime.dir.display()
    );
    Ok(())
}
