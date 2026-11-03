//! `rightkit-theme generate --json <brand.json> [--out <file>] [--check]`
//!
//! Resolves a brand file and writes (or prints) the per-mode design tokens as W3C design-token JSON.
//! With `--check` nothing is written: exit 0 when `--out` already holds exactly these bytes, exit 1 when
//! it differs or is missing.

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: rightkit-theme generate --json <brand.json> [--out <file>] [--check]");
    ExitCode::from(2)
}

fn run(brand_path: &str, out: Option<PathBuf>, check: bool) -> Result<ExitCode, String> {
    let text = std::fs::read_to_string(brand_path).map_err(|e| format!("{brand_path}: {e}"))?;
    let brand = rightkit_theme::parse_brand(&text).map_err(|e| format!("{brand_path}: {e}"))?;
    let json =
        rightkit_theme::design_tokens_json(&brand).map_err(|e| format!("{brand_path}: {e}"))?;
    let Some(out) = out else {
        if check {
            return Err("--check needs --out".into());
        }
        print!("{json}");
        return Ok(ExitCode::SUCCESS);
    };
    let existing = std::fs::read_to_string(&out).ok();
    let same = existing.as_deref() == Some(json.as_str());
    if check {
        if !same {
            eprintln!(
                "{} is out of date; run rightkit-theme generate --json",
                out.display()
            );
        }
        return Ok(if same {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }
    if !same {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&out, json).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("generate") {
        return usage();
    }
    let (mut json, mut check) = (false, false);
    let (mut out, mut brand) = (None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--check" => check = true,
            "--out" => match args.next() {
                Some(path) => out = Some(PathBuf::from(path)),
                None => return usage(),
            },
            other if other.starts_with("--") => return usage(),
            other => brand = Some(other.to_owned()),
        }
    }
    let (true, Some(brand)) = (json, brand) else {
        return usage();
    };
    match run(&brand, out, check) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("rightkit-theme: {message}");
            ExitCode::from(1)
        }
    }
}
