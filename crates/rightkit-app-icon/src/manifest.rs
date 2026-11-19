//! Pure matching and `AppxManifest.xml` helpers (cross-platform so they are unit-testable), and the
//! bounded, contained loader of a packaged app's logo.

use crate::limits::{self, Budget, Reject, Sink};
use rightkit_fs::Roots;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Whether a running image name (`Code.exe`) is the process a history row named
/// (`Code.exe`, `code`, `CODE.EXE`). Case-insensitive, `.exe`-suffix tolerant.
pub fn process_names_match(running_image: &str, wanted: &str) -> bool {
    fn norm(name: &str) -> String {
        let lower = name.trim().to_lowercase();
        lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
    }
    let wanted = norm(wanted);
    !wanted.is_empty() && norm(running_image) == wanted
}

/// Last path component, splitting on both `\` and `/`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn file_name_of(path: &str) -> &str {
    path.trim()
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
}

/// Whether a shortcut / App Paths target executable is the wanted process.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn target_matches_name(target: &str, wanted: &str) -> bool {
    process_names_match(file_name_of(target), wanted)
}

/// `App Paths` subkey for a process name (`ChatGPT` -> `...\App Paths\ChatGPT.exe`).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn app_paths_subkey(process_name: &str) -> Option<String> {
    let name = file_name_of(process_name);
    if name.is_empty() {
        return None;
    }
    let exe = if name.to_lowercase().ends_with(".exe") {
        name.to_string()
    } else {
        format!("{name}.exe")
    };
    Some(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"
    ))
}

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ManifestApp {
    pub executable: String,
    /// Relative logo paths in preference order (Square44 first).
    pub logos: Vec<String>,
}

fn xml_attr(tag: &str, wanted: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut i = 0;
    while let Some(rel) = tag[i..].find(wanted) {
        let start = i + rel;
        let end = start + wanted.len();
        let boundary_ok =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let rest = tag[end..].trim_start();
        if boundary_ok && rest.starts_with('=') {
            let rest = rest[1..].trim_start();
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                let inner = &rest[1..];
                return inner.find(quote).map(|e| inner[..e].to_string());
            }
        }
        i = end;
    }
    None
}

/// Dependency-light scan of an `AppxManifest.xml`: each `<Application
/// Executable=..>` with the logos of its `<VisualElements>`. At most
/// `limits::MAX_MANIFEST_APPS` applications are read, and a logo value longer than
/// `limits::MAX_LOGO_PATH_BYTES` is dropped. The input is already capped by the caller.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn parse_manifest_apps(xml: &str) -> Vec<ManifestApp> {
    let mut apps: Vec<ManifestApp> = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[..close];
        rest = &rest[close + 1..];
        if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tag.len());
        let local = tag[..name_end].rsplit(':').next().unwrap_or("");
        match local {
            "Application" => {
                if apps.len() >= limits::MAX_MANIFEST_APPS {
                    break;
                }
                apps.push(ManifestApp {
                    executable: xml_attr(tag, "Executable").unwrap_or_default(),
                    logos: Vec::new(),
                });
            }
            "VisualElements" => {
                if let Some(app) = apps.last_mut() {
                    for attr in ["Square44x44Logo", "Square150x150Logo"] {
                        if let Some(value) = xml_attr(tag, attr)
                            .filter(|v| !v.is_empty() && v.len() <= limits::MAX_LOGO_PATH_BYTES)
                        {
                            app.logos.push(value);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    apps
}

/// Logos of the manifest application whose executable matches `exe_name`; when
/// none matches and `allow_any`, the first application that has logos.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn manifest_logos_for(
    xml: &str,
    exe_name: &str,
    allow_any: bool,
) -> Option<Vec<String>> {
    let apps = parse_manifest_apps(xml);
    let matched = apps.iter().find(|a| {
        !a.executable.is_empty()
            && target_matches_name(&a.executable, exe_name)
            && !a.logos.is_empty()
    });
    let chosen = matched.or_else(|| {
        if allow_any {
            apps.iter().find(|a| !a.logos.is_empty())
        } else {
            None
        }
    })?;
    Some(chosen.logos.clone())
}

/// Parse a `.scale-N` / `.targetsize-N` qualifier value: `None` when it is not a number or is
/// outside the accepted range (`1..=max`), which rejects the candidate that carries it.
fn qualifier_value(text: &str, max: u32) -> Option<u32> {
    let n = text.parse::<u32>().ok()?;
    (1..=max).contains(&n).then_some(n)
}

/// Pick the best on-disk variant of a manifest logo (`Assets\Square44x44Logo.png`)
/// from the directory's file names: unqualified, `.scale-N` (relative to the
/// logo's nominal size) or `.targetsize-N`, preferring the smallest >= 48px,
/// unplated over plated, never contrast / light-unplated variants.
///
/// All qualifier arithmetic is checked: a candidate whose qualifier is not a number in range,
/// whose computed size overflows or is outside `1..=max_edge`, or whose score overflows is
/// skipped (never wrapped, never a panic). `max_edge` skips variants larger than the caller will
/// accept, so a smaller acceptable variant can still win.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn pick_logo_variant(logo_rel: &str, files: &[String], max_edge: u32) -> Option<String> {
    let file = file_name_of(logo_rel);
    let (stem, ext) = file.rsplit_once('.')?;
    let stem = stem.to_lowercase();
    let suffix = format!(".{}", ext.to_lowercase());
    let nominal: u32 = if stem.contains("150") { 150 } else { 44 };
    let size_limit = max_edge.min(limits::MAX_QUALIFIER_SIZE);
    let mut best: Option<(u32, String)> = None;
    'candidates: for candidate in files {
        let lower = candidate.to_lowercase();
        let Some(tail) = lower
            .strip_prefix(&stem)
            .and_then(|t| t.strip_suffix(&suffix))
        else {
            continue;
        };
        if !(tail.is_empty() || tail.starts_with('.') || tail.starts_with('_')) {
            continue;
        }
        if lower.contains("contrast") || lower.contains("lightunplated") {
            continue;
        }
        let mut size = nominal;
        for part in tail.split(['.', '_']) {
            if let Some(n) = part.strip_prefix("targetsize-") {
                match qualifier_value(n, limits::MAX_QUALIFIER_SIZE) {
                    Some(n) => size = n,
                    None => continue 'candidates,
                }
            } else if let Some(n) = part.strip_prefix("scale-") {
                let Some(n) = qualifier_value(n, limits::MAX_QUALIFIER_SCALE) else {
                    continue 'candidates;
                };
                match nominal.checked_mul(n) {
                    Some(scaled) => size = scaled / 100,
                    None => continue 'candidates,
                }
            }
        }
        if size == 0 || size > size_limit {
            continue;
        }
        let distance = if size >= 48 {
            size - 48
        } else {
            10_000 + (48 - size)
        };
        let plated = u32::from(!lower.contains("unplated") && lower.contains("targetsize-"));
        let Some(score) = distance.checked_mul(2).and_then(|d| d.checked_add(plated)) else {
            continue;
        };
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, candidate.clone()));
        }
    }
    best.map(|(_, name)| name)
}

/// No component of `root/dir_components` may be a symbolic link or reparse point, and each must
/// exist as a directory.
fn dir_chain_has_no_links(root: &Path, dir_components: &[String]) -> bool {
    let mut cur = root.to_path_buf();
    for component in dir_components {
        cur.push(component);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if !limits::is_link_or_reparse(&meta) && meta.is_dir() => {}
            _ => return false,
        }
    }
    true
}

/// File names (regular files only; links and reparse points are skipped) of `dir`, within
/// the per-directory entry cap and the shared budget.
fn list_logo_dir(dir: &Path, budget: &mut Budget) -> Result<Vec<String>, Reject> {
    let entries = std::fs::read_dir(dir).map_err(|_| Reject::Unreadable)?;
    let mut names = Vec::new();
    let mut seen = 0u32;
    for item in entries {
        seen += 1;
        if seen > limits::MAX_LOGO_DIR_ENTRIES {
            return Err(Reject::BudgetEntries);
        }
        budget.charge(Instant::now()).map_err(|s| s.reject())?;
        let Ok(entry) = item else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || limits::is_link_or_reparse(&meta) {
            continue;
        }
        if let Ok(name) = entry.file_name().into_string() {
            if name.len() <= 255 {
                names.push(name);
            }
        }
    }
    Ok(names)
}

/// Load the best manifest logo of `package_root` for `exe_name` (PNG bytes).
///
/// Every read is capped (`MAX_MANIFEST_BYTES`, `MAX_LOGO_BYTES`; oversize is "no icon", never a
/// partial read). A logo path from the manifest is screened (`limits::logo_rel_components`),
/// joined under the package root only through `rightkit_fs::Roots`, its directory chain is
/// checked for links and reparse points, and both files (manifest and logo) are opened with
/// `Roots::open_confined(.., &limits::confined_read_options())`: no link followed, nothing outside
/// the root, and `require_single_link(true)`, so a hard-linked manifest or logo is refused
/// (`link_or_escape`, "no icon"). The package root is not treated as trusted: it comes from a
/// user-writable registry key or a host path, and nothing here can establish that package files
/// are immutable. The PNG is validated in full (structure, CRCs, dimensions against `max_edge`
/// and the pixel cap, and the inflated IDAT stream against the exact decoded length) before the
/// bytes are returned. Rejections go to `sink`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn packaged_logo_png(
    package_root: &Path,
    exe_name: &str,
    allow_any: bool,
    max_edge: u32,
    sink: Sink,
    budget: &mut Budget,
) -> Option<Vec<u8>> {
    if let Err(stop) = budget.charge(Instant::now()) {
        limits::report(sink, "package", stop.reject());
        return None;
    }
    let roots = Roots::with_roots([package_root]).ok()?;
    let root: PathBuf = roots.roots().first()?.clone();
    // Manifest and logo are read under the same policy: no link followed, single hard link only.
    let options = limits::confined_read_options();

    let bytes = match limits::read_confined(
        &roots,
        &root.join("AppxManifest.xml"),
        &options,
        limits::MAX_MANIFEST_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(reject) => {
            limits::report(sink, "manifest", reject);
            return None;
        }
    };
    let xml = match String::from_utf8(bytes) {
        Ok(xml) => xml,
        Err(_) => {
            limits::report(sink, "manifest", Reject::NotText);
            return None;
        }
    };
    for logo in manifest_logos_for(&xml, exe_name, allow_any)? {
        match logo_png(&roots, &root, &logo, max_edge, budget) {
            Ok(png) => return Some(png),
            Err(reject) => {
                limits::report(sink, "logo", reject);
                if matches!(reject, Reject::BudgetEntries | Reject::BudgetTime) {
                    return None;
                }
            }
        }
    }
    None
}

fn logo_png(
    roots: &Roots,
    root: &Path,
    logo: &str,
    max_edge: u32,
    budget: &mut Budget,
) -> Result<Vec<u8>, Reject> {
    let components = limits::logo_rel_components(logo)?;
    let (file_component, dir_components) = components.split_last().ok_or(Reject::BadPath)?;
    if !dir_chain_has_no_links(root, dir_components) {
        return Err(Reject::LinkOrEscape);
    }
    let mut dir_path = root.to_path_buf();
    dir_path.extend(dir_components);
    let dir = roots
        .resolve_confined(&dir_path)
        .map_err(|e| Reject::from_contain(&e))?;
    let files = list_logo_dir(&dir, budget)?;
    let best = pick_logo_variant(file_component, &files, max_edge).ok_or(Reject::Unreadable)?;
    let bytes = limits::read_confined(
        roots,
        &dir.join(best),
        &limits::confined_read_options(),
        limits::MAX_LOGO_BYTES,
    )?;
    limits::validate_png(&bytes, max_edge, limits::MAX_LOGO_BYTES)?;
    Ok(bytes)
}
