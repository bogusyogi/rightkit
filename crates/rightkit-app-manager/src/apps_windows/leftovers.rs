//! Leftovers of an installed app on Windows, for an Apps page.
//!
//! Windows has no bundle id, so matching is by names, strongest first:
//!  * `install` - the folder the app registered as its install location (the
//!    app's own uninstaller removes it; listed so the page shows what goes).
//!  * `publisher` - `<root>\<publisher>\<product>`: the publisher folder is shared
//!    with the publisher's other products and is never listed itself, only the
//!    product folder inside it. Selected by default.
//!  * `name` - a folder directly in a root named like the product. Listed but
//!    left unchecked ("Review before moving").
//!    Roots searched: `%APPDATA%`, `%LOCALAPPDATA%` (and its `Programs`),
//!    `%USERPROFILE%\AppData\LocalLow`, `%ProgramData%`.
//!
//! Registry keys (`HKCU`/`HKLM\Software\<publisher>\<product>`) and startup
//! entries (Run keys) are only listed, as read-only background entries. Registry data is never
//! deleted. Links and junctions are never followed or listed. The registry is read through the
//! host's `RegistryRead`, and the label of a listed registry row is the host's wording.

use std::collections::HashSet;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Installed;
use super::usage::norm;
use crate::apps::registry::{Hive, RegistryRead, View};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const WALK_BUDGET: u64 = 200_000;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Folder names shared by many programs; never a match by themselves.
const GENERIC: [&str; 22] = [
    "microsoft",
    "windows",
    "temp",
    "packages",
    "programs",
    "apps",
    "local",
    "roaming",
    "data",
    "cache",
    "logs",
    "crashdumps",
    "history",
    "comms",
    "d3dscache",
    "publishers",
    "common",
    "commonfiles",
    "software",
    "system",
    "setup",
    "installer",
];

const COMPANY_WORDS: [&str; 14] = [
    "inc",
    "llc",
    "ltd",
    "corp",
    "corporation",
    "gmbh",
    "co",
    "company",
    "limited",
    "ag",
    "sa",
    "bv",
    "plc",
    "foundation",
];

/// One related folder: the fields of the Apps page's related item.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub path: String,
    pub label: String,
    pub location: String,
    pub exact: bool,
    pub confidence: String,
    pub reason: String,
    pub admin: bool,
    pub size_bytes: u64,
    pub preselected: bool,
    /// The root folder this item was found under; pinned with it, never sent to the page.
    #[serde(skip)]
    pub root: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct Background {
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
}

/// One source's share of the leftovers (the macOS event shape).
#[derive(Clone, Debug, Serialize)]
pub struct Part {
    pub source: String,
    pub items: Vec<Item>,
    pub background: Vec<Background>,
    pub receipts: Vec<String>,
}

struct Root {
    dir: PathBuf,
    label: &'static str,
    location: &'static str,
    admin: bool,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

fn roots() -> Vec<Root> {
    let mut out = Vec::new();
    let mut add =
        |dir: Option<PathBuf>, label: &'static str, location: &'static str, admin: bool| {
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                out.push(Root {
                    dir,
                    label,
                    location,
                    admin,
                });
            }
        };
    add(
        env_dir("APPDATA"),
        "AppData\\Roaming",
        "Your app data",
        false,
    );
    add(
        env_dir("LOCALAPPDATA"),
        "AppData\\Local",
        "Your app data",
        false,
    );
    add(
        env_dir("LOCALAPPDATA").map(|p| p.join("Programs")),
        "AppData\\Local\\Programs",
        "Your app data",
        false,
    );
    add(
        env_dir("USERPROFILE").map(|p| p.join("AppData").join("LocalLow")),
        "AppData\\LocalLow",
        "Your app data",
        false,
    );
    add(
        env_dir("ProgramData"),
        "ProgramData",
        "Shared app data",
        true,
    );
    out
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Bytes under `path` (allocated size is not read here; this is file length),
/// without following links, within an entry budget.
fn dir_size(path: &Path) -> u64 {
    let Ok(top) = path.symlink_metadata() else {
        return 0;
    };
    if is_reparse(&top) {
        return 0;
    }
    if top.is_file() {
        return top.len();
    }
    let mut total = 0u64;
    let mut budget = WALK_BUDGET;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return total;
            }
            budget -= 1;
            let Ok(metadata) = entry.path().symlink_metadata() else {
                continue;
            };
            if is_reparse(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

fn looks_like_guid(text: &str) -> bool {
    text.starts_with('{') || (text.len() == 36 && text.matches('-').count() == 4)
}

/// "Foo Editor 2.3.1 (x64)" -> "Foo Editor".
fn product_words(display: &str) -> String {
    let mut depth = 0i32;
    let mut kept: Vec<&str> = Vec::new();
    for word in display.split_whitespace() {
        let opens = word.matches('(').count() as i32;
        let closes = word.matches(')').count() as i32;
        let inside = depth > 0 || opens > 0;
        depth += opens - closes;
        if inside {
            continue;
        }
        let lower = word.to_lowercase();
        let first = lower.chars().next().unwrap_or(' ');
        let second = lower.chars().nth(1).unwrap_or(' ');
        if lower == "version"
            || lower == "x64"
            || lower == "x86"
            || first.is_ascii_digit()
            || (first == 'v' && second.is_ascii_digit())
        {
            continue;
        }
        kept.push(word);
    }
    if kept.is_empty() {
        display.to_string()
    } else {
        kept.join(" ")
    }
}

fn publisher_norm(publisher: &str) -> String {
    let words: Vec<&str> = publisher
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|w| w.trim_matches('.'))
        .filter(|w| !w.is_empty() && !COMPANY_WORDS.contains(&w.to_lowercase().as_str()))
        .collect();
    norm(&words.join(" "))
}

fn usable(name: &str) -> bool {
    name.len() >= 4 && !GENERIC.contains(&name)
}

/// The names (normalised) this app's folders and keys are expected to carry.
fn product_names(app: &Installed) -> HashSet<String> {
    let mut names = HashSet::new();
    names.insert(norm(&product_words(&app.entry.name)));
    names.insert(norm(&app.entry.name));
    if let Some(folder) = &app.folder
        && let Some(base) = Path::new(folder).file_name()
    {
        names.insert(norm(&base.to_string_lossy()));
    }
    if !looks_like_guid(&app.key_name) {
        let key = app.key_name.trim_end_matches("_is1");
        names.insert(norm(key));
    }
    names.retain(|n| usable(n));
    names
}

fn publisher_of(app: &Installed) -> Option<String> {
    app.publisher
        .as_deref()
        .map(publisher_norm)
        .filter(|p| usable(p))
}

fn under_program_files(path: &str) -> bool {
    let lower = path.to_lowercase();
    [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ]
    .iter()
    .any(|var| {
        std::env::var(var)
            .map(|dir| !dir.is_empty() && lower.starts_with(&dir.to_lowercase()))
            .unwrap_or(false)
    })
}

/// The "Application" row: the install folder, or the registry registration when
/// no folder is known. The app's own uninstaller removes both.
pub fn bundle_part(app: &Installed) -> Part {
    let path = app.entry.path.clone();
    let is_folder = Path::new(&path).is_dir();
    let (size_bytes, reason) = if is_folder {
        (
            dir_size(Path::new(&path)),
            "Install folder, removed by the app's own uninstaller".to_string(),
        )
    } else {
        (
            app.entry.size_bytes,
            "Registered in Windows; removed by the app's own uninstaller".to_string(),
        )
    };
    Part {
        source: "bundle".into(),
        items: vec![Item {
            admin: is_folder && under_program_files(&path),
            label: "Application".into(),
            location: "Application".into(),
            exact: true,
            confidence: "install".into(),
            reason,
            size_bytes,
            preselected: app.entry.protected.is_none(),
            // The app's own uninstaller removes this row; it is never moved from here, so its root
            // is only the folder it sits in.
            root: Path::new(&path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            path,
        }],
        background: Vec::new(),
        receipts: Vec::new(),
    }
}

/// What the scan reads the registry through, and the host's wording for a listed registry row.
pub struct ScanRegistry<'a> {
    pub registry: &'a dyn RegistryRead,
    /// The `kind` of a listed registry row, for example "Registry (listed only, never removed by
    /// `<app>`)".
    pub registry_kind: &'a str,
}

fn registry_hits(
    registry: &dyn RegistryRead,
    products: &HashSet<String>,
    publisher: Option<&str>,
) -> Vec<String> {
    let views = [
        ("HKCU", Hive::CurrentUser, View::Wow64),
        ("HKLM", Hive::LocalMachine, View::Wow64),
        ("HKLM (32-bit view)", Hive::LocalMachine, View::Wow32),
    ];
    let mut hits = Vec::new();
    for (label, hive, view) in views {
        let Some(names) = registry.subkey_names(hive, view, "Software") else {
            continue;
        };
        for name in names {
            let n = norm(&name);
            if n.is_empty() || GENERIC.contains(&n.as_str()) {
                continue;
            }
            if products.contains(&n) {
                hits.push(format!("{label}\\Software\\{name}"));
            } else if publisher == Some(n.as_str())
                && let Some(children) =
                    registry.subkey_names(hive, view, &format!("Software\\{name}"))
            {
                for child in children {
                    if products.contains(&norm(&child)) {
                        hits.push(format!("{label}\\Software\\{name}\\{child}"));
                    }
                }
            }
        }
    }
    hits
}

fn startup_hits(
    registry: &dyn RegistryRead,
    app: &Installed,
    products: &HashSet<String>,
) -> Vec<Background> {
    let folder = app.folder.as_ref().map(|f| f.to_lowercase());
    let views = [
        ("HKCU", Hive::CurrentUser, View::Wow64),
        ("HKLM", Hive::LocalMachine, View::Wow64),
    ];
    let mut out = Vec::new();
    for (hive_label, hive, view) in views {
        let Some(values) = registry.values(hive, view, RUN_KEY) else {
            continue;
        };
        for value in values.into_iter().flatten() {
            let name = value.name.clone();
            let data = value.text().unwrap_or_default();
            let by_folder = folder
                .as_ref()
                .is_some_and(|f| data.to_lowercase().contains(f.as_str()));
            if by_folder || products.contains(&norm(&name)) {
                out.push(Background {
                    kind: format!("Startup ({hive_label} Run key)"),
                    label: name,
                    path: (!data.is_empty()).then_some(data),
                });
            }
        }
    }
    out
}

/// True when `candidate` is another installed app's folder, or holds one, or is inside one.
fn collides(candidate: &Path, app: &Installed, all: &[Installed]) -> bool {
    let lower = candidate.to_string_lossy().to_lowercase();
    all.iter()
        .filter(|other| other.key_name != app.key_name || other.entry.path != app.entry.path)
        .filter_map(|other| other.folder.as_ref())
        .map(|folder| folder.to_lowercase())
        .any(|folder| {
            folder == lower
                || folder.starts_with(&format!("{lower}\\"))
                || lower.starts_with(&format!("{folder}\\"))
        })
}

fn child_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.path().symlink_metadata().ok()?;
            (metadata.is_dir() && !is_reparse(&metadata)).then(|| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.path(),
                )
            })
        })
        .collect()
}

/// Folders and listed-only entries for one app. `emit` gets each source as it
/// completes. Returns the folders offered as leftovers, each pinned by identity.
pub fn scan(
    app: &Installed,
    all: &[Installed],
    registry: &ScanRegistry<'_>,
    emit: &dyn Fn(Part),
) -> Vec<Pinned> {
    let products = product_names(app);
    let publisher = publisher_of(app);
    let own = app.folder.as_ref().map(|f| f.to_lowercase());
    let mut items: Vec<Item> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |path: PathBuf,
                    root: &Root,
                    confidence: &str,
                    reason: String,
                    preselected: bool,
                    items: &mut Vec<Item>| {
        let lower = path.to_string_lossy().to_lowercase();
        if own.as_deref() == Some(lower.as_str())
            || !seen.insert(lower)
            || collides(&path, app, all)
        {
            return;
        }
        let size_bytes = dir_size(&path);
        items.push(Item {
            path: path.to_string_lossy().into_owned(),
            label: root.label.into(),
            location: root.location.into(),
            exact: false,
            confidence: confidence.into(),
            reason,
            admin: root.admin,
            size_bytes,
            preselected,
            root: root.dir.clone(),
        });
    };
    if !products.is_empty() {
        for root in roots() {
            for (name, path) in child_dirs(&root.dir) {
                let n = norm(&name);
                if n.is_empty() || GENERIC.contains(&n.as_str()) {
                    continue;
                }
                if products.contains(&n) {
                    push(
                        path,
                        &root,
                        "name",
                        format!("A folder named like {}", app.entry.name),
                        false,
                        &mut items,
                    );
                } else if publisher.as_deref() == Some(n.as_str()) {
                    for (inner_name, inner) in child_dirs(&path) {
                        if products.contains(&norm(&inner_name)) {
                            push(
                                inner,
                                &root,
                                "publisher",
                                format!("{name}\\{inner_name}: the publisher and product match"),
                                true,
                                &mut items,
                            );
                        }
                    }
                }
            }
        }
    }
    items.sort_by_key(|a| a.path.to_lowercase());
    emit(Part {
        source: "library".into(),
        items: items.clone(),
        background: Vec::new(),
        receipts: Vec::new(),
    });

    let mut background: Vec<Background> =
        registry_hits(registry.registry, &products, publisher.as_deref())
            .into_iter()
            .map(|key| Background {
                kind: registry.registry_kind.into(),
                label: key,
                path: None,
            })
            .collect();
    background.extend(startup_hits(registry.registry, app, &products));
    emit(Part {
        source: "background".into(),
        items: Vec::new(),
        background,
        receipts: Vec::new(),
    });
    // Only an item whose identity can be pinned may be moved later; the rest stay listed only.
    items
        .iter()
        .filter_map(|item| Pinned::new(&item.path, &item.root))
        .collect()
}

/// A folder's volume serial and file id, when it is a plain directory (not a link or
/// junction) on a volume that gives files a stable identity.
fn dir_key(path: &Path) -> Option<(String, String)> {
    let metadata = path.symlink_metadata().ok()?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return None;
    }
    let info = rightkit_disk_cleanup::platform::inspect(path, &metadata);
    if !info.volume_stable {
        return None;
    }
    let id = info.file_id?;
    Some((id.volume.id, id.id))
}

/// A leftover folder as the scan found it, pinned by what it is, not by its path text: the
/// canonical path, and the volume serial and file id of the item and of every folder between
/// it and its root. A later move is allowed only while all of them still match, so a folder
/// that was replaced, or one reached through a changed ancestor, is refused.
#[derive(Clone, Debug)]
pub struct Pinned {
    path: String,
    canonical: PathBuf,
    /// The root's canonical path, then each folder down to the item, with its identity.
    chain: Vec<(PathBuf, (String, String))>,
}

impl Pinned {
    /// None when the folder cannot be proven to be a plain folder strictly inside `root` with
    /// no link in between (unknown stays refused).
    fn new(path: &str, root: &Path) -> Option<Pinned> {
        let listed = Path::new(path);
        let root_canonical = std::fs::canonicalize(root).ok()?;
        let canonical = std::fs::canonicalize(listed).ok()?;
        let relative = canonical.strip_prefix(&root_canonical).ok()?;
        if relative.as_os_str().is_empty() {
            return None;
        }
        // The path as listed must be the real path under the root: no junction or link on the way.
        let listed_relative = listed.strip_prefix(root).ok()?;
        if relative.to_string_lossy().to_lowercase()
            != listed_relative.to_string_lossy().to_lowercase()
        {
            return None;
        }
        let mut chain = Vec::new();
        let mut step = root_canonical.clone();
        chain.push((step.clone(), dir_key(&step)?));
        for part in relative.components() {
            step.push(part);
            chain.push((step.clone(), dir_key(&step)?));
        }
        Some(Pinned {
            path: path.to_string(),
            canonical,
            chain,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Ok when the item is still the same plain folder under the same unchanged ancestors.
    /// Err says why not; a folder that no longer exists is reported as gone.
    pub fn still_same(&self) -> Result<(), String> {
        let listed = Path::new(&self.path);
        if listed.symlink_metadata().is_err() {
            return Err("Already gone.".into());
        }
        match std::fs::canonicalize(listed) {
            Ok(now) if now == self.canonical => {}
            _ => {
                return Err("The folder's location changed since it was found; left alone.".into());
            }
        }
        for (step, key) in &self.chain {
            if dir_key(step).as_ref() != Some(key) {
                return Err(
                    "The folder, or a folder above it, was replaced or is now a link; left alone."
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// True when another installed app's folder is this folder, holds it, or is inside it.
    pub fn collides_with(&self, app: &Installed, all: &[Installed]) -> bool {
        collides(Path::new(&self.path), app, all)
    }
}

/// Size used when a leftover is moved, measured again right before the move.
pub fn size_now(path: &Path) -> u64 {
    dir_size(path)
}
