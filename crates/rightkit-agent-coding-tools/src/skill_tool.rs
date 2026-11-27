//! `invoke_skill` — look up a named skill by slug and return its content.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Largest skill source (a skill file or a reference file) `invoke_skill` reads, in bytes.
const MAX_SKILL_SOURCE_BYTES: usize = 256 * 1024;

/// The `invoke_skill` tool.
pub struct SkillTool {
    description: std::sync::OnceLock<String>,
}

impl SkillTool {
    /// A fresh tool; its description is composed on first use from the host's names and options.
    pub fn new() -> Self {
        Self {
            description: std::sync::OnceLock::new(),
        }
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "invoke_skill"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        // Composed on first ask and kept for this tool instance only, so it follows the names and
        // options the host has set by the time the tool is built, not a process-wide first read.
        self.description.get_or_init(|| {
            let state_dir = rightkit_agent_config::state_dir_name();
            // The mobile build has no env override, so its description names none.
            let location = if cfg!(feature = "host") {
                format!(
                    "${}/<slug>.md or ~/{state_dir}/skills/<slug>.md",
                    rightkit_agent_config::env_name("SKILLS_DIR")
                )
            } else {
                format!("~/{state_dir}/skills/<slug>.md")
            };
            let file = if crate::tool_options::skill_files_and_search() {
                " Pass `file` to read a reference file of the skill."
            } else {
                ""
            };
            format!(
                "Invoke a named skill by slug. Reads the skill definition from {location} and returns its content for the agent to act on. Returns 'Skill not found: {{slug}}' if missing.{file}"
            )
        })
    }

    fn input_schema(&self) -> Value {
        if !crate::tool_options::skill_files_and_search() {
            return json!({
                "type": "object",
                "required": ["skill"],
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "Slug of the skill to invoke (e.g. 'deep-research')."
                    },
                    "args": {
                        "description": "Optional arguments to pass to the skill."
                    }
                }
            });
        }
        json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "Slug of the skill to invoke (e.g. 'deep-research')."
                },
                "query": {
                    "type": "string",
                    "description": "Instead of `skill`: words to search the slugs and descriptions of every available skill, for one that the catalog in the prompt does not list."
                },
                "args": {
                    "description": "Optional arguments to pass to the skill."
                },
                "file": {
                    "type": "string",
                    "description": "Optional path of a text file inside the skill's own folder (for example 'references/manual.md') or in the shared folder next to the skills ('../_shared/anti-slop.md'). With `skill`, returns that file instead of SKILL.md."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // `query` and `file` exist only with `set_skill_files_and_search`.
        let extended = crate::tool_options::skill_files_and_search();
        if extended && args["skill"].as_str().is_none() {
            if let Some(query) = args["query"]
                .as_str()
                .map(str::trim)
                .filter(|q| !q.is_empty())
            {
                return ToolResult::ok(search_skills(&skill_dirs(ctx), query));
            }
        }
        let slug = match args["skill"].as_str() {
            Some(s) => s,
            None if extended => {
                return ToolResult::error("invoke_skill: 'skill' (or 'query') is required")
            }
            None => return ToolResult::error("invoke_skill: 'skill' is required"),
        };
        let Some(slug) = normalize_skill_slug(slug) else {
            return ToolResult::error("invoke_skill: invalid skill slug");
        };

        let dirs = skill_dirs(ctx);

        if extended {
            if let Some(file) = args["file"]
                .as_str()
                .map(str::trim)
                .filter(|f| !f.is_empty())
            {
                return serve_skill_file(&dirs, &slug, file);
            }
        }

        for path in candidate_paths(&dirs, &slug) {
            let limit = MAX_SKILL_SOURCE_BYTES;
            let mut content = String::new();
            let read = open_regular_file(&path).and_then(|file| {
                std::io::Read::read_to_string(
                    &mut std::io::Read::take(file, limit as u64 + 1),
                    &mut content,
                )
            });
            if read.is_ok() {
                if content.len() > limit {
                    return ToolResult::error(format!(
                        "invoke_skill: skill '{slug}' exceeds the {limit}-byte skill size limit"
                    ));
                }
                // Manifest parsing belongs to the host. Plain markdown without frontmatter
                // stays a valid skill, but a declared frontmatter cannot be shown to enable
                // the skill without a catalog to read it, or when it does not parse, so
                // both fail closed.
                let has_frontmatter = {
                    let head = content.trim_start_matches(['\n', '\r', ' ']);
                    head.starts_with("---") || head.starts_with("+++")
                };
                match rightkit_agent_host_traits::skill_catalog() {
                    Some(catalog) => match catalog.parse_manifest(&content) {
                        Some(manifest) if !manifest.active => {
                            record_skill_asset_observation(&slug, &content, false);
                            return ToolResult::error(format!(
                                "invoke_skill: skill '{slug}' is not active (lifecycle: {})",
                                manifest.lifecycle
                            ));
                        }
                        None if has_frontmatter => {
                            return ToolResult::error(format!(
                                "invoke_skill: skill '{slug}' has invalid frontmatter"
                            ));
                        }
                        _ => {}
                    },
                    None if has_frontmatter => {
                        return ToolResult::error(format!(
                            "invoke_skill: skill '{slug}' declares frontmatter and no skill catalog is installed to check it, so it is not served"
                        ));
                    }
                    None => {}
                }
                record_skill_asset_observation(&slug, &content, true);
                let skill_args = &args["args"];
                return if skill_args.is_null() {
                    ToolResult::ok(format!(
                        "Skill: {slug}\nPath: {}\n\n{content}",
                        path.display()
                    ))
                } else {
                    ToolResult::ok(format!(
                        "Skill: {slug}\nPath: {}\n\n{content}\n\n---\nArgs: {skill_args}",
                        path.display()
                    ))
                };
            }
        }
        if extended {
            return ToolResult::ok(format!(
                "Skill not found: {slug}. Call invoke_skill with `query` to search the available skills."
            ));
        }
        ToolResult::ok(format!("Skill not found: {slug}"))
    }
}

/// Opens `path` only when it is a regular file, so a FIFO or device never blocks the read.
/// The file is checked before it is opened and again on the open handle, so a path swapped
/// in between is caught too.
fn open_regular_file(path: &Path) -> std::io::Result<std::fs::File> {
    let not_regular = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "skill source is not a regular file",
        )
    };
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

/// A skill source read only when it is a regular file (symlinks are followed; discovery
/// roots legitimately symlink) no larger than [`MAX_SKILL_SOURCE_BYTES`]. FIFOs, devices
/// and oversized files are treated as unreadable and never loaded whole.
fn read_bounded_skill_source(path: &Path) -> Option<String> {
    use std::io::Read;
    let limit = MAX_SKILL_SOURCE_BYTES;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut raw = String::new();
    file.take(limit as u64 + 1).read_to_string(&mut raw).ok()?;
    (raw.len() <= limit).then_some(raw)
}

/// Serve a reference file that lives inside a skill's own folder, or in the
/// `_shared` folder next to the skills. Confinement is by canonical path, so a
/// symlink or a crafted path cannot reach any other file.
fn serve_skill_file(dirs: &[PathBuf], slug: &str, file: &str) -> ToolResult {
    const INVALID: &str = "invoke_skill: invalid skill file path";
    let path = Path::new(file);
    if path.is_absolute() || file.contains('\\') || file.contains('\0') {
        return ToolResult::error(INVALID);
    }
    let extension_allowed = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| {
            matches!(extension.as_str(), "md" | "json" | "yaml" | "yml" | "txt")
        });
    if !extension_allowed {
        return ToolResult::error(INVALID);
    }
    // The only allowed `..` is exactly one leading component followed by
    // `_shared`.
    let components: Vec<std::path::Component<'_>> = path.components().collect();
    let rest_start = match components.as_slice() {
        [std::path::Component::ParentDir, std::path::Component::Normal(next), ..]
            if next.to_str() == Some("_shared") =>
        {
            1
        }
        _ => 0,
    };
    if components[rest_start..].iter().any(|component| {
        !matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return ToolResult::error(INVALID);
    }

    let limit = MAX_SKILL_SOURCE_BYTES;
    for dir in dirs {
        let skill_dir = dir.join(slug);
        // Only directory skills have reference files.
        if !skill_dir.join("SKILL.md").is_file() {
            continue;
        }
        let Ok(candidate) = skill_dir.join(file).canonicalize() else {
            continue;
        };
        let Ok(canonical_skill_dir) = skill_dir.canonicalize() else {
            continue;
        };
        let inside_skill = candidate.starts_with(&canonical_skill_dir);
        let inside_shared = dir
            .join("_shared")
            .canonicalize()
            .is_ok_and(|shared| candidate.starts_with(shared));
        if !inside_skill && !inside_shared {
            continue;
        }
        let mut content = String::new();
        let read = open_regular_file(&candidate).and_then(|opened| {
            std::io::Read::read_to_string(
                &mut std::io::Read::take(opened, limit as u64 + 1),
                &mut content,
            )
        });
        if read.is_err() {
            continue;
        }
        if content.len() > limit {
            return ToolResult::error(format!(
                "invoke_skill: file '{file}' exceeds the {limit}-byte skill size limit"
            ));
        }
        return ToolResult::ok(format!("Skill: {slug}\nFile: {file}\n\n{content}"));
    }
    ToolResult::ok(format!("Skill file not found: {slug}/{file}"))
}

/// Active skills whose slug or description holds every word of `query` (the prompt's catalog
/// lists only the first few dozen, so a skill past that cap is found here).
fn search_skills(dirs: &[PathBuf], query: &str) -> String {
    const MAX_HITS: usize = 25;
    // Manifest parsing belongs to the host: with no catalog installed no skill is known.
    let Some(catalog) = rightkit_agent_host_traits::skill_catalog() else {
        return format!("No available skill matches '{query}'.");
    };
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(String, String)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let (slug, file) = if path.is_dir() {
                let file = path.join("SKILL.md");
                match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) if file.is_file() => (name.to_string(), file),
                    _ => continue,
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                match path.file_stem().and_then(|n| n.to_str()) {
                    Some(stem) => (stem.to_string(), path.clone()),
                    None => continue,
                }
            } else {
                continue;
            };
            if normalize_skill_slug(&slug).as_deref() != Some(slug.as_str())
                || hits.iter().any(|(seen, _)| *seen == slug)
            {
                continue;
            }
            let Some(raw) = read_bounded_skill_source(&file) else {
                continue;
            };
            let Some(manifest) = catalog.parse_manifest(&raw) else {
                continue;
            };
            if !manifest.active {
                continue;
            }
            let haystack = format!("{slug} {}", manifest.description).to_lowercase();
            if words.iter().all(|word| haystack.contains(word)) {
                hits.push((slug, manifest.description));
            }
        }
    }
    if hits.is_empty() {
        return format!("No available skill matches '{query}'.");
    }
    hits.sort();
    let total = hits.len();
    let mut out = format!("{total} skill(s) match '{query}' (call invoke_skill with the slug):");
    for (slug, description) in hits.into_iter().take(MAX_HITS) {
        let one_line: String = description.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!(
            "\n- {slug}: {}",
            one_line.chars().take(200).collect::<String>()
        ));
    }
    out
}

fn record_skill_asset_observation(slug: &str, content: &str, applied: bool) {
    rightkit_agent_telemetry::record_procedural_asset_observation(
        rightkit_agent_telemetry::ProceduralAssetObservationV1::from_bytes(
            slug,
            rightkit_agent_telemetry::ProceduralAssetKind::Skill,
            content.as_bytes(),
            false,
            Vec::new(),
            rightkit_agent_telemetry::CountObservationV1::unavailable(
                rightkit_agent_telemetry::UnavailableReason::NotInstrumented,
            ),
            rightkit_agent_telemetry::CountObservationV1::observed(1),
            rightkit_agent_telemetry::CountObservationV1::observed(if applied { 1 } else { 0 }),
            rightkit_agent_telemetry::CountObservationV1::unavailable(
                rightkit_agent_telemetry::UnavailableReason::NotInstrumented,
            ),
            None,
            rightkit_agent_telemetry::ProvenanceV1::from_evidence(
                "skill_tool",
                vec![format!("skill:{slug}")],
            ),
        ),
    );
}

fn normalize_skill_slug(slug: &str) -> Option<String> {
    let slug = slug.trim();
    if slug.is_empty()
        || slug.contains("..")
        || slug.contains('/')
        || slug.contains('\\')
        || !slug
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    Some(slug.to_string())
}

fn skill_dirs(ctx: &ToolContext) -> Vec<PathBuf> {
    if !ctx.skill_dirs.is_empty() {
        return ctx.skill_dirs.clone();
    }
    default_skill_dirs()
}

fn default_skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(feature = "host")]
    {
        if let Ok(dir) = std::env::var(rightkit_agent_config::env_name("SKILLS_DIR")) {
            let trimmed = dir.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
    }
    if let Some(root) = rightkit_agent_config::state_root() {
        dirs.push(root.join("skills"));
    }
    dirs
}

fn candidate_paths(dirs: &[PathBuf], slug: &str) -> Vec<PathBuf> {
    dirs.iter()
        .flat_map(|dir| skill_candidate_paths(dir, slug))
        .collect()
}

fn skill_candidate_paths(dir: &Path, slug: &str) -> Vec<PathBuf> {
    vec![
        dir.join(format!("{slug}.md")),
        dir.join(slug).join("SKILL.md"),
    ]
}
