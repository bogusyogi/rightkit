//! `rightkit-catalog check <dir>` and
//! `rightkit-catalog generate <dir> --target ts|rust|swift --out <file> [--check]`
//!
//! `check` exits 1 on any incomplete locale. `generate` refuses an incomplete catalog (so a generated
//! table is always complete) and, with `--check`, exits 1 when the file differs from what it would write.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rightkit_catalog::{check, generate, load_dir, Target};

fn usage() -> ExitCode {
    eprintln!("usage: rightkit-catalog check <dir>\n       rightkit-catalog generate <dir> --target ts|rust|swift --out <file> [--check]");
    ExitCode::from(2)
}

fn complete(dir: &Path) -> Result<rightkit_catalog::Catalogs, String> {
    let catalogs = load_dir(dir)?;
    let findings = check(&catalogs);
    if findings.is_empty() {
        return Ok(catalogs);
    }
    let lines: Vec<String> = findings.iter().map(ToString::to_string).collect();
    Err(format!(
        "{} problem(s):\n{}",
        findings.len(),
        lines.join("\n")
    ))
}

fn run_generate(
    dir: &Path,
    target: Target,
    out: &Path,
    only_check: bool,
) -> Result<ExitCode, String> {
    let text = generate(&complete(dir)?, target);
    let same = std::fs::read_to_string(out).ok().as_deref() == Some(text.as_str());
    if only_check {
        if !same {
            eprintln!(
                "{} is out of date; run rightkit-catalog generate",
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
        std::fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return usage();
    };
    let mut dir: Option<PathBuf> = None;
    let (mut target, mut out, mut only_check) = (None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => target = args.next().and_then(|t| Target::parse(&t)),
            "--out" => out = args.next().map(PathBuf::from),
            "--check" => only_check = true,
            other if other.starts_with("--") => return usage(),
            other => dir = Some(PathBuf::from(other)),
        }
    }
    let Some(dir) = dir else {
        return usage();
    };
    let result = match command.as_str() {
        "check" => complete(&dir).map(|_| ExitCode::SUCCESS),
        "generate" => match (target, out) {
            (Some(target), Some(out)) => run_generate(&dir, target, &out, only_check),
            _ => return usage(),
        },
        _ => return usage(),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("rightkit-catalog: {message}");
            ExitCode::from(1)
        }
    }
}
