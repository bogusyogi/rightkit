//! Host glue for the #136 panes (A193 observability, A194 diff review, A197
//! PR/CI band). The pane models live in `rightkit_terminal_ui`; this module owns the
//! side effects the TUI crate must not: git, `git apply`, token lookup and the
//! forge fetch. `tui_host.rs` holds one [`PaneState`] and forwards usage, tool
//! and key events to it.

use crate::tui_projects::{key_matches, TuiSettings, TUI_ACTIONS};
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

/// The pane actions a chord can run. Each one is a `[tui.keymap]` action, so
/// the chord is looked up through [`TuiSettings::key_for`] (A021-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneKey {
    Observability,
    CiBand,
    DiffOpen,
    HunkNext,
    HunkPrev,
    HunkAccept,
    HunkRevert,
}

impl PaneKey {
    /// Every pane action with its keymap name.
    pub const ALL: [(PaneKey, &'static str); 7] = [
        (PaneKey::Observability, "observability"),
        (PaneKey::CiBand, "ci_band"),
        (PaneKey::DiffOpen, "diff_open"),
        (PaneKey::HunkNext, "hunk_next"),
        (PaneKey::HunkPrev, "hunk_prev"),
        (PaneKey::HunkAccept, "hunk_accept"),
        (PaneKey::HunkRevert, "hunk_revert"),
    ];

    /// The pane action whose configured chord is `key`.
    pub fn lookup(settings: &TuiSettings, key: &KeyEvent) -> Option<PaneKey> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        Self::ALL
            .iter()
            .find(|(_, name)| key_matches(&settings.key_for(name), key))
            .map(|(pane, _)| *pane)
            .or_else(|| Self::legacy_lookup(settings, key))
    }

    /// The hunk action an older default chord still runs (issue #535). Only
    /// while that action keeps its shipped chord and no action at all is bound
    /// to the old one, so a user binding always wins and nothing is shadowed.
    fn legacy_lookup(settings: &TuiSettings, key: &KeyEvent) -> Option<PaneKey> {
        let (pane, name, _) = LEGACY_HUNK_CHORDS
            .iter()
            .find(|(_, _, chord)| key_matches(chord, key))?;
        let taken = settings.keymap.contains_key(*name)
            || TUI_ACTIONS
                .iter()
                .any(|action| key_matches(&settings.key_for(action.name), key));
        (!taken).then_some(*pane)
    }
}

/// The hunk chords shipped before issue #535, kept as extra bindings. A
/// terminal with the extended key protocol sends them unambiguously
/// (`CSI 93;3u`, `CSI 91;3u`); without it Alt+[ is `ESC [` and never arrives
/// as a key, which is why they are no longer the defaults.
const LEGACY_HUNK_CHORDS: [(PaneKey, &str, &str); 2] = [
    (PaneKey::HunkNext, "hunk_next", "alt+]"),
    (PaneKey::HunkPrev, "hunk_prev", "alt+["),
];

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
    output_total: u64,
    cached_total: u64,
    cost_usd: f64,
    ci_rx: Option<Receiver<CiFetch>>,
    ci_last: Option<Instant>,
    /// Alt+R was pressed; the next key answers `[y/N]`.
    revert_pending: bool,
    /// The workspace diff being read on a thread; `poll_diff` collects it.
    diff_rx: Option<Receiver<Result<String, String>>>,
}

/// The cost half of a `DaemonEvent::Usage`: the micro-dollar figure and whether the event reports
/// a cumulative cost at all.
#[derive(Debug, Clone, Copy)]
pub struct UsageCost {
    pub micros: Option<i64>,
    pub reported: bool,
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
        completion: i64,
        cost: UsageCost,
    ) {
        let UsageCost {
            micros: cost_micros,
            reported: cost_reported,
        } = cost;
        self.input_total += prompt.max(0) as u64;
        self.cached_total += cache_read.max(0) as u64;
        // Only the cumulative event carries session-wide output; the per-response
        // one would double count against it.
        if cost_reported {
            self.output_total = completion.max(0) as u64;
        }
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
            output_tokens: self.output_total,
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
                            self.ci_note = Some(format!(
                                "PR/CI status needs a GitHub sign-in (run `{}`).",
                                crate::host_identity::command("github sign-in")
                            ));
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

    /// Comment on line `n` (counting from 1) of the current hunk.
    pub fn add_comment(&mut self, n: usize, text: &str) -> PaneAction {
        let Some(review) = self.review.as_mut() else {
            return PaneAction::Notice("Run /changes first to load the changes.".into());
        };
        if n > 0 && review.comment(n - 1, text) {
            PaneAction::Notice(format!(
                "Comment added on line {n}. /changes send sends them."
            ))
        } else {
            PaneAction::Notice("No such line in this hunk, or the comment is empty.".into())
        }
    }

    /// The review comments as one prompt for the agent, cleared once taken.
    pub fn take_feedback_prompt(&mut self) -> Option<String> {
        let feedback = self.review.as_mut()?.take_feedback()?;
        Some(format!(
            "Review feedback on my uncommitted changes:\n{feedback}"
        ))
    }

    /// True while a diff read is running.
    pub fn diff_pending(&self) -> bool {
        self.diff_rx.is_some()
    }

    /// The finished diff read as the pane's answer; None while it still runs.
    pub fn poll_diff(&mut self) -> Option<PaneAction> {
        let result = match self.diff_rx.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the git read stopped".to_string()),
        };
        self.diff_rx = None;
        Some(match result {
            Ok(diff) => {
                let review = DiffReview::from_diff(&diff);
                if review.is_empty() {
                    self.review = None;
                    PaneAction::Notice("No uncommitted changes to review.".into())
                } else {
                    let lines = hunk_lines(&review);
                    self.review = Some(review);
                    PaneAction::Show(lines)
                }
            }
            Err(e) => PaneAction::Notice(format!("Diff review unavailable: {e}")),
        })
    }

    /// Handle a pane chord (already resolved to `pane` through the keymap) or
    /// the `[y/N]` answer of a pending revert. None when the key is not for us.
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        cwd: &Path,
        pane: Option<PaneKey>,
    ) -> Option<PaneAction> {
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
        self.run(pane?, cwd)
    }

    /// Run one pane action. None when it needs a loaded diff and none is.
    pub fn run(&mut self, pane: PaneKey, cwd: &Path) -> Option<PaneAction> {
        match pane {
            PaneKey::Observability => Some(PaneAction::Show(self.obs.lines())),
            // PR/CI band (refreshes in the background).
            PaneKey::CiBand => {
                self.poll_ci(cwd, true);
                Some(match (self.ci.line(), &self.ci_note) {
                    (Some(line), _) => PaneAction::Show(vec![line]),
                    (None, Some(note)) => PaneAction::Notice(note.clone()),
                    (None, None) => PaneAction::Notice(
                        "Fetching PR/CI status; run /ci or press Alt+B again in a moment."
                            .to_string(),
                    ),
                })
            }
            // (Re)load the workspace diff and show the current hunk. `git diff` runs on a
            // thread so a large repository never stalls typing; `poll_diff` shows the result.
            PaneKey::DiffOpen => {
                if self.diff_rx.is_some() {
                    return Some(PaneAction::Notice("The diff is still loading.".into()));
                }
                let (tx, rx) = channel();
                let cwd = cwd.to_path_buf();
                std::thread::spawn(move || {
                    let _ = tx.send(git_diff(&cwd));
                });
                self.diff_rx = Some(rx);
                Some(PaneAction::Notice("Loading the diff...".into()))
            }
            PaneKey::HunkNext | PaneKey::HunkPrev => {
                let r = self.review.as_mut()?;
                if pane == PaneKey::HunkNext {
                    r.next_hunk();
                } else {
                    r.prev_hunk();
                }
                Some(PaneAction::Show(hunk_lines(r)))
            }
            PaneKey::HunkAccept => {
                let r = self.review.as_mut()?;
                r.accept();
                Some(PaneAction::Show(hunk_lines(r)))
            }
            // Revert the current hunk through `git apply`, after a `[y/N]`.
            PaneKey::HunkRevert => {
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
    out.extend(
        hunk.lines
            .iter()
            .enumerate()
            .map(|(i, l)| Line::from(format!("{:>3} {l}", i + 1))),
    );
    out.push(Line::from(
        "Alt+J next  Alt+K prev  Alt+A accept  Alt+R revert  (or /changes next|prev|accept|revert|comment <line> <text>|send)",
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
