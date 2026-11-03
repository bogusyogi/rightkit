//! Machine-readable sources for the source ledger (rightkit#1495).
//!
//! `bootstrap` never starts the open-items ledger from the source model's memory. It reads, at
//! handoff time: `git status` and unpushed commits for each repository, the files under the
//! approvals directory (`tasks/approvals/`), and, when asked, the open issues of the owning
//! repositories through `gh`. Each finding becomes one [`OpenItem`] with a stable id. The source
//! may add rows, replace a row by declaring the same id, or strike a row with a stated reason
//! (`SourceBlock::merge_prefilled_open_items`); it cannot silently drop one.
//!
//! A source that was asked for and cannot be read is an error, not an empty list.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::live_state::{Approval, OpenItem, OpenItemState};

/// What `bootstrap` reads.
#[derive(Debug, Default, Clone)]
pub struct PrefillSources {
    /// Repository work trees: uncommitted paths and commits on no remote.
    pub repos: Vec<PathBuf>,
    /// Directory of issued approvals (one file each), when it exists.
    pub approvals_dir: Option<PathBuf>,
    /// `owner/name` repositories whose open issues are listed with `gh`.
    pub issue_repos: Vec<String>,
}

const MAX_ISSUES_PER_REPO: usize = 100;
const MAX_TEXT: usize = 200;

/// Collect every ledger row the sources hold.
pub fn collect(sources: &PrefillSources) -> Result<Vec<OpenItem>, String> {
    let mut rows = Vec::new();
    for repo in &sources.repos {
        rows.extend(git_rows(repo)?);
    }
    if let Some(dir) = &sources.approvals_dir {
        rows.extend(approval_rows(dir)?);
    }
    for repo in &sources.issue_repos {
        rows.extend(issue_rows(repo)?);
    }
    Ok(rows)
}

fn slug(value: &str) -> String {
    let mapped: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches('-').chars().take(40).collect()
}

fn one_line(value: &str) -> String {
    let flat = value
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(MAX_TEXT).collect()
}

/// Text copied from a file or an issue title must not carry a secret into the block.
fn safe_text(value: &str) -> String {
    let text = one_line(value);
    for (_, pattern) in crate::l1b_port::validate::secret_patterns() {
        if pattern.is_match(&text) {
            return "(text withheld: it looks like a secret; read the source)".to_string();
        }
    }
    if text.is_empty() {
        "(empty)".to_string()
    } else {
        text
    }
}

fn run(program: &str, args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn repo_name(repo: &Path) -> String {
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    slug(&name)
}

fn git_rows(repo: &Path) -> Result<Vec<OpenItem>, String> {
    let shown = repo.display().to_string();
    let name = repo_name(repo);
    let status = run("git", &["status", "--porcelain=v1"], Some(repo))
        .map_err(|e| format!("repository {shown}: {e}"))?;
    let dirty = status.lines().filter(|l| !l.trim().is_empty()).count();
    let unpushed = run(
        "git",
        &["log", "--branches", "--not", "--remotes", "--oneline"],
        Some(repo),
    )
    .map_err(|e| format!("repository {shown}: {e}"))?;
    let unpushed = unpushed.lines().filter(|l| !l.trim().is_empty()).count();
    let mut rows = Vec::new();
    if dirty > 0 {
        rows.push(OpenItem {
            id: format!("git.{name}.uncommitted"),
            item: safe_text(&format!("{dirty} uncommitted path(s) in {shown}")),
            owner: "source".to_string(),
            state: OpenItemState::InProgress,
            next: "review each path, then commit it or hand it to its owner".to_string(),
            blocker: "none".to_string(),
            approval: Approval {
                needed: false,
                from: None,
            },
            evidence: format!("git -C {shown} status --porcelain=v1"),
        });
    }
    if unpushed > 0 {
        rows.push(OpenItem {
            id: format!("git.{name}.unpushed"),
            item: safe_text(&format!(
                "{unpushed} local commit(s) in {shown} are on no remote"
            )),
            owner: "source".to_string(),
            state: OpenItemState::Pending,
            next: "report them as unpushed; push only after the owner approves".to_string(),
            blocker: "push waits for the owner's approval".to_string(),
            approval: Approval {
                needed: true,
                from: Some("user".to_string()),
            },
            evidence: format!("git -C {shown} log --branches --not --remotes --oneline"),
        });
    }
    Ok(rows)
}

fn approval_rows(dir: &Path) -> Result<Vec<OpenItem>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A workspace without an approvals directory has no issued approvals.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read approvals {}: {error}", dir.display())),
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read approvals {}: {e}", dir.display()))?;
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if path.is_file() && !hidden {
            files.push(path);
        }
    }
    files.sort();
    let mut rows = Vec::new();
    for path in files {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let first = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("");
        rows.push(OpenItem {
            id: format!("approval.{}", slug(&stem)),
            item: safe_text(&format!("issued approval {stem}: {first}")),
            owner: "source".to_string(),
            state: OpenItemState::Pending,
            next: "finish the approved work or close the approval file".to_string(),
            blocker: "none".to_string(),
            approval: Approval {
                needed: false,
                from: None,
            },
            evidence: path.display().to_string(),
        });
    }
    Ok(rows)
}

fn issue_rows(repo: &str) -> Result<Vec<OpenItem>, String> {
    let valid = repo.split_once('/').is_some_and(|(owner, name)| {
        !owner.is_empty()
            && !name.is_empty()
            && !name.contains('/')
            && repo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
    });
    if !valid {
        return Err(format!("--issues-repo must be owner/name: {repo}"));
    }
    let limit = MAX_ISSUES_PER_REPO.to_string();
    let json = run(
        "gh",
        &[
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title",
        ],
        None,
    )?;
    let parsed: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("gh issue list for {repo}: {e}"))?;
    let list = parsed
        .as_array()
        .ok_or_else(|| format!("gh issue list for {repo} did not return a list"))?;
    let mut rows = Vec::new();
    for issue in list {
        let Some(number) = issue["number"].as_u64() else {
            continue;
        };
        let title = issue["title"].as_str().unwrap_or("");
        rows.push(OpenItem {
            id: format!("issue.{}.{number}", slug(repo)),
            item: safe_text(&format!("{repo}#{number} {title}")),
            owner: "unassigned".to_string(),
            state: OpenItemState::Pending,
            next: "work it or close it with evidence".to_string(),
            blocker: "none".to_string(),
            approval: Approval {
                needed: false,
                from: None,
            },
            evidence: format!("gh issue view {number} --repo {repo} --comments"),
        });
    }
    if list.len() >= MAX_ISSUES_PER_REPO {
        rows.push(OpenItem {
            id: format!("issue.{}.more", slug(repo)),
            item: format!(
                "{repo} has at least {MAX_ISSUES_PER_REPO} open issues; only the first are listed"
            ),
            owner: "source".to_string(),
            state: OpenItemState::Pending,
            next: "read the rest of the list with gh".to_string(),
            blocker: "none".to_string(),
            approval: Approval {
                needed: false,
                from: None,
            },
            evidence: format!("gh issue list --repo {repo} --state open --limit 1000"),
        });
    }
    Ok(rows)
}
