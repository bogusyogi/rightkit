use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_app_settings::agent_config::okf::{emit_bundle, OkfConcept, OkfEmitOptions, OkfLink};

use crate::api::{now_iso, DaemonState};

const UNDERSTANDING_DIR: &str = ".coderight/understanding";
const MAX_MAPPED_FILES: usize = 2_000;

pub(crate) fn spawn_workspace_blueprint_init(
    state: DaemonState,
    session_id: String,
    workspace_root: String,
) {
    tokio::spawn(async move {
        let root = PathBuf::from(workspace_root.trim());
        if root.as_os_str().is_empty() {
            return;
        }

        match emit_bootstrap_understanding(&root) {
            Ok(report) => tracing::info!(
                session_id = %session_id,
                workspace_root = %root.display(),
                output_dir = %report.out_dir.display(),
                first_open = report.first_open,
                drifted = report.drifted,
                fingerprint = %report.fingerprint,
                "blueprint deterministic workspace map ready"
            ),
            Err(err) => {
                tracing::warn!(
                    session_id = %session_id,
                    workspace_root = %root.display(),
                    error = %err,
                    "blueprint init failed"
                );
                publish_failure(&state, &session_id, err);
            }
        }
    });
}

#[derive(Debug, Clone)]
struct BlueprintReport {
    out_dir: PathBuf,
    first_open: bool,
    drifted: bool,
    fingerprint: String,
}

fn emit_bootstrap_understanding(root: &Path) -> Result<BlueprintReport, String> {
    let coderight_dir = root.join(".coderight");
    fs::create_dir_all(&coderight_dir)
        .map_err(|err| format!("create RightKit state directory: {err}"))?;
    let gitignore = coderight_dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")
            .map_err(|err| format!("ignore RightKit workspace state: {err}"))?;
    }
    let out_dir = root.join(UNDERSTANDING_DIR);
    let previous = read_previous_fingerprint(&out_dir);
    let first_open = previous.is_none();
    let map = RepoMap::scan(root)?;
    let drifted = previous
        .as_deref()
        .is_some_and(|fingerprint| fingerprint != map.fingerprint);
    let concepts = concepts_for_map(root, &out_dir, &map, first_open, drifted);

    emit_bundle(&out_dir, &concepts, OkfEmitOptions::default())
        .map_err(|err| format!("emit OKF understanding: {err}"))?;

    Ok(BlueprintReport {
        out_dir,
        first_open,
        drifted,
        fingerprint: map.fingerprint,
    })
}

#[derive(Debug)]
struct RepoMap {
    fingerprint: String,
    files_seen: usize,
    files_mapped: usize,
    by_top_level: BTreeMap<String, ModuleStats>,
}

impl RepoMap {
    fn scan(root: &Path) -> Result<Self, String> {
        let mut files = Vec::new();
        collect_files(root, root, &mut files)?;
        files.sort_by(|a, b| a.relative.cmp(&b.relative));

        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        let mut by_top_level = BTreeMap::<String, ModuleStats>::new();
        for entry in files.iter().take(MAX_MAPPED_FILES) {
            hasher.update(entry.relative.as_bytes());
            hasher.update(&[0]);
            hasher.update(entry.len.to_string().as_bytes());
            hasher.update(&[0]);
            hasher.update(entry.modified_unix.to_string().as_bytes());
            hasher.update(b"\n");

            let top = entry
                .relative
                .split('/')
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or(".")
                .to_string();
            let ext = Path::new(&entry.relative)
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .unwrap_or_else(|| "(none)".to_string());
            by_top_level.entry(top).or_default().record(&ext, entry.len);
        }

        Ok(Self {
            fingerprint: hasher.finalize(),
            files_seen: files.len(),
            files_mapped: files.len().min(MAX_MAPPED_FILES),
            by_top_level,
        })
    }
}

#[derive(Debug, Default)]
struct ModuleStats {
    files: usize,
    bytes: u64,
    extensions: BTreeMap<String, usize>,
}

impl ModuleStats {
    fn record(&mut self, ext: &str, len: u64) {
        self.files += 1;
        self.bytes += len;
        *self.extensions.entry(ext.to_string()).or_default() += 1;
    }
}

#[derive(Debug)]
struct FileEntry {
    relative: String,
    len: u64,
    modified_unix: u64,
}

fn collect_files(root: &Path, dir: &Path, files: &mut Vec<FileEntry>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|err| format!("scan {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("scan {}: {err}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if should_skip(&name, root, &path) {
            continue;
        }

        let metadata = entry
            .metadata()
            .map_err(|err| format!("stat {}: {err}", path.display()))?;
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let modified_unix = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            files.push(FileEntry {
                relative,
                len: metadata.len(),
                modified_unix,
            });
        }
    }
    Ok(())
}

fn should_skip(name: &str, root: &Path, path: &Path) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "dist" | "build" | ".next" | ".cache"
    ) || path == root.join(UNDERSTANDING_DIR)
        || path.starts_with(root.join(UNDERSTANDING_DIR))
}

fn concepts_for_map(
    root: &Path,
    out_dir: &Path,
    map: &RepoMap,
    first_open: bool,
    drifted: bool,
) -> Vec<OkfConcept> {
    let timestamp = now_iso();
    let mut concepts = vec![OkfConcept {
        name: "repo-map".to_string(),
        kind: "repo-map".to_string(),
        title: Some("Repository Map".to_string()),
        description: Some("Bootstrap repository understanding produced on workspace open.".into()),
        resource: Some(root.display().to_string()),
        timestamp: Some(timestamp.clone()),
        tags: vec!["blueprint".into(), "workspace-init".into()],
        body: format!(
            concat!(
                "# Repository Map\n\n",
                "- workspace: `{}`\n",
                "- fingerprint: `{}`\n",
                "- files mapped: {} of {}\n",
                "- lifecycle: {}\n",
                "- drift: {}\n\n",
                "This bootstrap map is emitted before the conducted blueprint run so ",
                "first interaction stays non-blocking. The background conductor should ",
                "refine these concepts into module, interface, and claim-level notes."
            ),
            root.display(),
            map.fingerprint,
            map.files_mapped,
            map.files_seen,
            if first_open { "first-open" } else { "re-open" },
            if drifted { "flagged" } else { "none" }
        ),
        links: map
            .by_top_level
            .keys()
            .map(|name| OkfLink {
                title: format!("Module {name}"),
                target: format!("module-{}.md", concept_slug(name)),
            })
            .collect(),
    }];

    for (name, stats) in &map.by_top_level {
        concepts.push(module_concept(name, stats, &timestamp));
    }

    if drifted {
        concepts.push(OkfConcept {
            name: "drift".to_string(),
            kind: "drift".to_string(),
            title: Some("Understanding Drift".to_string()),
            description: Some("Current repository map differs from the stored OKF map.".into()),
            resource: Some(out_dir.display().to_string()),
            timestamp: Some(timestamp),
            tags: vec!["blueprint".into(), "drift".into()],
            body: format!(
                concat!(
                    "# Understanding Drift\n\n",
                    "The workspace changed since the last OKF understanding bundle was written.\n\n",
                    "- current fingerprint: `{}`\n",
                    "- action: background blueprint re-verification requested\n"
                ),
                map.fingerprint
            ),
            links: vec![OkfLink {
                title: "Repository Map".to_string(),
                target: "repo-map.md".to_string(),
            }],
        });
    }

    concepts
}

fn module_concept(name: &str, stats: &ModuleStats, timestamp: &str) -> OkfConcept {
    let extensions = stats
        .extensions
        .iter()
        .map(|(ext, count)| format!("- `{ext}`: {count}"))
        .collect::<Vec<_>>()
        .join("\n");
    OkfConcept {
        name: format!("module-{}", concept_slug(name)),
        kind: "module".to_string(),
        title: Some(name.to_string()),
        description: Some(format!("Bootstrap map for `{name}`.")),
        resource: Some(name.to_string()),
        timestamp: Some(timestamp.to_string()),
        tags: vec!["blueprint".into(), "module".into()],
        body: format!(
            "# {name}\n\n- files: {}\n- bytes: {}\n\n## Extensions\n{}",
            stats.files, stats.bytes, extensions
        ),
        links: vec![OkfLink {
            title: "Repository Map".to_string(),
            target: "repo-map.md".to_string(),
        }],
    }
}

fn read_previous_fingerprint(out_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(out_dir.join("repo-map.md")).ok()?;
    text.lines()
        .find_map(|line| line.trim().strip_prefix("- fingerprint: `"))
        .and_then(|rest| rest.strip_suffix('`'))
        .map(str::to_string)
}

fn concept_slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "root".to_string()
    } else {
        slug
    }
}

fn publish_failure(state: &DaemonState, session_id: &str, error: impl Into<String>) {
    let Some(session) = state.get_session(session_id) else {
        return;
    };
    let seq = session.conduit.next_seq();
    session.conduit.publish(ProtocolEvent::RunFailed {
        session_id: session_id.to_string(),
        seq,
        created_at: now_iso(),
        error: error.into(),
        kind: "blueprint_init".to_string(),
        hint: "The workspace opened normally; blueprint initialization runs in the background."
            .to_string(),
    });
}
