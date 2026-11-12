//! Host glue for the #136 panes (A193 observability, A194 diff review, A197
//! PR/CI band). The pane models live in `rightkit_terminal_ui`; this module owns the
//! side effects the TUI crate must not: git, `git apply`, token lookup and the
//! forge fetch. `tui_host.rs` holds one [`PaneState`] and forwards usage, tool
//! and key events to it.

use rightkit_github::forge_client::{execute_get_blocking, GitHubForgeClient};
use rightkit_terminal_ui::ci_band::{parse_check_runs, CiBand};
use rightkit_terminal_ui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rightkit_terminal_ui::diff_review_pane::{Decision, DiffReview};
use rightkit_terminal_ui::observability_pane::{ObservabilityPane, UsageSnapshot};
use rightkit_terminal_ui::ratatui::text::Line;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

const CI_REFRESH_AFTER: Duration = Duration::from_secs(60);

/// What the host should do with a handled key.
pub enum PaneAction {
    Show(Vec<Line<'static>>),
    Notice(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CiFetch {
    Done(CiBand),
    /// No GitHub credential: the band says so instead of failing.
    Unauthenticated,
    Failed(String),
}

#[derive(Default)]
pub struct PaneState {
    pub obs: ObservabilityPane,
    pub review: Option<DiffReview>,
    pub ci: CiBand,
    ci_note: Option<String>,
    tool_starts: HashMap<String, (String, Instant)>,
    input_total: u64,
    cached_total: u64,
    cost_usd: f64,
    ci_rx: Option<Receiver<CiFetch>>,
    ci_last: Option<Instant>,
    /// Alt+R was pressed; the next key answers `[y/N]`.
    revert_pending: bool,
}

impl PaneState {
    /// True while a revert confirmation waits for its answer key.
    pub fn confirming_revert(&self) -> bool {
        self.revert_pending
    }

    /// Fold one `DaemonEvent::Usage` into the observability pane.
    pub fn on_usage(
        &mut self,
        context_size: i64,
        context_permille: i64,
        cache_read: i64,
        prompt: i64,
        cost_micros: Option<i64>,
        cost_reported: bool,
    ) {
        self.input_total += prompt.max(0) as u64;
        self.cached_total += cache_read.max(0) as u64;
        if cost_reported {
            self.cost_usd = cost_micros.map_or(0.0, |m| m as f64 / 1_000_000.0);
        }
        let window = if context_permille > 0 && context_size > 0 {
            (context_size as u64).saturating_mul(1000) / context_permille as u64
        } else {
            0
        };
        self.obs.set_usage(UsageSnapshot {
            input_tokens: self.input_total,
            output_tokens: 0,
            cached_input_tokens: self.cached_total,
            cost_usd: self.cost_usd,
            context_used: context_size.max(0) as u64,
            context_window: window,
        });
    }

    pub fn tool_started(&mut self, call_id: &str, name: &str) {
        self.tool_starts
            .insert(call_id.to_string(), (name.to_string(), Instant::now()));
    }

    pub fn tool_finished(&mut self, call_id: &str) {
        if let Some((name, at)) = self.tool_starts.remove(call_id) {
            self.obs.record_tool(&name, at.elapsed().as_millis() as u64);
        }
    }

    /// Footer chip for the CI band, e.g. `PR #7 ok`; None when nothing to show.
    pub fn footer_chip(&self) -> Option<String> {
        use rightkit_terminal_ui::ci_band::CheckState;
        let pr = self.ci.pr_number?;
        let state = match self.ci.checks.as_ref().map(|c| c.state()) {
            Some(CheckState::Passing) => " ok",
            Some(CheckState::Failing) => " failing",
            Some(CheckState::Pending) => " running",
            None => "",
        };
        Some(format!("PR #{pr}{state}"))
    }

    /// Collect a finished background fetch and start a new one when stale.
    pub fn poll_ci(&mut self, cwd: &Path, force: bool) {
        if let Some(rx) = &self.ci_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.ci_rx = None;
                    match result {
                        CiFetch::Done(band) => {
                            self.ci = band;
                            self.ci_note = None;
                        }
                        CiFetch::Unauthenticated => {
                            self.ci_note = Some(
                                format!("PR/CI status needs a GitHub sign-in (run `{app} github sign-in`).", app = crate::host_identity::app())
                                    .to_string(),
                            );
                        }
                        CiFetch::Failed(why) => {
                            self.ci_note = Some(format!("PR/CI status unavailable: {why}"));
                        }
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => self.ci_rx = None,
            }
        }
        let stale = self.ci_last.is_none_or(|t| t.elapsed() >= CI_REFRESH_AFTER);
        if self.ci_rx.is_none() && (force || stale) {
            self.ci_last = Some(Instant::now());
            let (tx, rx) = channel();
            let cwd = cwd.to_path_buf();
            std::thread::spawn(move || {
                let _ = tx.send(fetch_branch_ci(&cwd));
            });
            self.ci_rx = Some(rx);
        }
    }

    /// Handle an Alt-chord. None when the key is not a pane key.
    pub fn handle_key(&mut self, key: &KeyEvent, cwd: &Path) -> Option<PaneAction> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if self.revert_pending {
            self.revert_pending = false;
            if key.code == KeyCode::Char('y') && key.modifiers == KeyModifiers::NONE {
                let Some(root) = rightkit_agent_config::state_root() else {
                    return Some(PaneAction::Notice(
                        "Revert cancelled: no state root for the backup patch.".into(),
                    ));
                };
                return Some(self.confirm_revert(cwd, &root));
            }
            return Some(PaneAction::Notice("Revert cancelled.".into()));
        }
        if key.modifiers != KeyModifiers::ALT {
            return None;
        }
        match key.code {
            // Alt+O: observability pane.
            KeyCode::Char('o') => Some(PaneAction::Show(self.obs.lines())),
            // Alt+B: PR/CI band (refreshes in the background).
            KeyCode::Char('b') => {
                self.poll_ci(cwd, true);
                Some(match (self.ci.line(), &self.ci_note) {
                    (Some(line), _) => PaneAction::Show(vec![line]),
                    (None, Some(note)) => PaneAction::Notice(note.clone()),
                    (None, None) => PaneAction::Notice(
                        "Fetching PR/CI status; press Alt+B again in a moment.".to_string(),
                    ),
                })
            }
            // Alt+D: (re)load the workspace diff and show the current hunk.
            KeyCode::Char('d') => match git_diff(cwd) {
                Ok(diff) => {
                    let review = DiffReview::from_diff(&diff);
                    if review.is_empty() {
                        self.review = None;
                        return Some(PaneAction::Notice(
                            "No uncommitted changes to review.".into(),
                        ));
                    }
                    let lines = hunk_lines(&review);
                    self.review = Some(review);
                    Some(PaneAction::Show(lines))
                }
                Err(e) => Some(PaneAction::Notice(format!("Diff review unavailable: {e}"))),
            },
            // Alt+] / Alt+[: next / previous hunk.
            KeyCode::Char(']') | KeyCode::Char('[') => {
                let r = self.review.as_mut()?;
                if key.code == KeyCode::Char(']') {
                    r.next_hunk();
                } else {
                    r.prev_hunk();
                }
                Some(PaneAction::Show(hunk_lines(r)))
            }
            // Alt+A: accept the current hunk.
            KeyCode::Char('a') => {
                let r = self.review.as_mut()?;
                r.accept();
                Some(PaneAction::Show(hunk_lines(r)))
            }
            // Alt+R: revert the current hunk through `git apply`.
            KeyCode::Char('r') => {
                let r = self.review.as_ref()?;
                let (file, hunk) = r.current()?;
                if hunk.decision == Decision::Reverted {
                    return Some(PaneAction::Notice("Hunk already reverted.".into()));
                }
                self.revert_pending = true;
                Some(PaneAction::Notice(format!(
                    "Revert this hunk in {}? It is not recoverable. [y/N]",
                    file.path
                )))
            }
            _ => None,
        }
    }
}

impl PaneState {
    /// Save the forward patch under `<root>/reverted-hunks/`, then revert the
    /// current hunk. Undo with `git apply <backup>`.
    fn confirm_revert(&mut self, cwd: &Path, root: &Path) -> PaneAction {
        let Some(r) = self.review.as_mut() else {
            return PaneAction::Notice("No diff loaded.".into());
        };
        let Some((file, hunk)) = r.current() else {
            return PaneAction::Notice("No hunk.".into());
        };
        let mut forward = format!("--- a/{0}\n+++ b/{0}\n{1}\n", file.path, hunk.header);
        for l in &hunk.lines {
            forward.push_str(l);
            forward.push('\n');
        }
        let slug: String = file
            .path
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let dir = root.join("reverted-hunks");
        let backup = dir.join(format!("{stamp}-{slug}.patch"));
        if let Err(e) =
            std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&backup, &forward))
        {
            return PaneAction::Notice(format!("Revert cancelled: backup failed: {e}"));
        }
        let Some(patch) = r.revert() else {
            return PaneAction::Notice("No hunk.".into());
        };
        match apply_patch(cwd, &patch) {
            Ok(()) => PaneAction::Notice(format!(
                "Hunk reverted. Undo with: git apply \"{}\"",
                backup.display()
            )),
            Err(e) => PaneAction::Notice(format!("Revert failed: {e}")),
        }
    }
}

/// The current hunk with its file path and decision tag.
pub fn hunk_lines(review: &DiffReview) -> Vec<Line<'static>> {
    let Some((file, hunk)) = review.current() else {
        return vec![Line::from("No hunk.")];
    };
    let tag = match hunk.decision {
        Decision::Undecided => "",
        Decision::Accepted => " [accepted]",
        Decision::Reverted => " [reverted]",
    };
    let mut out = vec![Line::from(format!("{}  {}{tag}", file.path, hunk.header))];
    out.extend(hunk.lines.iter().map(|l| Line::from(format!("  {l}"))));
    out.push(Line::from(
        "Alt+] next  Alt+[ prev  Alt+A accept  Alt+R revert",
    ));
    out
}

/// Runs `git -C <cwd> <args>` through the shared RightKit runner (hidden console, null stdin or the
/// given input) with no deadline and no output cap, as the previous direct `Command` calls had.
/// `cwd` is passed as a `Path`, so a non-UTF-8 workspace path works. Returns the exit success flag,
/// full stdout, and stderr lines joined with a newline.
fn run_git(
    cwd: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<(bool, Vec<u8>, String), String> {
    let mut owned = rightkit_child_process::OwnedCommand::new("git");
    owned.command_mut().arg("-C").arg(cwd).args(args);
    let mut options = rightkit_child_process::StreamOptions::new()
        .stdout_tail_cap(0)
        .stderr_tail_cap(0);
    if let Some(bytes) = input {
        options = options.input(bytes);
    }
    let mut stdout = Vec::new();
    let mut stderr: Vec<String> = Vec::new();
    let out = rightkit_child_process::run_streaming(
        owned,
        &options,
        &|| false,
        &mut |chunk: &[u8]| stdout.extend_from_slice(chunk),
        &mut |line: &str| stderr.push(line.to_string()),
    )
    .map_err(|e| e.to_string())?;
    Ok((
        out.status.success(),
        stdout,
        stderr.join(
            "
",
        ),
    ))
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let (ok, stdout, stderr) = run_git(cwd, args, None)?;
    if !ok {
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Uncommitted changes of the workspace (tracked files).
fn git_diff(cwd: &Path) -> Result<String, String> {
    git(cwd, &["diff", "HEAD", "--no-color", "--no-ext-diff"])
}

/// Apply a unified patch to the working tree via `git apply` on stdin.
fn apply_patch(cwd: &Path, patch: &str) -> Result<(), String> {
    let (ok, _stdout, stderr) = run_git(
        cwd,
        &["apply", "--recount", "--whitespace=nowarn", "-"],
        Some(patch.as_bytes()),
    )?;
    if ok {
        Ok(())
    } else {
        Err(stderr.trim().to_string())
    }
}

/// `(owner, repo)` from a GitHub remote URL (https or ssh form).
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
        .then(|| (owner.to_string(), repo.to_string()))
}

/// First open PR `(number, title)` from a GitHub `pulls` response body.
pub fn parse_first_pr(body: &str) -> Option<(u64, String)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let pr = v.as_array()?.first()?;
    Some((
        pr.get("number")?.as_u64()?,
        pr.get("title")?.as_str()?.to_string(),
    ))
}

fn fetch_branch_ci(cwd: &Path) -> CiFetch {
    let token = match crate::github_signin::resolve_access_token() {
        Ok(Some(t)) if !t.trim().is_empty() => t,
        Ok(_) => return CiFetch::Unauthenticated,
        Err(_) => return CiFetch::Unauthenticated,
    };
    let remote = match git(cwd, &["remote", "get-url", "origin"]) {
        Ok(r) => r,
        Err(e) => return CiFetch::Failed(e),
    };
    let Some((owner, repo)) = parse_github_remote(&remote) else {
        return CiFetch::Failed("origin is not a GitHub remote".into());
    };
    let branch = match git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(b) => b.trim().to_string(),
        Err(e) => return CiFetch::Failed(e),
    };
    let sha = match git(cwd, &["rev-parse", "HEAD"]) {
        Ok(s) => s.trim().to_string(),
        Err(e) => return CiFetch::Failed(e),
    };
    let client = GitHubForgeClient::new(token);
    let mut band = CiBand {
        branch: branch.clone(),
        ..Default::default()
    };
    let pr_body = client
        .open_pr_for_branch_request(&owner, &repo, &branch)
        .map_err(|e| e.to_string())
        .and_then(|spec| execute_get_blocking(&spec));
    match pr_body {
        Ok(body) => {
            if let Some((n, title)) = parse_first_pr(&body) {
                band.pr_number = Some(n);
                band.pr_title = Some(title);
            }
        }
        Err(e) if e.contains("401") || e.contains("403") => return CiFetch::Unauthenticated,
        Err(e) => return CiFetch::Failed(e),
    }
    if let Ok(body) = client
        .check_runs_request(&owner, &repo, &sha)
        .map_err(|e| e.to_string())
        .and_then(|spec| execute_get_blocking(&spec))
    {
        band.checks = parse_check_runs(&body);
    }
    CiFetch::Done(band)
}
