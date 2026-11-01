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
//!   cannot decide it either.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use crate::component::RenderFrameExt as _;
use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::component::{Component, Event, Outcome, Role, Theme};

use super::cell::{Cell, PrefixedWrappedCell};
use super::diff::{FileChange, create_diff_preview};
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
        }
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
    /// Model name shown in the redirect option label.
    model_label: Option<String>,
    /// False until `render` has run once for the current request. Gate for
    /// "input buffered before the overlay appeared must not approve".
    armed: bool,
}

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
        if let Some(current) = &self.current {
            if current.request_id == request_id {
                self.advance();
                return true;
            }
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

    /// Mark the current request as already drawn, as a render would.
    /// Host-level tests have no frame to draw into.
    #[doc(hidden)]
    pub fn arm_for_tests(&mut self) {
        self.armed = true;
    }

    /// Name the redirect option after the active model.
    pub fn set_model_label(&mut self, model: Option<String>) {
        self.model_label = model.map(|m| sanitize(&m)).filter(|m| !m.trim().is_empty());
    }

    fn option_label(&self, option: ApprovalDecision) -> String {
        match (option, &self.model_label) {
            (ApprovalDecision::Redirect, Some(model)) => {
                // The label may carry `model · effort`; the sentence names the
                // model only.
                let model = model.split(" · ").next().unwrap_or(model);
                format!("No, and tell {model} what to do instead")
            }
            _ => option.label().to_string(),
        }
    }

    /// The request currently displayed, if any.
    pub fn current_request(&self) -> Option<&ApprovalRequest> {
        self.current.as_ref()
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

    /// The card title: what is being approved, in words.
    fn title(request: &ApprovalRequest) -> &'static str {
        match request.kind {
            ApprovalKind::Exec { .. } => "Approve shell command",
            ApprovalKind::Patch { .. } => "Approve file change",
            ApprovalKind::Generic { .. } => "Approve request",
        }
    }

    /// The card body: the thing being approved, where it runs, and why the
    /// policy is asking. Every optional line appears only when its source
    /// supplied it.
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
                for row in wrap_command(command, inner) {
                    lines.push(Line::from(vec![Span::raw("  "), Span::styled(row, bright)]));
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
                for row in create_diff_preview(changes, cwd, inner, APPROVAL_DIFF_ROWS, theme) {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(row.spans);
                    lines.push(Line::from(spans));
                }
            }
            ApprovalKind::Generic { description } => {
                for row in wrap_command(description, inner) {
                    lines.push(Line::from(vec![Span::raw("  "), Span::styled(row, bright)]));
                }
            }
        }
        if let Some(why) = request.policy_reason.as_ref() {
            lines.push(Line::from(Span::styled(
                format!("  asking because {why}"),
                dim,
            )));
        }
        lines
    }
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
        let bright = Style::default().add_modifier(Modifier::BOLD);
        let inner = (area.width as usize).saturating_sub(RAIL_WIDTH).max(1);

        // Title row: what is being approved, the active model on the right.
        let mut title_spans = vec![Span::styled(Self::title(&request).to_string(), bright)];
        if let Some(model) = self.model_label.as_ref() {
            let name = model.split(" · ").next().unwrap_or(model).to_string();
            let used = display_width(Self::title(&request)) + display_width(&name);
            if used + 3 <= inner {
                title_spans.push(Span::raw(" ".repeat(inner - used - 1)));
                title_spans.push(Span::styled(name, dim));
            }
        }

        let mut body: Vec<Line<'static>> = vec![Line::from(title_spans), Line::from("")];
        body.extend(self.body_lines(area.width, theme));
        body.push(Line::from(""));

        for (i, option) in request.options.iter().enumerate() {
            let selected = i == self.selected;
            let marker = if selected { "› " } else { "  " };
            let name_style = if selected { bright } else { Style::default() };
            let label = match (option, &request.kind) {
                (ApprovalDecision::ApprovedForSession, ApprovalKind::Exec { command, .. }) => {
                    session_allowance_scope(command)[0].clone()
                }
                (ApprovalDecision::ApprovedForPrefix, _) if request.allow_rule().is_some() => {
                    format!(
                        "Always allow: {}",
                        request
                            .allow_rule()
                            .map(|r| r.label.as_str())
                            .unwrap_or_default()
                    )
                }
                (ApprovalDecision::ApprovedForPrefix, _) => request
                    .execpolicy_amendment()
                    .map(|prefix| format!("Always allow prefix: {}", prefix.join(" ")))
                    .unwrap_or_else(|| "Always allow this prefix".to_string()),
                _ => self.option_label(*option),
            };
            let label_budget = inner.saturating_sub(display_width(marker) + 3).max(1);
            for (row_index, row) in wrap(&label, label_budget as u16).into_iter().enumerate() {
                if row_index == 0 {
                    body.push(Line::from(vec![
                        Span::styled(marker.to_string(), name_style),
                        Span::styled(format!("{}  ", i + 1), if selected { bright } else { dim }),
                        Span::styled(row, name_style),
                    ]));
                } else {
                    body.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled(row, name_style),
                    ]));
                }
            }
            if let (ApprovalDecision::ApprovedForSession, ApprovalKind::Exec { command, .. }) =
                (option, &request.kind)
            {
                let scope = &session_allowance_scope(command)[1];
                for row in wrap(scope, inner.saturating_sub(8).max(1) as u16) {
                    body.push(Line::from(Span::styled(format!("        {row}"), dim)));
                }
            }
            if *option == ApprovalDecision::ApprovedForPrefix {
                if let Some(rule) = request.allow_rule() {
                    for line in [rule.installs(), rule.scope_note.clone()] {
                        for row in wrap(&line, inner.saturating_sub(8).max(1) as u16) {
                            body.push(Line::from(Span::styled(format!("        {row}"), dim)));
                        }
                    }
                } else if let Some(prefix) = request.execpolicy_amendment() {
                    let scope = format!("Persists shell({}) for this session", prefix.join(" "));
                    for row in wrap(&scope, inner.saturating_sub(8).max(1) as u16) {
                        body.push(Line::from(Span::styled(format!("        {row}"), dim)));
                    }
                }
            }
        }
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            format!(
                "1–{} or ↑↓ ⏎ choose · esc reject",
                request.options.len().max(1)
            ),
            dim,
        )));

        // Everything wears the rail; the card is bottom-anchored and, when the
        // area is short, sheds body rows after the title (never the options
        // or the hint).
        let max_rows = area.height as usize;
        if body.len() > max_rows {
            let excess = body.len() - max_rows;
            let drop_to = (1 + excess).min(body.len().saturating_sub(1));
            body.drain(1..drop_to);
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
        // frame onward were typed against *this* request and may decide.
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
            KeyCode::Up => {
                self.move_selection(-1);
                Outcome::Redraw
            }
            KeyCode::Down => {
                self.move_selection(1);
                Outcome::Redraw
            }
            KeyCode::Enter => {
                self.accept_selected();
                Outcome::Redraw
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                let has = self.current.as_ref().is_some_and(|r| idx < r.options.len());
                if has {
                    self.selected = idx;
                    self.accept_selected();
                }
                Outcome::Redraw
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Background, ColorDepth};
    use crossterm::event::KeyEvent;

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, crossterm::event::KeyModifiers::NONE))
    }

    fn exec_request(id: &str) -> ApprovalRequest {
        ApprovalRequest::new(
            "s1",
            id,
            ApprovalKind::Exec {
                command: "rm -rf build".into(),
                cwd: Some("D:\\repo".into()),
                reason: Some("clean build outputs".into()),
                writes: None,
                network: None,
            },
            vec![
                ApprovalDecision::Approved,
                ApprovalDecision::ApprovedForSession,
                ApprovalDecision::Denied,
            ],
        )
    }

    #[test]
    fn a_request_from_the_wrong_session_cannot_be_queued() {
        let mut overlay = ApprovalOverlay::new("s1");
        let foreign = ApprovalRequest::new(
            "other-session",
            "r9",
            ApprovalKind::Generic {
                description: "do a thing".into(),
            },
            vec![ApprovalDecision::Approved],
        );
        assert!(!overlay.enqueue(foreign));
        assert!(overlay.is_complete());
    }

    #[test]
    fn a_duplicate_request_id_is_never_presented_twice() {
        let mut overlay = ApprovalOverlay::new("s1");
        assert!(overlay.enqueue(exec_request("r1")));
        assert!(!overlay.enqueue(exec_request("r1")));
    }

    struct BufFrame {
        buf: ratatui::buffer::Buffer,
    }

    impl crate::component::RenderFrame for BufFrame {
        fn area(&self) -> Rect {
            self.buf.area
        }
        fn buffer_mut(&mut self) -> &mut ratatui::buffer::Buffer {
            &mut self.buf
        }
        fn set_cursor(&mut self, _position: ratatui::layout::Position) {}
    }

    /// Regression: the card painted over the live region without clearing it,
    /// so the working row and composer text showed through the option rows.
    #[test]
    fn the_card_is_opaque_over_whatever_was_painted_underneath() {
        use crate::component::RenderFrame as _;
        let area = Rect::new(0, 0, 60, 20);
        let mut frame = BufFrame {
            buf: ratatui::buffer::Buffer::empty(area),
        };
        for y in 0..area.height {
            frame
                .buf
                .set_string(0, y, "UNDERNEATH".repeat(6), Style::default());
        }
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.render(&mut frame, area, &theme());
        let dump: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| frame.buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let card_top = dump.find("Approve shell command").expect("card drawn");
        assert!(
            !dump[card_top..].contains("UNDERNEATH"),
            "underlying paint leaked through the card:\n{dump}"
        );
        let _ = frame.area();
    }

    #[test]
    fn keys_buffered_before_the_first_render_cannot_approve() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        // A buffered Enter arrives before the overlay has ever drawn.
        let outcome = overlay.handle(&key(KeyCode::Enter));
        assert!(outcome.is_terminal());
        assert!(
            overlay.drain_intents().is_empty(),
            "a pre-render keypress must not resolve the request"
        );
        assert!(overlay.current_request().is_some());
    }

    #[test]
    fn an_armed_overlay_resolves_with_the_requests_own_ids() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        // Simulate the host's first-presented frame.
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Enter));
        let intents = overlay.drain_intents();
        assert_eq!(
            intents,
            vec![ApprovalIntent::Resolve {
                session_id: "s1".into(),
                request_id: "r1".into(),
                decision: ApprovalDecision::Approved,
            }]
        );
        assert!(overlay.is_complete());
    }

    #[test]
    fn a_queued_request_cannot_be_decided_by_keys_queued_before_it_draws() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.enqueue(exec_request("r2"));
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Enter)); // resolves r1
        overlay.drain_intents();
        // r2 promoted but not yet rendered — this key belongs to r1's frame.
        let outcome = overlay.handle(&key(KeyCode::Enter));
        assert!(outcome.is_terminal());
        assert!(
            overlay.drain_intents().is_empty(),
            "a key queued before r2 rendered must not decide it"
        );
    }

    #[test]
    fn resolving_externally_dismisses_without_emitting_a_decision() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.armed = true;
        assert!(overlay.resolve_external("s1", "r1"));
        assert!(overlay.current_request().is_none());
        assert!(overlay.drain_intents().is_empty());
    }

    #[test]
    fn resolving_an_unknown_id_changes_nothing() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        assert!(!overlay.resolve_external("s1", "nope"));
        assert!(!overlay.resolve_external("other-session", "r1"));
        assert_eq!(overlay.pending_ids(), vec!["r1"]);
    }

    #[test]
    fn escape_rejects_the_current_request() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Esc));
        assert_eq!(
            overlay.drain_intents(),
            vec![ApprovalIntent::Rejected {
                session_id: "s1".into(),
                request_id: "r1".into(),
            }]
        );
        assert!(overlay.current_request().is_none());
    }

    #[test]
    fn escape_rejects_every_queued_request_with_the_turn() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.enqueue(exec_request("r2"));
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Esc));
        let ids: Vec<_> = overlay
            .drain_intents()
            .into_iter()
            .map(|i| match i {
                ApprovalIntent::Rejected { request_id, .. } => request_id,
                other => panic!("unexpected intent {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["r1", "r2"]);
        assert!(overlay.is_complete());
    }

    #[test]
    fn redirect_option_rejects_like_escape_and_names_the_model() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.set_model_label(Some("sonnet-5.5".into()));
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "r1",
            ApprovalKind::Generic {
                description: "x".into(),
            },
            vec![
                ApprovalDecision::Approved,
                ApprovalDecision::Denied,
                ApprovalDecision::Redirect,
            ],
        ));
        assert_eq!(
            overlay.option_label(ApprovalDecision::Redirect),
            "No, and tell sonnet-5.5 what to do instead"
        );
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Char('3')));
        assert_eq!(
            overlay.drain_intents(),
            vec![ApprovalIntent::Rejected {
                session_id: "s1".into(),
                request_id: "r1".into(),
            }]
        );
        assert!(overlay.is_complete());
    }

    #[test]
    fn escape_before_first_render_is_swallowed() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.handle(&key(KeyCode::Esc));
        assert!(overlay.drain_intents().is_empty());
        assert_eq!(overlay.pending_ids(), vec!["r1"]);
    }

    #[test]
    fn digit_shortcuts_resolve_the_named_option() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Char('3')));
        assert_eq!(
            overlay.drain_intents(),
            vec![ApprovalIntent::Resolve {
                session_id: "s1".into(),
                request_id: "r1".into(),
                decision: ApprovalDecision::Denied,
            }]
        );
    }

    #[test]
    fn hostile_request_text_cannot_inject_controls_into_the_overlay() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "r1",
            ApprovalKind::Exec {
                command: "rm \x1b[2J-rf /".into(),
                cwd: None,
                reason: Some("\x1b]0;pwned\x07".into()),
                writes: Some("\x1b[2Jout".into()),
                network: None,
            },
            vec![ApprovalDecision::Approved],
        ));
        let body = overlay.body_lines(80, &theme());
        for line in body {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(!text.contains('\u{1b}') && !text.contains('\u{7}'));
        }
    }

    #[test]
    fn decision_cells_record_the_outcome_with_a_state_glyph() {
        let cell = approval_decision_cell(
            "rm -rf build",
            DecisionOutcome::Approved,
            DecisionActor::User,
        );
        let lines = cell.display_lines(80, &theme());
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('✓') || text.contains('✔'));
        assert_eq!(text.trim_end(), "  ✓ allowed rm -rf build");

        let user_denied = approval_decision_cell(
            "shell · echo hi",
            DecisionOutcome::Denied,
            DecisionActor::User,
        );
        let text: String = user_denied.display_lines(80, &theme())[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text.trim_end(), "  ✗ rejected shell · echo hi");

        let session = approval_decision_cell(
            "shell · echo hi",
            DecisionOutcome::ApprovedForSession,
            DecisionActor::User,
        );
        let text: String = session.display_lines(80, &theme())[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            text.trim_end(),
            "  ✓ allowed shell · echo hi · this session"
        );

        let denied =
            approval_decision_cell("", DecisionOutcome::Denied, DecisionActor::AutoReviewer);
        let lines = denied.display_lines(80, &theme());
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("denied"));
    }

    #[test]
    fn session_allowance_names_exactly_the_command_the_grant_covers() {
        let lines = session_allowance_scope("rustright cargo test -p router retry");
        assert_eq!(
            lines[0],
            "Allow for this session: rustright cargo test -p router retry"
        );
        // The authority's rule is the exact command string: no prefix, no
        // wildcard, no directory binding — and the card claims none.
        assert!(lines[1].contains("only this exact command"), "{}", lines[1]);
        assert!(lines[1].contains("chained forms"), "{}", lines[1]);
        assert!(!lines[1].contains("only in "), "{}", lines[1]);
        let chained = session_allowance_scope("make a && make b");
        assert_eq!(chained[0], "Allow for this session: make a && make b");
    }

    fn render_dump(overlay: &mut ApprovalOverlay, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut frame = BufFrame {
            buf: ratatui::buffer::Buffer::empty(area),
        };
        overlay.render(&mut frame, area, &theme());
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| frame.buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .filter(|row| !row.is_empty())
            .collect()
    }

    fn shell_request(policy: Option<&str>) -> ApprovalRequest {
        ApprovalRequest::new(
            "s1",
            "r1",
            ApprovalKind::Exec {
                command: "rustright cargo test -p router retry".into(),
                cwd: Some(r"D:\Claude\myrepo\engine".into()),
                reason: None,
                writes: None,
                network: None,
            },
            vec![
                ApprovalDecision::Approved,
                ApprovalDecision::ApprovedForSession,
                ApprovalDecision::Denied,
                ApprovalDecision::Redirect,
            ],
        )
        .with_policy_reason(policy.map(str::to_string))
    }

    /// Spec section 05: an amber rail card with no box — title, the command,
    /// where it runs, why it is asking, four numbered options, a hint.
    #[test]
    fn the_card_is_an_amber_rail_with_the_spec_rows_and_no_box() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.set_model_label(Some("gpt-6.1-sol · high".into()));
        overlay.enqueue(shell_request(Some(
            "shell commands need approval in Ask mode",
        )));
        let rows = render_dump(&mut overlay, 100, 24);
        let joined = rows.join("\n");
        assert!(rows[0].starts_with("┃ Approve shell command"), "{joined}");
        assert!(rows[0].ends_with("gpt-6.1-sol"), "{joined}");
        assert!(
            rows.iter()
                .any(|r| r == "┃   rustright cargo test -p router retry"),
            "{joined}"
        );
        assert!(
            rows.iter()
                .any(|r| r == r"┃   runs in D:\Claude\myrepo\engine"),
            "{joined}"
        );
        assert!(
            rows.iter()
                .any(|r| r == "┃   asking because shell commands need approval in Ask mode"),
            "{joined}"
        );
        assert!(rows.iter().any(|r| r == "┃ › 1  Allow once"), "{joined}");
        assert!(
            rows.iter()
                .any(|r| r == "┃   2  Allow for this session: rustright cargo test -p router retry"),
            "{joined}"
        );
        assert!(
            rows.iter()
                .any(|r| r.starts_with("┃         only this exact command")),
            "{joined}"
        );
        assert!(rows.iter().any(|r| r == "┃   3  Deny"), "{joined}");
        assert!(
            rows.iter()
                .any(|r| r == "┃   4  No, and tell gpt-6.1-sol what to do instead"),
            "{joined}"
        );
        assert_eq!(
            rows.last().unwrap(),
            "┃ 1–4 or ↑↓ ⏎ choose · esc reject",
            "{joined}"
        );
        // No box drawing: every row is the rail and content.
        assert!(
            !joined.contains('┌') && !joined.contains('└') && !joined.contains('│'),
            "{joined}"
        );
        // The rail is the attention colour.
        let area = Rect::new(0, 0, 100, 24);
        let mut frame = BufFrame {
            buf: ratatui::buffer::Buffer::empty(area),
        };
        overlay.render(&mut frame, area, &theme());
        let rail_cell = (0..area.height)
            .map(|y| &frame.buf[(0, y)])
            .find(|c| c.symbol() == "┃")
            .expect("rail drawn");
        assert_eq!(rail_cell.fg, theme().color(Role::Attention));
    }

    #[test]
    fn unknown_effect_fields_and_policy_reasons_are_omitted_not_invented() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "r1",
            ApprovalKind::Exec {
                command: "echo hi".into(),
                cwd: None,
                reason: None,
                writes: None,
                network: None,
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        ));
        let rows = render_dump(&mut overlay, 80, 20);
        let joined = rows.join("\n");
        assert!(!joined.contains("runs in"), "{joined}");
        assert!(!joined.contains("network"), "{joined}");
        assert!(!joined.contains("writes"), "{joined}");
        assert!(!joined.contains("asking because"), "{joined}");
        assert!(!joined.contains("for this session"), "{joined}");
        assert!(joined.contains("┃ › 1  Allow once"), "{joined}");
        assert!(joined.contains("┃   2  Deny"), "{joined}");
        assert!(
            joined.contains("1–2 or ↑↓ ⏎ choose · esc reject"),
            "{joined}"
        );
    }

    #[test]
    fn declared_writes_and_network_join_the_runs_in_line() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "r1",
            ApprovalKind::Exec {
                command: "make".into(),
                cwd: Some("D:\\repo".into()),
                reason: None,
                writes: Some("target/".into()),
                network: Some("off".into()),
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        ));
        let rows = render_dump(&mut overlay, 80, 20);
        assert!(
            rows.iter()
                .any(|r| r == "┃   runs in D:\\repo · writes target/ · network off"),
            "{rows:?}"
        );
    }

    #[test]
    fn the_session_option_selects_by_digit_and_resolves_for_session() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(shell_request(None));
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Char('2')));
        assert_eq!(
            overlay.drain_intents(),
            vec![ApprovalIntent::Resolve {
                session_id: "s1".into(),
                request_id: "r1".into(),
                decision: ApprovalDecision::ApprovedForSession,
            }]
        );
    }

    #[test]
    fn a_short_area_sheds_body_rows_but_keeps_options_and_hint() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(shell_request(Some(
            "shell commands need approval in Ask mode",
        )));
        let rows = render_dump(&mut overlay, 60, 12);
        let joined = rows.join("\n");
        assert!(rows.len() <= 12, "{joined}");
        assert!(rows[0].starts_with("┃ Approve shell command"), "{joined}");
        assert!(
            joined.contains("4  No, and tell the model what to do instead"),
            "{joined}"
        );
        assert!(joined.contains("esc reject"), "{joined}");
    }

    #[test]
    fn patch_and_generic_requests_get_their_own_titles_on_the_same_rail() {
        let mut changes = BTreeMap::new();
        changes.insert(PathBuf::from("src/new.rs"), FileChange::add("fn f() {}"));
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "p1",
            ApprovalKind::Patch {
                changes,
                cwd: PathBuf::new(),
                reason: None,
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        ));
        let rows = render_dump(&mut overlay, 80, 24);
        assert!(rows[0].starts_with("┃ Approve file change"), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("src/new.rs")), "{rows:?}");
        assert!(rows.iter().all(|r| r.starts_with('┃')), "{rows:?}");

        let mut generic = ApprovalOverlay::new("s1");
        generic.enqueue(ApprovalRequest::new(
            "s1",
            "g1",
            ApprovalKind::Generic {
                description: "allow network access".into(),
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        ));
        let rows = render_dump(&mut generic, 80, 24);
        assert!(rows[0].starts_with("┃ Approve request"), "{rows:?}");
        assert!(
            rows.iter().any(|r| r == "┃   allow network access"),
            "{rows:?}"
        );
    }

    fn rule_request(command: &str, policy: Option<&str>) -> ApprovalRequest {
        let kind = ApprovalKind::Exec {
            command: command.into(),
            cwd: None,
            reason: None,
            writes: None,
            network: None,
        };
        let rule = super::super::allow_rule::derive_allow_rule(&kind, policy);
        ApprovalRequest::new(
            "s1",
            "r1",
            kind,
            vec![
                ApprovalDecision::Approved,
                ApprovalDecision::ApprovedForPrefix,
                ApprovalDecision::Denied,
            ],
        )
        .with_allow_rule(rule)
    }

    #[test]
    fn the_always_allow_option_prints_the_exact_rule_it_installs() {
        let mut overlay = ApprovalOverlay::new("s1");
        assert!(overlay.enqueue(rule_request("git status --short", Some("mode:ask"))));
        let rows = render_dump(&mut overlay, 120, 30).join("\n");
        assert!(rows.contains("Always allow: git status"), "{rows}");
        assert!(rows.contains("shell(git status)"), "{rows}");
        assert!(rows.contains("shell(git status *)"), "{rows}");
    }

    #[test]
    fn no_always_allow_option_without_an_eligible_rule() {
        for (command, policy) in [
            ("ls; id", Some("mode:ask")),
            ("bash x.sh", Some("mode:ask")),
            ("ls", Some("rule:session:shell(ls)")),
        ] {
            let mut overlay = ApprovalOverlay::new("s1");
            assert!(overlay.enqueue(rule_request(command, policy)));
            let request = overlay.current_request().expect("card");
            assert!(
                !request
                    .options
                    .contains(&ApprovalDecision::ApprovedForPrefix),
                "{command}"
            );
        }
    }

    #[test]
    fn a_destructive_command_shows_what_it_would_change_above_the_options() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(exec_request("r1"));
        let joined = render_dump(&mut overlay, 100, 24).join("\n");
        assert!(joined.contains("! Deletes files: build"), "{joined}");
        assert!(joined.contains("recursively deletes build"), "{joined}");
        // Proceed and cancel remain the card's options.
        assert!(joined.contains("3  Deny"), "{joined}");
    }

    #[test]
    fn a_harmless_command_gets_no_destructive_preview() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "r2",
            ApprovalKind::Exec {
                command: "cargo check".into(),
                cwd: None,
                reason: None,
                writes: None,
                network: None,
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        ));
        let joined = render_dump(&mut overlay, 100, 24).join("\n");
        assert!(!joined.contains("! "), "{joined}");
    }

    fn resolve_with(request: ApprovalRequest, keys: &[KeyCode]) -> Vec<ApprovalIntent> {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(request);
        overlay.armed = true;
        for code in keys {
            overlay.handle(&key(*code));
        }
        overlay.drain_intents()
    }

    fn resolved(decision: ApprovalDecision) -> Vec<ApprovalIntent> {
        vec![ApprovalIntent::Resolve {
            session_id: "s1".into(),
            request_id: "r1".into(),
            decision,
        }]
    }

    #[test]
    fn the_prompt_renders_the_command_its_effects_and_the_destructive_risk() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(ApprovalRequest::new(
            "s1",
            "r1",
            ApprovalKind::Exec {
                command: "rm -rf build".into(),
                cwd: Some("D:\\repo".into()),
                reason: Some("clean outputs".into()),
                writes: None,
                network: None,
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        ));
        let joined = render_dump(&mut overlay, 100, 30).join("\n");
        assert!(joined.contains("Approve shell command"), "{joined}");
        assert!(joined.contains("rm -rf build"), "{joined}");
        assert!(joined.contains("runs in D:\\repo"), "{joined}");
        assert!(joined.contains("model's reason: clean outputs"), "{joined}");
        assert!(joined.contains("  ! "), "risk headline missing: {joined}");
    }

    #[test]
    fn approve_key_resolves_approved() {
        assert_eq!(
            resolve_with(shell_request(None), &[KeyCode::Char('1')]),
            resolved(ApprovalDecision::Approved)
        );
    }

    #[test]
    fn deny_by_digit_or_by_arrows_and_enter_resolves_denied() {
        assert_eq!(
            resolve_with(shell_request(None), &[KeyCode::Char('3')]),
            resolved(ApprovalDecision::Denied)
        );
        assert_eq!(
            resolve_with(
                shell_request(None),
                &[KeyCode::Down, KeyCode::Down, KeyCode::Enter]
            ),
            resolved(ApprovalDecision::Denied)
        );
        // Up from the first row wraps to the last (redirect), which rejects.
        assert_eq!(
            resolve_with(shell_request(None), &[KeyCode::Up, KeyCode::Enter]),
            vec![ApprovalIntent::Rejected {
                session_id: "s1".into(),
                request_id: "r1".into(),
            }]
        );
    }

    #[test]
    fn always_allow_key_resolves_for_prefix() {
        assert_eq!(
            resolve_with(
                rule_request("git status --short", Some("mode:ask")),
                &[KeyCode::Char('2')]
            ),
            resolved(ApprovalDecision::ApprovedForPrefix)
        );
    }

    #[test]
    fn a_digit_beyond_the_options_changes_nothing() {
        let mut overlay = ApprovalOverlay::new("s1");
        overlay.enqueue(shell_request(None));
        overlay.armed = true;
        overlay.handle(&key(KeyCode::Char('9')));
        assert!(overlay.drain_intents().is_empty());
        assert!(overlay.current_request().is_some());
    }
}
