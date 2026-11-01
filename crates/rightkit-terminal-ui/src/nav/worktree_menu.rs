//! The `/worktree` menu: list, create, switch, remove.
//!
//! Pure model, same discipline as the rest of `nav`: it parses the daemon's
//! `git worktree list --porcelain` result, renders the menu text, and resolves
//! a user-typed target (menu number, path, or branch) into a
//! [`WorktreeRequest`]. It performs nothing; the host runs the request through
//! the existing delivery worktree API (list/add/remove) or the repository
//! switch (switch).

use crate::render::sanitize;

/// One worktree row parsed from `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub bare: bool,
    pub detached: bool,
}

/// What the menu asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeRequest {
    List,
    Create {
        branch: String,
        base: Option<String>,
        path: Option<String>,
    },
    Switch {
        path: String,
    },
    Remove {
        path: String,
    },
}

/// Extract the porcelain text from a daemon list result (a command record
/// carrying `stdoutTail`/`stdout_tail`), or accept raw porcelain text.
pub fn porcelain_from_result(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => ["stdoutTail", "stdout_tail", "stdout"]
            .iter()
            .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string(),
        Err(_) => raw.to_string(),
    }
}

pub fn parse_porcelain(text: &str) -> Vec<WorktreeEntry> {
    let mut out: Vec<WorktreeEntry> = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for line in text.lines().map(str::trim_end) {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(done) = cur.take() {
                out.push(done);
            }
            cur = Some(WorktreeEntry {
                path: path.to_string(),
                branch: None,
                head: None,
                bare: false,
                detached: false,
            });
        } else if let Some(entry) = cur.as_mut() {
            if let Some(head) = line.strip_prefix("HEAD ") {
                entry.head = Some(head.chars().take(8).collect());
            } else if let Some(branch) = line.strip_prefix("branch ") {
                entry.branch = Some(
                    branch
                        .strip_prefix("refs/heads/")
                        .unwrap_or(branch)
                        .to_string(),
                );
            } else if line == "bare" {
                entry.bare = true;
            } else if line == "detached" {
                entry.detached = true;
            }
        }
    }
    if let Some(done) = cur.take() {
        out.push(done);
    }
    out
}

fn norm(path: &str) -> String {
    path.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Render the numbered menu. The row whose path equals `active` is marked.
pub fn render_menu(entries: &[WorktreeEntry], active: Option<&str>) -> String {
    let mut lines = vec!["Worktrees".to_string()];
    if entries.is_empty() {
        lines.push("  (none)".into());
    }
    for (i, e) in entries.iter().enumerate() {
        let label = if e.bare {
            "bare".to_string()
        } else if let Some(b) = &e.branch {
            b.clone()
        } else {
            format!("detached {}", e.head.clone().unwrap_or_default())
        };
        let mark = if active.is_some_and(|a| norm(a) == norm(&e.path)) {
            " (active)"
        } else {
            ""
        };
        lines.push(sanitize(&format!("  {}. {label}  {}{mark}", i + 1, e.path)));
    }
    lines.push(
        "Actions: /worktree list | add <branch> [base] [path] | switch <n|path|branch> | remove <n|path|branch>"
            .into(),
    );
    lines.join("\n")
}

fn resolve(entries: &[WorktreeEntry], target: &str) -> Result<String, String> {
    if let Ok(n) = target.parse::<usize>() {
        return entries
            .get(n.wrapping_sub(1))
            .map(|e| e.path.clone())
            .ok_or_else(|| format!("no worktree #{n}; run /worktree list"));
    }
    entries
        .iter()
        .find(|e| norm(&e.path) == norm(target) || e.branch.as_deref() == Some(target))
        .map(|e| e.path.clone())
        .ok_or_else(|| format!("unknown worktree `{target}`; run /worktree list"))
}

/// Resolve `/worktree <action> [args]` against the current entries.
pub fn parse_request(
    action: &str,
    args: &[&str],
    entries: &[WorktreeEntry],
) -> Result<WorktreeRequest, String> {
    match action {
        "" | "list" | "menu" => Ok(WorktreeRequest::List),
        "add" | "create" | "new" => {
            let branch = args
                .first()
                .ok_or("usage: /worktree add <branch> [base] [path]")?;
            Ok(WorktreeRequest::Create {
                branch: (*branch).to_string(),
                base: args.get(1).map(|s| (*s).to_string()),
                path: args.get(2).map(|s| (*s).to_string()),
            })
        }
        "switch" | "cd" | "open" => {
            let t = args
                .first()
                .ok_or("usage: /worktree switch <n|path|branch>")?;
            Ok(WorktreeRequest::Switch {
                path: resolve(entries, t)?,
            })
        }
        "remove" | "rm" => {
            let t = args
                .first()
                .ok_or("usage: /worktree remove <n|path|branch>")?;
            Ok(WorktreeRequest::Remove {
                path: resolve(entries, t)?,
            })
        }
        other => Err(format!(
            "unknown worktree action `{other}`; use list|add|switch|remove"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "worktree C:/repo\nHEAD 1234567890abcdef\nbranch refs/heads/main\n\nworktree C:/wt/feat\nHEAD abcdef1234567890\nbranch refs/heads/feat\n\nworktree C:/wt/det\nHEAD 99999999aaaa\ndetached\n";

    #[test]
    fn parses_porcelain_entries() {
        let e = parse_porcelain(SAMPLE);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].branch.as_deref(), Some("main"));
        assert_eq!(e[1].path, "C:/wt/feat");
        assert!(e[2].detached && e[2].branch.is_none());
        assert_eq!(e[0].head.as_deref(), Some("12345678"));
    }

    #[test]
    fn extracts_porcelain_from_daemon_record() {
        let raw = serde_json::json!({"stdoutTail": SAMPLE}).to_string();
        assert_eq!(parse_porcelain(&porcelain_from_result(&raw)).len(), 3);
        assert_eq!(parse_porcelain(&porcelain_from_result(SAMPLE)).len(), 3);
    }

    #[test]
    fn menu_marks_active_and_lists_actions() {
        let e = parse_porcelain(SAMPLE);
        let text = render_menu(&e, Some("c:\\wt\\feat\\"));
        assert!(text.contains("2. feat  C:/wt/feat (active)"));
        assert!(!text.contains("C:/repo (active)"));
        assert!(text.contains("switch"));
    }

    #[test]
    fn list_and_create_requests() {
        let e = parse_porcelain(SAMPLE);
        assert_eq!(parse_request("", &[], &e), Ok(WorktreeRequest::List));
        assert_eq!(
            parse_request("add", &["topic", "main"], &e),
            Ok(WorktreeRequest::Create {
                branch: "topic".into(),
                base: Some("main".into()),
                path: None
            })
        );
        assert!(parse_request("add", &[], &e).is_err());
    }

    #[test]
    fn switch_and_remove_resolve_number_branch_and_path() {
        let e = parse_porcelain(SAMPLE);
        assert_eq!(
            parse_request("switch", &["2"], &e),
            Ok(WorktreeRequest::Switch {
                path: "C:/wt/feat".into()
            })
        );
        assert_eq!(
            parse_request("remove", &["feat"], &e),
            Ok(WorktreeRequest::Remove {
                path: "C:/wt/feat".into()
            })
        );
        assert_eq!(
            parse_request("switch", &["c:\\wt\\det"], &e),
            Ok(WorktreeRequest::Switch {
                path: "C:/wt/det".into()
            })
        );
        assert!(parse_request("switch", &["9"], &e).is_err());
        assert!(parse_request("remove", &["nope"], &e).is_err());
        assert!(parse_request("bogus", &[], &e).is_err());
    }
}
