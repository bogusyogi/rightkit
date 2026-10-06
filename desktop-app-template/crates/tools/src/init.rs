//! Rename the template into a real app, in place:
//!   rightapp-tools init --slug heardright2 --name "HeardRight2" --org app.orthiclabs
//! `slug`: lowercase letters/digits only (it becomes the Rust lib name and sidecar prefix).
//! Rewrites the template identity in text files, then re-runs the gate.
use crate::{check, Result};
use std::path::{Path, PathBuf};

const SKIP_DIRS: [&str; 5] = ["node_modules", "target", ".git", "dist", "gen"];
const TEXT_EXT: [&str; 11] = ["json", "toml", "mjs", "ts", "tsx", "rs", "md", "html", "css", "yaml", "yml"];

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == &format!("--{name}")).and_then(|i| args.get(i + 1)).cloned()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(&p, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).map(|x| TEXT_EXT.contains(&x)).unwrap_or(false) {
            out.push(p);
        }
    }
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let usage = "usage: rightapp-tools init --slug <lowercase> --name <Product Name> [--org <reverse.dns.prefix>]";
    let (Some(slug), Some(name)) = (opt(args, "slug"), opt(args, "name")) else { return Err(usage.into()) };
    let org = opt(args, "org").unwrap_or_else(|| "app.rightsuite".into());
    let slug_ok = slug.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false) && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let name_ok = name.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ');
    if !slug_ok || !name_ok {
        return Err(usage.into());
    }
    // The template identity, assembled so this file is not rewritten into the new identity.
    let (title, lower, upper) = (["Right", "App"].concat(), ["right", "app"].concat(), ["RIGHT", "APP"].concat());
    let ident = format!("app.rightsuite.{lower}");
    let mut files = vec![];
    walk(root, &mut files);
    for file in files {
        if file.ends_with("crates/tools/src/init.rs") {
            continue;
        }
        let Ok(before) = std::fs::read_to_string(&file) else { continue };
        let after = before
            .replace(&ident, &format!("{org}.{slug}"))
            .replace(&upper, &slug.to_uppercase())
            .replace(&title, &name)
            .replace(&lower, &slug);
        if after != before {
            std::fs::write(&file, after).map_err(|e| format!("{}: {e}", file.display()))?;
        }
    }
    if name.replace(' ', "").to_lowercase() != slug {
        eprintln!("warning: --name \"{name}\" lowercased != slug \"{slug}\"; check productName/installer names.");
    }
    check::run(root)
}
