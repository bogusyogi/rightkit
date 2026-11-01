//! Destructive-command preview (A200): what a shell command would change,
//! shown on the approval card above the proceed/cancel options.
//!
//! Classification is lexical and conservative: it names only effects the
//! command text states. It never executes anything and never claims a
//! command is safe; absence of a preview means "not recognised", not "safe".

/// A recognised destructive command and the effects it would have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructivePreview {
    pub headline: String,
    pub effects: Vec<String>,
}

fn segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | ';' => out.push(std::mem::take(&mut cur)),
            '&' | '|' => {
                if chars.peek() == Some(&c) {
                    chars.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn unquote(t: &str) -> &str {
    t.trim_matches(|c| c == '"' || c == '\'')
}

fn preview_segment(seg: &str) -> Option<(String, String)> {
    let toks: Vec<&str> = seg.split_whitespace().collect();
    let first = unquote(toks.first()?).to_ascii_lowercase();
    let first = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&first)
        .to_string();
    let args: Vec<&str> = toks[1..].iter().map(|t| unquote(t)).collect();
    let flags: Vec<String> = args
        .iter()
        .filter(|a| a.starts_with('-'))
        .map(|a| a.to_ascii_lowercase())
        .collect();
    let targets: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let list = |t: &[&str]| {
        if t.is_empty() {
            "its arguments".to_string()
        } else {
            t.join(", ")
        }
    };
    let has_short = |ch: char| flags.iter().any(|f| !f.starts_with("--") && f.contains(ch));
    let has_long = |name: &str| flags.iter().any(|f| f == name);
    match first.as_str() {
        "rm" | "unlink" | "shred" => {
            let recursive = has_short('r') || has_long("--recursive");
            let force = has_short('f') || has_long("--force");
            let what = if recursive {
                "recursively deletes"
            } else {
                "deletes"
            };
            let tail = if force {
                " (no confirmation, no recovery)"
            } else {
                ""
            };
            Some((
                format!("Deletes files: {}", list(&targets)),
                format!("{what} {}{tail}", list(&targets)),
            ))
        }
        "rmdir" | "rd" => Some((
            "Deletes directories".into(),
            format!("removes {}", list(&targets)),
        )),
        "del" | "erase" => Some((
            "Deletes files".into(),
            format!("deletes {}", list(&targets)),
        )),
        "remove-item" | "ri" => Some((
            "Deletes files".into(),
            format!("removes {}", list(&targets)),
        )),
        "mkfs" | "format" | "diskpart" => Some((
            "Reformats a disk".into(),
            "destroys all data on the target volume".into(),
        )),
        "dd" => {
            let of = args.iter().find_map(|a| a.strip_prefix("of="))?;
            Some((
                "Overwrites a file or device".into(),
                format!("overwrites {of}"),
            ))
        }
        "truncate" => Some((
            "Truncates files".into(),
            format!("empties or resizes {}", list(&targets)),
        )),
        "git" => {
            let sub = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .copied()
                .unwrap_or("");
            match sub {
                "reset" if has_long("--hard") => Some((
                    "Discards uncommitted changes".into(),
                    "git reset --hard drops working-tree and index changes".into(),
                )),
                "clean" if has_short('f') || has_long("--force") => Some((
                    "Deletes untracked files".into(),
                    "git clean removes untracked files (not recoverable from git)".into(),
                )),
                "checkout" | "restore" if args.contains(&".") || args.contains(&"--") => Some((
                    "Discards file changes".into(),
                    "reverts working-tree files to the last commit".into(),
                )),
                "push" if has_short('f') || flags.iter().any(|f| f.starts_with("--force")) => {
                    Some((
                        "Force-pushes".into(),
                        "rewrites remote history; collaborators may lose commits".into(),
                    ))
                }
                "branch" if args.contains(&"-D") => Some((
                    "Deletes a branch".into(),
                    "force-deletes the branch even if unmerged".into(),
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Classify a shell command line. Returns `None` when no destructive effect is
/// recognised.
pub fn destructive_preview(command: &str) -> Option<DestructivePreview> {
    let mut headline = None;
    let mut effects = Vec::new();
    for seg in segments(command) {
        if let Some((h, e)) = preview_segment(&seg) {
            headline.get_or_insert(h);
            effects.push(e);
        }
        // Redirect-truncate: `> file` that is not `>>`, `2>` or `&>`.
        if let Some(i) = seg.find('>') {
            let before = &seg[..i];
            let after = &seg[i + 1..];
            let plain = !before.ends_with('>')
                && !before.ends_with('2')
                && !before.ends_with('&')
                && !after.starts_with('>')
                && !after.starts_with('&');
            if plain {
                let target = after.split_whitespace().next().unwrap_or("");
                if !target.is_empty()
                    && target != "/dev/null"
                    && !target.eq_ignore_ascii_case("nul")
                {
                    headline.get_or_insert_with(|| "Overwrites a file".to_string());
                    effects.push(format!("redirect overwrites {target}"));
                }
            }
        }
    }
    headline.map(|headline| DestructivePreview { headline, effects })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm_rf_is_flagged_with_targets() {
        let p = destructive_preview("rm -rf build dist").unwrap();
        assert!(p.headline.contains("build, dist"));
        assert!(p.effects[0].contains("recursively deletes"));
        assert!(p.effects[0].contains("no recovery"));
    }

    #[test]
    fn plain_commands_are_not_flagged() {
        for c in [
            "ls -la",
            "cargo build",
            "git status",
            "git push origin main",
            "echo hi >> log.txt",
            "cat a 2>/dev/null",
        ] {
            assert!(destructive_preview(c).is_none(), "{c}");
        }
    }

    #[test]
    fn git_destructive_forms() {
        for c in [
            "git reset --hard HEAD~1",
            "git clean -fd",
            "git push --force",
            "git push -f origin x",
            "git branch -D old",
            "git checkout -- .",
            "git restore .",
        ] {
            assert!(destructive_preview(c).is_some(), "{c}");
        }
        assert!(destructive_preview("git reset --soft HEAD~1").is_none());
    }

    #[test]
    fn chained_segment_is_found_and_paths_stripped() {
        let p = destructive_preview("cd x && /bin/rm -r old; echo done").unwrap();
        assert_eq!(p.effects.len(), 1);
        assert!(p.effects[0].contains("old"));
    }

    #[test]
    fn redirect_truncate_and_dd() {
        assert!(
            destructive_preview("echo x > notes.txt").unwrap().effects[0].contains("notes.txt")
        );
        assert!(
            destructive_preview("dd if=a of=/dev/sda").unwrap().effects[0].contains("/dev/sda")
        );
        assert!(destructive_preview("dd if=a bs=1").is_none());
    }

    #[test]
    fn windows_forms() {
        assert!(destructive_preview("Remove-Item -Recurse C:\\tmp\\x").is_some());
        assert!(destructive_preview("del /s foo.txt").is_some());
    }
}
