//! Approval presentation: the pending-request overlay and the decision
//! transcript cells that record its outcome.
//!
//! Donor lineage: `codex-rs/tui/src/bottom_pane/approval_overlay.rs` and
//! `codex-rs/tui/src/history_cell/approvals.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca`, adapted to the host app's
//! authority model:
//!
//! - **Intent, never execution.** The overlay emits [`ApprovalIntent`]s
//!   carrying the *existing* approval/tool-call `request_id` and the
//!   session id the host bound it with. It performs no registry, policy, or
//!   daemon call of its own — the host maps `Resolve` onto its existing
//!   `resolve_approval` authority.
//! - **Stale/wrong-session safety.** The overlay is constructed bound to
//!   one session id; `enqueue` rejects foreign sessions, `resolve_external`
//!   dismisses requests the authority settled elsewhere, and a decision can
//!   only ever name the currently displayed request's own ids.
//! - **Buffered-input safety.** `handle` swallows every key until the
//!   overlay has rendered once (`armed`). Keys typed before the modal was
//!   on screen cannot approve — and a request promoted from the queue
//!   re-disarms until its own first render, so input queued *ahead* of it
//!   cannot decide it either. Letter keys (`y a n t`) are common prose, so
//!   they are also ignored for `LETTER_DELAY` after the first render, and
//!   never answer a card that appeared while the composer held unsent text,
//!   nor one whose host never reported the composer's state
//!   ([`ApprovalRequest::with_composer_had_text`]): digits, arrows plus Enter,
//!   and Esc still do.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::component::RenderFrameExt as _;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

use crate::component::{Component, Event, Outcome, Role, Theme};

use super::cell::{Cell, PrefixedWrappedCell};
use super::diff::{FileChange, create_diff_preview, ledger_diff_rows};
use super::sanitize::sanitize;
use crate::transcript::wrap::{display_width, wrap};

/// What the user may decide for a request — the host supplies the allowed
/// set per request, so policies that never offer "for session" simply omit
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Approve this request once.
    Approved,
    /// Approve this request and equivalent requests for the rest of the
    /// session.
    ApprovedForSession,
    /// Approve this request and persist the exact prefix proposed by the
    /// approval authority.
    ApprovedForPrefix,
    /// Decline the request.
    Denied,
    /// Decline, stop the turn, and hand the composer to the user to say
    /// what to do instead. Esc selects the same action.
    Redirect,
}

impl ApprovalDecision {
    pub fn label(self) -> &'static str {
        match self {
            ApprovalDecision::Approved => "Allow once",
            ApprovalDecision::ApprovedForSession => "Allow for this session",
            ApprovalDecision::ApprovedForPrefix => "Always allow this prefix",
            ApprovalDecision::Denied => "Deny",
            ApprovalDecision::Redirect => "No, and tell the model what to do instead",
        }
    }
}

/// What is being approved. All strings are sanitized at construction.
#[derive(Debug, Clone)]
pub enum ApprovalKind {
    /// Run a command.
    Exec {
        command: String,
        /// Where the command runs, when the request or session says.
        cwd: Option<String>,
        /// The model's own stated reason, when the call carried one.
        reason: Option<String>,
        /// Paths the command is declared to write, when the request says.
        writes: Option<String>,
        /// Network access the request declares (`on`/`off`), when it does.
        network: Option<String>,
    },
    /// Apply a patch; carries the changes so the modal can show a bounded
    /// diff preview of exactly what would land.
    Patch {
        changes: BTreeMap<PathBuf, FileChange>,
        cwd: PathBuf,
        reason: Option<String>,
    },
    /// Any other authority-gated action (network grants, MCP elicitation,
    /// …) the host describes in plain text.
    Generic { description: String },
}

/// One pending approval request, keyed by the authority's own ids.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The session this request belongs to. Must equal the overlay's bound
    /// session or it is rejected.
    pub session_id: String,
    /// The existing approval/tool-call authority id. `Resolve` intents echo
    /// this verbatim; the overlay never invents or rewrites it.
    pub request_id: String,
    pub kind: ApprovalKind,
    /// Ordered decision choices the host offers for this request.
    pub options: Vec<ApprovalDecision>,
    /// Why the policy is asking, in the policy's own terms ("shell commands
    /// need approval in Ask mode"). `None` when the policy did not say; the
    /// card then omits the line rather than inventing a reason.
    pub policy_reason: Option<String>,
    /// Exact prefix proposed by the approval authority. UI never derives or
    /// widens it; host submits it through the existing policy amendment.
    execpolicy_amendment: Option<Vec<String>>,
    /// Exact persistent allow rule the "always allow" option installs. The
    /// card prints these patterns verbatim; the host installs exactly them.
    allow_rule: Option<super::allow_rule::AllowRule>,
    /// What the host said about the composer as this card appeared:
    /// `Some(true)` when it held unsent text (letter keys then never answer
    /// the card: the user may still be typing), `Some(false)` when it did not,
    /// `None` when the host never said. Only `Some(false)` lets letter keys
    /// answer, so a host that has not been updated keeps digits, arrows plus
    /// Enter, and Esc as the only ways to decide.
    composer_had_text: Option<bool>,
}

impl ApprovalRequest {
    pub fn new(
        session_id: impl Into<String>,
        request_id: impl Into<String>,
        kind: ApprovalKind,
        options: Vec<ApprovalDecision>,
    ) -> Self {
        Self {
            session_id: sanitize(&session_id.into()),
            request_id: sanitize(&request_id.into()),
            kind: sanitize_kind(kind),
            options,
            policy_reason: None,
            execpolicy_amendment: None,
            allow_rule: None,
            composer_had_text: None,
        }
    }

    /// Record whether the composer held unsent text when this card appeared.
    /// `true` means letter keys cannot answer it (the user may still be
    /// typing); `false` lets them answer after the first-render delay. Not
    /// calling this leaves the card on digits, arrows plus Enter, and Esc.
    pub fn with_composer_had_text(mut self, had_text: bool) -> Self {
        self.composer_had_text = Some(had_text);
        self
    }

    /// Letter keys may answer this card only when the host reported an empty
    /// composer.
    fn letters_enabled(&self) -> bool {
        self.composer_had_text == Some(false)
    }

    /// Attach the persistent allow rule the "always allow" option installs.
    pub fn with_allow_rule(mut self, rule: Option<super::allow_rule::AllowRule>) -> Self {
        self.allow_rule = rule;
        self
    }

    /// The exact rule the "always allow" option installs, when offered.
    pub fn allow_rule(&self) -> Option<&super::allow_rule::AllowRule> {
        self.allow_rule.as_ref()
    }

    /// Attach the policy's reason for asking.
    pub fn with_policy_reason(mut self, reason: Option<String>) -> Self {
        self.policy_reason = reason
            .map(|r| sanitize(&r))
            .filter(|r| !r.trim().is_empty());
        self
    }

    /// Attach authority-proposed command prefix for the persistent allow
    /// option.
    pub fn with_execpolicy_amendment(mut self, command: Option<Vec<String>>) -> Self {
        self.execpolicy_amendment = command
            .map(|parts| parts.into_iter().map(|part| sanitize(&part)).collect())
            .filter(|parts: &Vec<String>| !parts.is_empty());
        self
    }

    pub fn execpolicy_amendment(&self) -> Option<&[String]> {
        self.execpolicy_amendment.as_deref()
    }
}

/// The session-allowance scope lines for an exec request. The grant the
/// authority records is a session rule on this *exact* command string (see
/// `resolve_tool_approval`'s `ApprovedForSession` arm), so the card names
/// precisely that and nothing wider: no prefix, no arguments, no chained
/// form. The grant carries no directory binding, so none is claimed.
pub fn session_allowance_scope(command: &str) -> Vec<String> {
    let command = command.lines().next().unwrap_or("").trim();
    vec![
        format!("Allow for this session: {command}"),
        "only this exact command · other commands and chained forms (&&  ;  |) ask again"
            .to_string(),
    ]
}

fn sanitize_kind(kind: ApprovalKind) -> ApprovalKind {
    match kind {
        ApprovalKind::Exec {
            command,
            cwd,
            reason,
            writes,
            network,
        } => ApprovalKind::Exec {
            command: sanitize(&command),
            cwd: cwd.map(|c| sanitize(&c)),
            reason: reason.map(|r| sanitize(&r)),
            writes: writes.map(|w| sanitize(&w)),
            network: network.map(|n| sanitize(&n)),
        },
        // `changes`/`cwd` are already sanitized by `FileChange` and drawn
        // through `display_path`'s sanitize; only the reason needs it here.
        ApprovalKind::Patch {
            changes,
            cwd,
            reason,
        } => ApprovalKind::Patch {
            changes,
            cwd,
            reason: reason.map(|r| sanitize(&r)),
        },
        ApprovalKind::Generic { description } => ApprovalKind::Generic {
            description: sanitize(&description),
        },
    }
}

/// What the overlay asks the host to do. The host owns the authority these
/// ids name; the overlay only reports the user's choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalIntent {
    /// The user picked a decision for the named request. Map verbatim onto
    /// the existing approval-resolution authority call.
    Resolve {
        session_id: String,
        request_id: String,
        decision: ApprovalDecision,
    },
    /// The user pressed Esc: reject the request AND stop the running turn
    /// (Codex `cancel_current_request`, Claude Code "No, and tell X what to
    /// do differently"). The host resolves it as a denial through the
    /// existing approval authority, cancels the turn and refocuses the
    /// composer.
    Rejected {
        session_id: String,
        request_id: String,
    },
}

/// The modal approval overlay.
///
/// Rendering model: the host gives it a `Rect` (typically centered above
/// the bottom pane or on the alt screen); it draws a bordered block with
/// the request summary, a bounded preview, the option list, and footer
/// hints. It holds the request queue and emits intents; nothing else.
pub struct ApprovalOverlay {
    /// The session this overlay answers for. Enqueues from any other
    /// session are rejected outright.
    session_id: String,
    current: Option<ApprovalRequest>,
    queue: VecDeque<ApprovalRequest>,
    selected: usize,
    intents: Vec<ApprovalIntent>,
    /// Model name shown in the title (`<model> wants to`).
    model_label: Option<String>,
    /// False until `render` has run once for the current request. Gate for
    /// "input buffered before the overlay appeared must not approve".
    armed: bool,
    /// False when the card is too short to show any of what is being
    /// approved; approving keys are then refused until it can.
    subject_visible: bool,
    /// Letter keys are ignored until this instant (set at the first render).
    /// `None` means the delay has passed or never applied (tests).
    letters_ok_at: Option<Instant>,
    /// Permission mode shown dim beside "esc denies" (`Ask mode`), if known.
    mode_label: Option<String>,
}

/// How long letter keys are ignored after the card is first drawn.
const LETTER_DELAY: Duration = Duration::from_millis(400);

impl ApprovalOverlay {
    /// Bind the overlay to `session_id` — a request carrying any other
    /// session id can never be answered here.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: sanitize(&session_id.into()),
            current: None,
            queue: VecDeque::new(),
            selected: 0,
            intents: Vec::new(),
            model_label: None,
            armed: false,
            subject_visible: true,
            letters_ok_at: None,
            mode_label: None,
        }
    }

    /// Add a pending request. Returns `false` when the request's session id
    /// does not match this overlay, or its request id is already tracked —
    /// a stale or foreign request must never reach the decision surface.
    pub fn enqueue(&mut self, mut request: ApprovalRequest) -> bool {
        if request.session_id != self.session_id {
            return false;
        }
        if request.execpolicy_amendment.is_none() && request.allow_rule.is_none() {
            request
                .options
                .retain(|decision| *decision != ApprovalDecision::ApprovedForPrefix);
        }
        let known = self
            .current
            .as_ref()
            .is_some_and(|c| c.request_id == request.request_id)
            || self
                .queue
                .iter()
                .any(|r| r.request_id == request.request_id);
        if known {
            return false;
        }
        if self.current.is_none() {
            self.set_current(request);
        } else {
            self.queue.push_back(request);
        }
        true
    }

    /// The authority settled this request without the overlay (timeout,
    /// policy, another surface). Remove it from view; if it was current,
    /// promote the next queued request. Returns `true` if a tracked
    /// request was dismissed — a false return means the ids matched
    /// nothing and nothing changed.
    pub fn resolve_external(&mut self, session_id: &str, request_id: &str) -> bool {
        if session_id != self.session_id {
            return false;
        }
        if let Some(current) = &self.current
            && current.request_id == request_id
        {
            self.advance();
            return true;
        }
        let before = self.queue.len();
        self.queue.retain(|r| r.request_id != request_id);
        self.queue.len() != before
    }

    /// Ids of every request this overlay still holds (current + queued) — for the host's stale-decision cross-checks.
    pub fn pending_ids(&self) -> Vec<&str> {
        self.current
            .iter()
            .chain(self.queue.iter())
            .map(|r| r.request_id.as_str())
            .collect()
    }

    /// Name the permission mode shown beside "esc denies" ("Ask mode").
    pub fn set_mode_label(&mut self, mode: Option<String>) {
        self.mode_label = mode.map(|m| sanitize(&m)).filter(|m| !m.trim().is_empty());
    }

    /// Name the active model in the title (`<model> wants to`).
    pub fn set_model_label(&mut self, model: Option<String>) {
        self.model_label = model.map(|m| sanitize(&m)).filter(|m| !m.trim().is_empty());
    }

    /// The request currently displayed, if any.
    pub fn current_request(&self) -> Option<&ApprovalRequest> {
        self.current.as_ref()
    }

    /// Every request still open: the one on screen, then the queued ones.
    pub fn pending_requests(&self) -> impl Iterator<Item = &ApprovalRequest> {
        self.current.iter().chain(self.queue.iter())
    }

    /// True when nothing is being shown and nothing is queued.
    pub fn is_complete(&self) -> bool {
        self.current.is_none() && self.queue.is_empty()
    }

    /// Drain intents emitted since the last call.
    pub fn drain_intents(&mut self) -> Vec<ApprovalIntent> {
        std::mem::take(&mut self.intents)
    }

    fn set_current(&mut self, request: ApprovalRequest) {
        self.selected = 0;
        // A new request must be seen before it can be decided: any input
        // already in flight was typed against the *previous* request's
        // screen, not this one.
        self.armed = false;
        self.current = Some(request);
    }

    fn advance(&mut self) {
        if let Some(next) = self.queue.pop_front() {
            self.set_current(next);
        } else {
            self.current = None;
            self.selected = 0;
            self.armed = false;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(request) = &self.current else {
            return;
        };
        let len = request.options.len() as isize;
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    fn selected_declines(&self) -> bool {
        self.current
            .as_ref()
            .and_then(|r| r.options.get(self.selected))
            .is_some_and(|d| matches!(d, ApprovalDecision::Denied | ApprovalDecision::Redirect))
    }

    fn accept_selected(&mut self) {
        let Some(request) = self.current.clone() else {
            return;
        };
        let Some(decision) = request.options.get(self.selected).copied() else {
            return;
        };
        if decision == ApprovalDecision::Redirect {
            self.reject_current();
            return;
        }
        self.intents.push(ApprovalIntent::Resolve {
            session_id: request.session_id,
            request_id: request.request_id,
            decision,
        });
        self.advance();
    }

    /// Esc: reject the current request, and every request queued behind it
    /// (the host stops the turn that raised them).
    fn reject_current(&mut self) {
        let Some(request) = self.current.take() else {
            return;
        };
        for request in std::iter::once(request).chain(self.queue.drain(..)) {
            self.intents.push(ApprovalIntent::Rejected {
                session_id: request.session_id,
                request_id: request.request_id,
            });
        }
        self.selected = 0;
        self.armed = false;
    }

    /// The title's parts: the verb, the first line of the object it acts on,
    /// and whether the object runs over more than that one line.
    fn title_parts(request: &ApprovalRequest) -> (&'static str, String, bool) {
        let first = |text: &str| text.trim().lines().next().unwrap_or("").trim().to_string();
        let multiline = |text: &str| text.trim().lines().count() > 1;
        match &request.kind {
            ApprovalKind::Exec { command, .. } => ("shell", first(command), multiline(command)),
            ApprovalKind::Patch { changes, cwd, .. } => {
                match (changes.len(), changes.iter().next()) {
                    (1, Some((path, change))) => {
                        let verb = match change {
                            FileChange::Add { .. } => "create",
                            FileChange::Delete { .. } => "delete",
                            FileChange::Update { .. } => "edit",
                        };
                        (verb, shown_path(path, cwd), false)
                    }
                    (count, _) => ("edit", format!("{count} files"), false),
                }
            }
            ApprovalKind::Generic { description } => {
                ("approve", first(description), multiline(description))
            }
        }
    }

    /// Total (added, removed) lines of a file-change request.
    fn change_counts(request: &ApprovalRequest, theme: &Theme) -> Option<(usize, usize)> {
        let ApprovalKind::Patch { changes, .. } = &request.kind else {
            return None;
        };
        let mut totals = (0usize, 0usize);
        for change in changes.values() {
            let (added, removed) = match change {
                FileChange::Add { content } => (content.lines().count(), 0),
                FileChange::Delete { content } => (0, content.lines().count()),
                FileChange::Update { unified_diff } => {
                    let (_, added, removed) = ledger_diff_rows(unified_diff, 0, 0, theme);
                    (added, removed)
                }
            };
            totals.0 += added;
            totals.1 += removed;
        }
        Some(totals)
    }

    /// Fit the title row to `width`: the verb and object on the left, the
    /// change count and "<model> wants to" on the right when they fit.
    fn title_layout(&self, request: &ApprovalRequest, width: u16, theme: &Theme) -> TitleLayout {
        let inner = (width as usize).saturating_sub(RAIL_WIDTH).max(1);
        let (verb, full, multiline) = Self::title_parts(request);
        let counts = Self::change_counts(request, theme);
        let who = self
            .model_label
            .as_ref()
            .map(|model| format!("{} wants to", model.split(" · ").next().unwrap_or(model)));
        let count_w = counts.map_or(0, |(a, d)| display_width(&format!("+{a} −{d}")));
        let who_w = who.as_ref().map_or(0, |w| display_width(w));
        let right_w = count_w + who_w + if count_w > 0 && who_w > 0 { 3 } else { 0 };
        let verb_w = display_width(verb) + 1;
        let full_w = display_width(&full);
        let with_right = inner.saturating_sub(verb_w + right_w + 2);
        let show_right = right_w > 0 && with_right >= full_w.min(16);
        let budget = if show_right {
            with_right
        } else {
            inner.saturating_sub(verb_w)
        };
        TitleLayout {
            verb,
            object_clipped: multiline || full_w > budget,
            object: clip_to_width(&full, budget.max(1)),
            counts,
            who,
            show_right,
            width: inner,
        }
    }

    fn title_row(&self, layout: &TitleLayout, theme: &Theme) -> Line<'static> {
        let dim = Style::default().fg(theme.color(Role::Dim));
        let bright = Style::default().add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::styled(layout.verb.to_string(), bright),
            Span::raw(format!(" {}", layout.object)),
        ];
        if layout.show_right {
            let mut right: Vec<Span<'static>> = Vec::new();
            if let Some((added, removed)) = layout.counts {
                right.push(Span::styled(
                    format!("+{added}"),
                    Style::default().fg(theme.color(Role::Success)),
                ));
                right.push(Span::raw(" "));
                right.push(Span::styled(
                    format!("−{removed}"),
                    Style::default().fg(theme.color(Role::Failure)),
                ));
                if layout.who.is_some() {
                    right.push(Span::raw("   "));
                }
            }
            if let Some(who) = &layout.who {
                right.push(Span::styled(who.clone(), dim));
            }
            let used: usize = display_width(layout.verb)
                + 1
                + display_width(&layout.object)
                + right
                    .iter()
                    .map(|s| display_width(&s.content))
                    .sum::<usize>();
            spans.push(Span::raw(
                " ".repeat(layout.width.saturating_sub(used).max(1)),
            ));
            spans.extend(right);
        }
        Line::from(spans)
    }

    /// The card body: what is being approved (a diff, or a command the title
    /// could not show whole), where it runs, and, when no mode is shown at
    /// the right of the choices, why the policy is asking. Every optional line
    /// appears only when its source supplied it.
    fn body_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let Some(request) = &self.current else {
            return Vec::new();
        };
        let dim = Style::default().fg(theme.color(Role::Dim));
        let bright = Style::default().add_modifier(Modifier::BOLD);
        let inner = (width as usize).saturating_sub(RAIL_WIDTH + 2).max(1);
        let mut lines: Vec<Line<'static>> = Vec::new();

        match &request.kind {
            ApprovalKind::Exec {
                command,
                cwd,
                reason,
                writes,
                network,
            } => {
                // The title carries the command; repeat it here only when the
                // title had to cut it or it runs over several lines.
                if self.title_layout(request, width, theme).object_clipped {
                    for row in wrap_command(command, inner) {
                        lines.push(Line::from(vec![Span::raw("  "), Span::raw(row)]));
                    }
                }
                if let Some(preview) = super::destructive::destructive_preview(command) {
                    lines.push(Line::from(Span::styled(
                        format!("  ! {}", preview.headline),
                        bright,
                    )));
                    for effect in &preview.effects {
                        for row in wrap(effect, inner.saturating_sub(4).max(1) as u16) {
                            lines.push(Line::from(Span::styled(format!("    {row}"), dim)));
                        }
                    }
                }
                let mut effects: Vec<String> = Vec::new();
                if let Some(cwd) = cwd.as_ref().filter(|c| !c.trim().is_empty()) {
                    effects.push(format!("runs in {cwd}"));
                }
                if let Some(writes) = writes.as_ref().filter(|w| !w.trim().is_empty()) {
                    effects.push(format!("writes {writes}"));
                }
                if let Some(network) = network.as_ref().filter(|n| !n.trim().is_empty()) {
                    effects.push(format!("network {network}"));
                }
                if !effects.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", effects.join(" · ")),
                        dim,
                    )));
                }
                if let Some(reason) = reason.as_ref().filter(|r| !r.trim().is_empty()) {
                    lines.push(Line::from(Span::styled(
                        format!("  model's reason: {reason}"),
                        dim,
                    )));
                }
            }
            ApprovalKind::Patch {
                changes,
                cwd,
                reason,
            } => {
                if let Some(reason) = reason.as_ref().filter(|r| !r.trim().is_empty()) {
                    lines.push(Line::from(Span::styled(format!("  {reason}"), dim)));
                }
                // The user is being asked to approve this change, so show it
                // — a few changed lines with their context — not a teaser.
                // The card still clips itself to the screen it has.
                let mut rows = create_diff_preview(changes, cwd, inner, APPROVAL_DIFF_ROWS, theme);
                // The preview opens with a "Proposed Diff" heading and a
                // spacer, and (for one file) a "(+a -d) path" row: the title
                // row already says all of that.
                if rows.len() > 2 {
                    rows.drain(..2);
                    let opens_with_header = rows.first().is_some_and(|row| {
                        let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
                        text.trim_start().starts_with("(+")
                    });
                    if changes.len() == 1 && opens_with_header {
                        rows.remove(0);
                    }
                }
                for row in rows {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(row.spans);
                    lines.push(Line::from(spans));
                }
            }
            ApprovalKind::Generic { description } => {
                if self.title_layout(request, width, theme).object_clipped {
                    for row in wrap_command(description, inner) {
                        lines.push(Line::from(vec![Span::raw("  "), Span::raw(row)]));
                    }
                }
            }
        }
        if self.mode_label.is_none()
            && let Some(why) = request.policy_reason.as_ref()
        {
            lines.push(Line::from(Span::styled(
                format!("  asking because {why}"),
                dim,
            )));
        }
        lines
    }

    /// The letter that answers with `option` on this card.
    fn option_letter(request: &ApprovalRequest, option: ApprovalDecision) -> char {
        match option {
            ApprovalDecision::Approved => 'y',
            ApprovalDecision::ApprovedForPrefix => 'a',
            ApprovalDecision::ApprovedForSession => {
                if request
                    .options
                    .contains(&ApprovalDecision::ApprovedForPrefix)
                {
                    's'
                } else {
                    'a'
                }
            }
            ApprovalDecision::Denied => 'n',
            ApprovalDecision::Redirect => 't',
        }
    }

    /// The short words for one choice in the choices row.
    fn option_text(request: &ApprovalRequest, option: ApprovalDecision) -> String {
        match option {
            ApprovalDecision::Approved => "allow".to_string(),
            ApprovalDecision::ApprovedForPrefix => match request.allow_rule() {
                Some(_) if matches!(request.kind, ApprovalKind::Patch { .. }) => {
                    "always for this file".to_string()
                }
                Some(rule) => format!("always for \"{}\"", rule.label.trim_end_matches('…').trim()),
                None => match request.execpolicy_amendment() {
                    Some(prefix) => format!("always for \"{}\"", prefix.join(" ")),
                    None => "always".to_string(),
                },
            },
            ApprovalDecision::ApprovedForSession => "for this session".to_string(),
            ApprovalDecision::Denied => "deny".to_string(),
            ApprovalDecision::Redirect => "tell it what to do".to_string(),
        }
    }

    /// What the highlighted choice installs, shown only while it is
    /// highlighted. Prints the rule's exact patterns, never a paraphrase.
    fn selected_note(&self, request: &ApprovalRequest) -> Option<String> {
        match request.options.get(self.selected)? {
            ApprovalDecision::ApprovedForPrefix => match request.allow_rule() {
                Some(rule) => {
                    let covers = if matches!(request.kind, ApprovalKind::Exec { .. }) {
                        "also covers longer commands starting with it"
                    } else {
                        "this file only"
                    };
                    Some(format!(
                        "{} · {covers} · in this project · a deny rule still wins",
                        rule.installs()
                    ))
                }
                None => request
                    .execpolicy_amendment()
                    .map(|prefix| format!("Persists shell({}) for this session", prefix.join(" "))),
            },
            ApprovalDecision::ApprovedForSession => match &request.kind {
                ApprovalKind::Exec { command, .. } => {
                    let scope = session_allowance_scope(command);
                    Some(format!("{} · {}", scope[0], scope[1]))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The choices row (one row when it fits with the mode and "esc denies"
    /// at the right; else the choices alone, else one per row), then the
    /// highlighted choice's rule note.
    fn footer_rows(
        &self,
        request: &ApprovalRequest,
        inner: usize,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let dim = Style::default().fg(theme.color(Role::Dim));
        // Digits stand in for the letters on a card that letters cannot answer.
        let letters = request.letters_enabled();
        let items: Vec<(String, String)> = request
            .options
            .iter()
            .enumerate()
            .map(|(i, option)| {
                let key = if letters {
                    Self::option_letter(request, *option).to_string()
                } else {
                    (i + 1).to_string()
                };
                (key, Self::option_text(request, *option))
            })
            .collect();
        let hint = match &self.mode_label {
            Some(mode) => format!("{mode} · esc denies"),
            None => "esc denies".to_string(),
        };
        let item_spans = |i: usize, key: &str, text: &str| -> Vec<Span<'static>> {
            let base = if i == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            vec![
                Span::styled(" ".to_string(), base),
                Span::styled(key.to_string(), base.add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {text} "), base),
            ]
        };
        let total: usize = items
            .iter()
            .map(|(k, t)| display_width(k) + display_width(t) + 3)
            .sum::<usize>()
            + items.len().saturating_sub(1);
        let hint_w = display_width(&hint);
        let mut rows: Vec<Line<'static>> = Vec::new();
        if 1 + total + 2 + hint_w <= inner {
            let mut spans = vec![Span::raw(" ")];
            for (i, (key, text)) in items.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.extend(item_spans(i, key, text));
            }
            spans.push(Span::raw(" ".repeat(inner - 1 - total - hint_w)));
            spans.push(Span::styled(hint, dim));
            rows.push(Line::from(spans));
        } else {
            if total < inner {
                let mut spans = vec![Span::raw(" ")];
                for (i, (key, text)) in items.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.extend(item_spans(i, key, text));
                }
                rows.push(Line::from(spans));
            } else {
                for (i, (key, text)) in items.iter().enumerate() {
                    let room = inner.saturating_sub(display_width(key) + 4).max(1);
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(item_spans(i, key, &clip_to_width(text, room)));
                    rows.push(Line::from(spans));
                }
            }
            rows.push(Line::from(Span::styled(format!("  {hint}"), dim)));
        }
        if let Some(note) = self.selected_note(request) {
            // The rule is the user's to read before choosing it: wrap, never cut.
            for row in wrap(&note, inner.saturating_sub(4).max(1) as u16)
                .into_iter()
                .take(3)
            {
                rows.push(Line::from(Span::styled(format!("    {row}"), dim)));
            }
        }
        rows
    }

    /// Answer with option `idx` (a digit or a letter): refused when the
    /// subject is not on screen and the option would approve.
    fn choose(&mut self, idx: usize) -> Outcome {
        let Some(option) = self
            .current
            .as_ref()
            .and_then(|r| r.options.get(idx).copied())
        else {
            return Outcome::Redraw;
        };
        let declines = matches!(
            option,
            ApprovalDecision::Denied | ApprovalDecision::Redirect
        );
        if !self.subject_visible && !declines {
            return Outcome::Consumed;
        }
        self.selected = idx;
        self.accept_selected();
        Outcome::Redraw
    }

    /// Whether a letter key may answer the current card right now.
    fn letters_allowed(&self) -> bool {
        let enabled = self
            .current
            .as_ref()
            .is_some_and(ApprovalRequest::letters_enabled);
        enabled && self.letters_ok_at.is_none_or(|at| Instant::now() >= at)
    }
}

/// The title row, fitted to a width.
struct TitleLayout {
    verb: &'static str,
    /// The object, clipped to the room the row has.
    object: String,
    /// The whole subject did not fit on the title row, so the body repeats it.
    object_clipped: bool,
    counts: Option<(usize, usize)>,
    who: Option<String>,
    show_right: bool,
    /// Columns inside the rail.
    width: usize,
}

/// Path as the card names it: relative to `cwd` when possible, sanitized.
fn shown_path(path: &Path, cwd: &Path) -> String {
    let shown = path
        .strip_prefix(cwd)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    sanitize(&shown)
}

/// Cut `text` to `width` display columns, ending in `…` when cut.
fn clip_to_width(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let w = display_width(grapheme);
        if used + w > budget {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('…');
    out
}

/// Width of the `┃ ` rail prefix.
const RAIL_WIDTH: usize = 2;
/// Rows a command may wrap onto before the card elides the rest.
const COMMAND_MAX_ROWS: usize = 6;
/// Rows of diff an approval card shows before eliding the rest.
const APPROVAL_DIFF_ROWS: usize = 24;

/// Hard-wrap a command (or description) to `width`, bounded, with an
/// elision mark when cut. Newlines in the command are kept as breaks.
fn wrap_command(text: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for logical in text.split('\n') {
        rows.extend(wrap(logical, width.max(1) as u16));
    }
    if rows.len() > COMMAND_MAX_ROWS {
        rows.truncate(COMMAND_MAX_ROWS);
        if let Some(last) = rows.last_mut() {
            last.push('…');
        }
    }
    rows
}

impl Component for ApprovalOverlay {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        let Some(request) = self.current.clone() else {
            return;
        };

        let dim = Style::default().fg(theme.color(Role::Dim));
        let rail = Style::default().fg(theme.color(Role::Attention));
        let inner = (area.width as usize).saturating_sub(RAIL_WIDTH).max(1);

        // Subject first: the verb and object are the title row.
        let layout = self.title_layout(&request, area.width, theme);
        let mut body: Vec<Line<'static>> = vec![self.title_row(&layout, theme)];
        let subject_start = body.len();
        body.extend(self.body_lines(area.width, theme));
        let subject_len = body.len() - subject_start;
        body.push(Line::from(""));
        body.extend(self.footer_rows(&request, inner, theme));

        // Everything wears the rail; the card is bottom-anchored and, when the
        // area is short, sheds spacer and subject rows (never the choices).
        let max_rows = area.height as usize;
        self.subject_visible = true;
        if body.len() > max_rows {
            let mut excess = body.len() - max_rows;
            // The spacer between subject and choices goes first.
            body.remove(subject_start + subject_len);
            excess -= 1;
            if excess > 0 && subject_len > 0 {
                if excess + 1 < subject_len {
                    // Shed the tail of the subject behind an explicit marker.
                    let keep = subject_len - excess - 1;
                    let hidden = subject_len - keep;
                    body.drain(subject_start + keep..subject_start + subject_len);
                    body.insert(
                        subject_start + keep,
                        Line::from(Span::styled(
                            format!("  … {hidden} more lines hidden, enlarge the terminal"),
                            dim,
                        )),
                    );
                } else {
                    // Not even one subject row fits: approving keys are refused.
                    self.subject_visible = false;
                    body.truncate(1);
                    body.push(Line::from(Span::styled(
                        "enlarge the terminal to approve · esc denies",
                        dim,
                    )));
                }
            }
        }
        let height = (body.len() as u16).min(area.height);
        let lines: Vec<Line<'static>> = body
            .into_iter()
            .map(|line| {
                let mut spans = vec![Span::styled("┃ ".to_string(), rail)];
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect();
        let view_area = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(height),
            width: area.width,
            height,
        };
        // Opaque card: whatever the live region painted underneath (working
        // row, composer, queue) must not show through.
        frame.render_widget(ratatui::widgets::Clear, view_area);
        frame.render_widget(Paragraph::new(lines), view_area);

        // The overlay is now visibly on screen: keys arriving from this
        // frame onward were typed against *this* request and may decide;
        // letters wait out `LETTER_DELAY` first.
        if !self.armed {
            self.letters_ok_at = Some(Instant::now() + LETTER_DELAY);
        }
        self.armed = true;
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if self.current.is_none() {
            return Outcome::Ignored;
        }
        let Event::Key(key) = event else {
            return Outcome::Ignored;
        };
        if key.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        // Anything typed before this request was rendered cannot decide it:
        // swallow the key so it neither approves nor leaks to the composer.
        if !self.armed {
            return Outcome::Consumed;
        }
        match key.code {
            KeyCode::Up | KeyCode::Left => {
                self.move_selection(-1);
                Outcome::Redraw
            }
            KeyCode::Down | KeyCode::Right => {
                self.move_selection(1);
                Outcome::Redraw
            }
            KeyCode::Enter => {
                if !self.subject_visible && !self.selected_declines() {
                    return Outcome::Consumed;
                }
                self.accept_selected();
                Outcome::Redraw
            }
            KeyCode::Char(c @ '1'..='9') => self.choose((c as usize) - ('1' as usize)),
            // Letters answer only a card the user was not typing through.
            KeyCode::Char(c @ ('y' | 'a' | 'n' | 't' | 's')) => {
                if !self.letters_allowed()
                    || key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    return Outcome::Consumed;
                }
                let idx = self.current.as_ref().and_then(|request| {
                    request
                        .options
                        .iter()
                        .position(|option| Self::option_letter(request, *option) == c)
                });
                match idx {
                    Some(idx) => self.choose(idx),
                    None => Outcome::Consumed,
                }
            }
            KeyCode::Esc => {
                self.reject_current();
                Outcome::Redraw
            }
            _ => Outcome::Consumed,
        }
    }

    fn desired_height(&self, _width: u16, available: u16) -> u16 {
        available.min(14)
    }
}

/// Who recorded an approval decision — the transcript wording differs
/// ("You approved" vs "Auto-reviewer denied").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionActor {
    User,
    AutoReviewer,
}

impl DecisionActor {
    fn subject(self) -> &'static str {
        match self {
            DecisionActor::User => "You",
            DecisionActor::AutoReviewer => "Auto-reviewer",
        }
    }
}

/// How a pending request settled, for the transcript record. Superset of
/// `ApprovalDecision`: the authority can also resolve a request the user
/// never touched (timeout, abort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionOutcome {
    Approved,
    ApprovedForSession,
    Denied,
    TimedOut,
    Aborted,
}

/// A transcript cell recording a settled approval — adapted from codex's
/// `new_approval_decision_cell` (`history_cell/approvals.rs`) with the
/// decision vocabulary narrowed to the host app's and colours moved to roles.
///
/// `subject` is a short display summary of the approved action (command
/// snippet, patch target count, …); it is sanitized here.
pub fn approval_decision_cell(
    subject: &str,
    outcome: DecisionOutcome,
    actor: DecisionActor,
) -> Box<dyn Cell> {
    let subject = sanitize(subject);
    let subject = subject.trim().to_string();

    let (glyph_role, summary) = match outcome {
        DecisionOutcome::Approved => (
            Role::Success,
            match actor {
                DecisionActor::User if subject.is_empty() => "allowed this request".to_string(),
                DecisionActor::User => format!("allowed {subject}"),
                DecisionActor::AutoReviewer if subject.is_empty() => {
                    "Auto-reviewer approved this request".to_string()
                }
                DecisionActor::AutoReviewer => format!("Auto-reviewer approved {subject}"),
            },
        ),
        DecisionOutcome::ApprovedForSession => (
            Role::Success,
            match actor {
                DecisionActor::User if subject.is_empty() => {
                    "allowed this request · this session".to_string()
                }
                DecisionActor::User => format!("allowed {subject} · this session"),
                DecisionActor::AutoReviewer if subject.is_empty() => {
                    "Auto-reviewer approved this request for the session".to_string()
                }
                DecisionActor::AutoReviewer => {
                    format!("Auto-reviewer approved {subject} for the session")
                }
            },
        ),
        DecisionOutcome::Denied => (
            Role::Failure,
            match actor {
                DecisionActor::User if subject.is_empty() => "rejected this request".to_string(),
                DecisionActor::User => format!("rejected {subject}"),
                DecisionActor::AutoReviewer if subject.is_empty() => "Request denied".to_string(),
                DecisionActor::AutoReviewer => format!("Request denied for {subject}"),
            },
        ),
        DecisionOutcome::TimedOut => (
            Role::Failure,
            if subject.is_empty() {
                "Review timed out before this request could be approved".to_string()
            } else {
                format!("Review timed out before {subject} could run")
            },
        ),
        DecisionOutcome::Aborted => (
            Role::Failure,
            if subject.is_empty() {
                format!("{} canceled this request", actor.subject())
            } else {
                format!("{} canceled the request to {subject}", actor.subject())
            },
        ),
    };

    Box::new(RoleMarkedCell {
        glyph_role,
        summary,
    })
}

/// A one-line `✔`/`✗`-marked summary cell — the transcript record shape
/// codex uses for approvals (marker span + wrapped summary under a `  `
/// hanging indent). Kept private since `approval_decision_cell` is the
/// only constructor path.
struct RoleMarkedCell {
    glyph_role: Role,
    summary: String,
}

impl Cell for RoleMarkedCell {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        // Aligned with the tool ledger: two spaces, glyph, then the line.
        let glyph = format!("  {} ", theme.glyph(self.glyph_role));
        let marker = Span::styled(glyph, Style::default().fg(theme.color(self.glyph_role)));
        let cell = PrefixedWrappedCell::new(
            Line::from(Span::raw(self.summary.clone())),
            marker,
            Span::raw("    "),
        );
        cell.display_lines(width, theme)
    }
}
