//! `list_files` — list files in the workspace (Claude-Code `Glob`/`LS` parity).
//!
//! Read-only file *discovery*, distinct from `search` (content grep). Walks the
//! workspace (or a subdirectory), respects `.gitignore`, optionally filters by a
//! filename glob (`*.rs`), and returns paths relative to the workspace root. An
//! agent uses this to see the project structure before reading or editing.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use serde_json::{json, Value};

pub struct ListFilesTool;

const DEFAULT_MAX: usize = 500;
/// Hard ceiling on `max` so a caller cannot request an unbounded listing.
const HARD_MAX: usize = 5000;

/// Build a glob matcher. A pattern containing `/` or `**` is matched against the
/// workspace-relative path (`src/**/*.rs`); a bare pattern (`*.rs`) is matched
/// against the file name only. Returns the matcher and whether it's path-scoped.
pub(crate) fn build_matcher(pattern: &str) -> Result<(GlobMatcher, bool), String> {
    let path_scoped = pattern.contains('/') || pattern.contains("**");
    let glob = Glob::new(pattern).map_err(|e| format!("Invalid glob '{pattern}': {e}"))?;
    Ok((glob.compile_matcher(), path_scoped))
}

/// The canonical workspace file listing — one gitignore-aware,
/// noise-filtered walk shared by `list_files` and every host file picker
/// (e.g. the TUI's `@` completion), so they all read the identical source
/// instead of growing a second scanner. Read-only.
///
/// `root` is the directory walked; it must already resolve inside
/// `workspace_root` (the tool path resolves user input through
/// `read_file::resolve_in_workspace` first; host pickers pass the
/// workspace root itself). Returned paths are relative to
/// `workspace_root`, sorted, and capped at `max`. `glob` applies the same
/// matcher the tool uses (bare pattern → filename, `**`/`/` pattern →
/// workspace-relative path). Returns `(paths, truncated)` — `truncated`
/// means the walk stopped at `max`, not that the tree was exhausted.
pub fn discover_workspace_files(
    workspace_root: &std::path::Path,
    root: &std::path::Path,
    glob: Option<&str>,
    max: usize,
) -> Result<(Vec<String>, bool), String> {
    discover_workspace_files_bounded(workspace_root, root, glob, max, None)
}

/// Same discovery source with an optional work deadline for interactive callers.
pub fn discover_workspace_files_bounded(
    workspace_root: &std::path::Path,
    root: &std::path::Path,
    glob: Option<&str>,
    max: usize,
    budget: Option<std::time::Duration>,
) -> Result<(Vec<String>, bool), String> {
    discover_workspace_files_filtered(workspace_root, root, glob, max, budget, &|_| false)
}

/// Discovery with a caller-supplied `skip` predicate evaluated on every file
/// before it is counted or returned (used to enforce read-deny globs, so a
/// denied path never appears in a listing nor consumes the `max` budget).
pub fn discover_workspace_files_filtered(
    workspace_root: &std::path::Path,
    root: &std::path::Path,
    glob: Option<&str>,
    max: usize,
    budget: Option<std::time::Duration>,
    skip: &dyn Fn(&std::path::Path) -> bool,
) -> Result<(Vec<String>, bool), String> {
    let canonical_workspace = workspace_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    if !crate::coding_tools::read_file::path_is_within(&canonical_root, &canonical_workspace) {
        return Err("discovery root escapes selected workspace".into());
    }
    if max == 0 {
        return Ok((Vec::new(), false));
    }
    let deadline = budget.map(|duration| std::time::Instant::now() + duration);
    let matcher = match glob {
        Some(g) => Some(build_matcher(g)?),
        None => None,
    };
    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .require_git(false)
        .filter_entry(crate::coding_tools::search::not_noise_dir)
        .build()
        .flatten()
    {
        if deadline.is_some_and(|limit| std::time::Instant::now() >= limit) {
            truncated = true;
            break;
        }
        let path = entry.path();
        if !path.is_file() || skip(path) {
            continue;
        }
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if let Some((m, path_scoped)) = &matcher {
            let hay = if *path_scoped {
                rel.as_str()
            } else {
                path.file_name().and_then(|n| n.to_str()).unwrap_or("")
            };
            if !m.is_match(hay) {
                continue;
            }
        }
        out.push(rel);
        if out.len() >= max {
            truncated = true;
            break;
        }
    }
    out.sort();
    Ok((out, truncated))
}

#[async_trait::async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "List workspace files (gitignore-aware, default 500, hard max 5000). Bare glob '*.rs' matches filename; path glob 'src/**/*.rs' matches full path. Fail: path escapes workspace root."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Subdirectory to list (relative to workspace; default the whole workspace)" },
                "glob": { "type": "string", "description": "Filename glob filter, e.g. '*.rs'" },
                "max": { "type": "integer", "description": "Max paths to return (default 500)" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let root = match args["path"].as_str() {
            Some(p) => {
                match crate::coding_tools::read_file::resolve_in_workspace(p, &ctx.workspace_root) {
                    Ok(r) => r,
                    Err(e) => return ToolResult::error(e),
                }
            }
            None => ctx.workspace_root.clone(),
        };
        if !root.is_dir() {
            return ToolResult::error(format!(
                "not_a_directory: '{}' is not a directory",
                args["path"].as_str().unwrap_or(".")
            ));
        }
        let max = match args.get("max") {
            None | Some(Value::Null) => DEFAULT_MAX,
            Some(v) => match v.as_u64() {
                Some(0) | None => {
                    return ToolResult::error(
                        "invalid_max: 'max' must be a positive integer".to_string(),
                    )
                }
                Some(n) => usize::try_from(n).unwrap_or(HARD_MAX).min(HARD_MAX),
            },
        };
        if let Some(g) = args["glob"].as_str() {
            if let Err(e) = build_matcher(g) {
                return ToolResult::error(format!("invalid_glob: {e}"));
            }
        }

        if let Err(error) = crate::coding_tools::read_file::enforce_path_deny(
            ctx,
            &root,
            crate::coding_tools::read_file::PathAccess::Read,
        ) {
            return ToolResult::error(error);
        }
        let (out, truncated) = match discover_workspace_files_filtered(
            &ctx.workspace_root,
            &root,
            args["glob"].as_str(),
            max,
            None,
            &|candidate| {
                crate::coding_tools::read_file::enforce_path_deny(
                    ctx,
                    candidate,
                    crate::coding_tools::read_file::PathAccess::Read,
                )
                .is_err()
            },
        ) {
            Ok(result) => result,
            Err(e) => return ToolResult::error(e),
        };

        if out.is_empty() {
            return ToolResult::ok("No files found.".to_string());
        }
        let mut body = crate::coding_tools::output_compaction::mask_secrets(&out.join("\n"));
        if truncated {
            // 7b: states the cap explicitly. The true original count isn't
            // tracked (the walk stops as soon as `max` is hit, so counting
            // the rest would mean walking the whole tree anyway) — the cap
            // itself is the actionable number here: raise `max` or narrow
            // `glob`.
            body.push_str(&format!("\n[truncated at {max} files]"));
        }
        ToolResult::ok(body)
    }
}
