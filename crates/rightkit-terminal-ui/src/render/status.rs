//! Compact status indicator: one clipped line naming what the agent is
//! doing right now.
//!
//! Donor lineage: pi `packages/coding-agent/src/modes/interactive/components/
//! status-indicator.ts` at `70c036211de5378508ae2456f3c7ae3cf9f8bc75` —
//! its working/retry/compaction states, adapted to the host app's rules:
//!
//! - **No animation frames.** Pi cycles a spinner glyph per tick; this
//!   renders the static `•`/`✓` role glyphs — state is carried by the glyph
//!   and role colour, never by motion.
//! - **No session accounting.** The pi status line can show token/cost
//!   figures; the host app's strip deliberately omits them — only the activity
//!   state and an optional sanitized detail (e.g. current tool name) draw.
//! - **Width-safe.** The whole row is one [`clipped_line`]; the detail is
//!   the first thing the clip reaches.
//!
//! The host owns the *meaning* — this only draws the state it is handed.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::component::{Role, Theme};
use crate::transcript::wrap::display_width;

use super::cell::clipped_line;
use super::sanitize::sanitize;
use crate::transcript::timeline::RunSignal;

/// What the indicator currently shows. The host maps its scheduler/agent
/// state onto these; `detail` is an optional short suffix (a tool name, a
/// file being read) drawn dimmed and clipped first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// A turn is in flight.
    Working,
    /// A provider call is being retried; carries the attempt number.
    Retrying(u32),
    /// The agent is waiting on an approval decision.
    AwaitingApproval,
    /// The last turn finished; nothing is running.
    Idle,
}

impl StatusKind {
    fn role(self) -> Role {
        match self {
            StatusKind::Working | StatusKind::Retrying(_) => Role::Running,
            StatusKind::AwaitingApproval => Role::Attention,
            StatusKind::Idle => Role::Dim,
        }
    }

    fn label(self) -> String {
        match self {
            StatusKind::Working => "Working".to_string(),
            StatusKind::Retrying(attempt) => format!("Retrying (attempt {attempt})"),
            StatusKind::AwaitingApproval => "Awaiting approval".to_string(),
            StatusKind::Idle => "Idle".to_string(),
        }
    }
}

/// Render the status row at `width`: `• Working — reading foo.rs` style.
/// The detail string is sanitized here — it may carry tool or path names
/// straight off the daemon feed.
pub fn status_line(
    kind: StatusKind,
    detail: Option<&str>,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let role = kind.role();
    let color = theme.color(role);
    let dim = Style::default().fg(theme.color(Role::Dim));

    let mut spans = vec![
        Span::styled(
            format!("{} ", theme.glyph(role)),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(kind.label(), Style::default().fg(color)),
    ];
    if let Some(detail) = detail.map(sanitize).filter(|d| !d.is_empty()) {
        spans.push(Span::styled(format!(" — {detail}"), dim));
    }
    clipped_line(Line::from(spans), width.max(1))
}

/// Codex status_indicator_widget::fmt_elapsed_compact @ 444da310, Apache-2.0.
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

/// Pi's owned working row plus Codex's elapsed/cancel suffix. The host owns
/// turn lifecycle; optional details clip after the core progress affordance.
pub fn working_line(seconds: u64, width: u16, theme: &Theme) -> Line<'static> {
    status_line(
        StatusKind::Working,
        Some(&format!("{} · Ctrl+C cancel", fmt_elapsed_compact(seconds))),
        width,
        theme,
    )
}

/// Which of the four working-line states is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkState {
    /// `●` the turn is running.
    Working,
    /// `!` the turn is paused on the user (approval); no cancel hint.
    Waiting,
    /// `↻` provider retry or remediation.
    Retry,
    /// `✗` rejected or failed.
    Failed,
}

/// One verification stage as the runtime actually reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageMark {
    /// `○` not reached.
    Pending,
    /// `●` running now.
    Active,
    /// `✓` passed.
    Done,
    /// `↻` remediating.
    Retry,
    /// `✗` rejected.
    Failed,
}

/// The work / verify / arbiter track. Only built from real runtime verify or
/// arbiter events; ordinary single-model turns pass `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTrack {
    pub marks: [StageMark; 3],
    /// Index of the stage the actor is in now.
    pub current: usize,
}

const STAGE_NAMES: [&str; 3] = ["work", "verify", "arbiter"];

/// Everything the working line draws. The host owns the meaning.
#[derive(Debug, Clone)]
pub struct WorkingStatus<'a> {
    pub state: WorkState,
    /// Who is acting: the model, `jury · name`, `arbiter · name`.
    pub actor: &'a str,
    /// Full operation (>= 90 columns).
    pub operation: &'a str,
    /// Shortened operation (60-89 columns); defaults to `operation`.
    pub short: Option<&'a str>,
    /// One word (< 60 columns); defaults to the first word of `operation`.
    pub verb: Option<&'a str>,
    pub elapsed_secs: u64,
    pub stages: Option<StageTrack>,
    pub queued: usize,
    /// The row outlives its turn (a held failure): the right side reads
    /// `esc dismiss` instead of `esc cancel`.
    pub dismissable: bool,
}

/// How a [`RunSignal`] reads on the working line and in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalView {
    pub state: WorkState,
    pub operation: String,
    pub short: String,
    pub verb: String,
    /// One-line transcript notice.
    pub notice: String,
}

/// Failure classes the runtime reports in `runFailed.kind`.
fn failure_label(kind: &str) -> &'static str {
    match kind {
        "network" => "network error",
        "timeout" => "provider timed out",
        "rate_limited" => "rate limited",
        "auth" => "authentication failed",
        "provider_error" => "provider error",
        "budget" => "budget exhausted",
        _ => "",
    }
}

/// Map one real runtime signal onto the working line. Pure: the host decides
/// when a signal starts and ends.
pub fn signal_view(signal: &RunSignal) -> SignalView {
    match signal {
        RunSignal::Failed { reason, hint } => {
            let reason = sanitize(reason);
            let reason = reason.trim();
            let label = failure_label(hint).to_string();
            let short = if label.is_empty() {
                reason.chars().take(60).collect::<String>()
            } else {
                label
            };
            let notice = if reason.is_empty() {
                "Turn failed".to_string()
            } else {
                format!("Turn failed: {reason}")
            };
            SignalView {
                state: WorkState::Failed,
                operation: short.clone(),
                short,
                verb: "Failed".into(),
                notice,
            }
        }
        RunSignal::Retrying {
            provider,
            reason,
            attempt,
            delay_secs,
        } => {
            let provider = sanitize(provider);
            let reason = sanitize(reason);
            let mut op = String::from("Retrying");
            for part in [provider.trim(), reason.trim()] {
                if !part.is_empty() {
                    op.push_str(" · ");
                    op.push_str(part);
                }
            }
            let mut tail = String::new();
            if let Some((n, m)) = attempt {
                tail.push_str(&format!(" · attempt {n}/{m}"));
            }
            if let Some(secs) = delay_secs {
                tail.push_str(&format!(" in {secs}s"));
            }
            let full = format!("{op}{tail}");
            SignalView {
                state: WorkState::Retry,
                short: format!("Retrying{tail}"),
                verb: "Retrying".into(),
                notice: full.clone(),
                operation: full,
            }
        }
        RunSignal::Compacting => SignalView {
            state: WorkState::Working,
            operation: "Compacting context…".into(),
            short: "Compacting context".into(),
            verb: "Compacting".into(),
            notice: "Compacting context…".into(),
        },
        RunSignal::Compacted { dropped } => {
            let op = if *dropped > 0 {
                format!("Context compacted · {dropped} messages folded")
            } else {
                "Context compacted".to_string()
            };
            SignalView {
                state: WorkState::Working,
                operation: op.clone(),
                short: "Context compacted".into(),
                verb: "Compacted".into(),
                notice: op,
            }
        }
        RunSignal::Verification {
            passed,
            tool,
            issues,
        } => {
            let tool = sanitize(tool);
            let tool = tool.trim();
            let word = if *passed { "passed" } else { "failed" };
            let mut op = format!("Verification {word}");
            if !tool.is_empty() {
                op.push_str(&format!(" · {tool}"));
            }
            if !*passed && *issues > 0 {
                op.push_str(&format!(" · {issues} issues"));
            }
            SignalView {
                // A failed gate is remediated by the run, not terminal.
                state: if *passed {
                    WorkState::Working
                } else {
                    WorkState::Retry
                },
                operation: op.clone(),
                short: format!("Verification {word}"),
                verb: "Verifying".into(),
                notice: op,
            }
        }
        RunSignal::Check {
            passed,
            jury_skipped,
        } => {
            let word = if *passed { "passed" } else { "failed" };
            let mut op = format!("Check {word}");
            if *jury_skipped {
                op.push_str(" · jury skipped");
            }
            SignalView {
                state: if *passed {
                    WorkState::Working
                } else {
                    WorkState::Retry
                },
                operation: op.clone(),
                short: format!("Check {word}"),
                verb: "Checking".into(),
                notice: op,
            }
        }
    }
}

/// `m:ss` clock used by the working line.
pub fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn stage_glyph(mark: StageMark, theme: &Theme) -> Span<'static> {
    let (g, role) = match mark {
        StageMark::Pending => ("○", Role::Dim),
        StageMark::Active => ("●", Role::Running),
        StageMark::Done => ("✓", Role::Success),
        StageMark::Retry => ("↻", Role::Attention),
        StageMark::Failed => ("✗", Role::Failure),
    };
    Span::styled(g, Style::default().fg(theme.color(role)))
}

/// The one bright line above the composer: `● <actor>  <operation>` on the
/// left, `[stages  ]<clock> · esc cancel` on the right. Width tiers: >= 90
/// full text and stage marks, 60-89 short operation and `verify 2/3`, < 60
/// actor, one word, clock and a bare `esc`. No animation: glyph and colour
/// carry state, only the clock moves.
pub fn working_status_line(status: &WorkingStatus<'_>, width: u16, theme: &Theme) -> Line<'static> {
    let w = usize::from(width);
    let bright = Style::default().fg(theme.color(Role::Text));
    let dim = Style::default().fg(theme.color(Role::Dim));
    let key = Style::default().add_modifier(Modifier::BOLD);
    let (glyph, glyph_style, op_role) = match status.state {
        WorkState::Working => (
            "●",
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
            Role::Text,
        ),
        WorkState::Waiting => (
            "!",
            Style::default()
                .fg(theme.color(Role::Attention))
                .add_modifier(Modifier::BOLD),
            Role::Attention,
        ),
        WorkState::Retry => (
            "↻",
            Style::default().fg(theme.color(Role::Attention)),
            Role::Attention,
        ),
        WorkState::Failed => (
            "✗",
            Style::default().fg(theme.color(Role::Failure)),
            Role::Failure,
        ),
    };
    let op_style = Style::default().fg(theme.color(op_role));
    let actor = sanitize(status.actor);
    let operation = sanitize(status.operation);

    let (actor_text, op_text) = if w >= 90 {
        (actor.clone(), operation.clone())
    } else if w >= 60 {
        (
            actor.clone(),
            status
                .short
                .map(sanitize)
                .unwrap_or_else(|| operation.clone()),
        )
    } else {
        let name = actor.rsplit(" · ").next().unwrap_or(&actor).to_string();
        let verb = status.verb.map(sanitize).unwrap_or_else(|| {
            operation
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        });
        (name, verb)
    };

    let mut right: Vec<Span<'static>> = Vec::new();
    if let Some(track) = status.stages {
        if w >= 90 {
            for (i, name) in STAGE_NAMES.iter().enumerate() {
                let style = if i == track.current { bright } else { dim };
                right.push(Span::styled((*name).to_string(), style));
                right.push(Span::raw(" "));
                right.push(stage_glyph(track.marks[i], theme));
                right.push(Span::raw("  "));
            }
            right.push(Span::raw(" "));
        } else if w >= 60 {
            let i = track.current.min(2);
            right.push(Span::styled(format!("{} {}/3", STAGE_NAMES[i], i + 1), dim));
            right.push(Span::raw("  "));
        }
    }
    right.push(Span::styled(clock(status.elapsed_secs), bright));
    if status.state != WorkState::Waiting {
        if w >= 60 {
            right.push(Span::styled(" · ", dim));
            right.push(Span::styled("esc", key));
            right.push(Span::styled(
                if status.dismissable {
                    " dismiss"
                } else {
                    " cancel"
                },
                dim,
            ));
        } else {
            right.push(Span::raw(" "));
            right.push(Span::styled("esc", key));
        }
    }
    right.push(Span::raw(" "));
    let right_width: usize = right
        .iter()
        .map(|s| display_width(s.content.as_ref()))
        .sum();

    let mut left = vec![
        Span::raw(" "),
        Span::styled(glyph.to_string(), glyph_style),
        Span::raw(" "),
        Span::styled(actor_text, bright),
        Span::raw(if w >= 60 { "  " } else { " " }),
        Span::styled(op_text, op_style),
    ];
    if status.queued > 0 && w >= 60 {
        left.push(Span::styled(format!(" · {} queued", status.queued), dim));
    }
    let left_budget = w.saturating_sub(right_width + 1).max(1);
    let left_line = clipped_line(Line::from(left), left_budget as u16);
    let left_width: usize = left_line
        .spans
        .iter()
        .map(|s| display_width(s.content.as_ref()))
        .sum();
    let gap = w.saturating_sub(left_width + right_width);
    let mut spans = left_line.spans;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    clipped_line(Line::from(spans), width.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Background, ColorDepth};
    use crate::transcript::wrap::display_width;

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn chat_chrome_elapsed_and_working_row_keep_core_text_when_clipped() {
        for (secs, expected) in [
            (0, "0s"),
            (59, "59s"),
            (60, "1m 00s"),
            (3599, "59m 59s"),
            (3600, "1h 00m 00s"),
            (7389, "2h 03m 09s"),
        ] {
            assert_eq!(fmt_elapsed_compact(secs), expected);
        }
        let text = text_of(&working_line(7, 50, &theme()));
        assert!(text.contains("Working — 7s · Ctrl+C cancel"), "{text}");
        let narrow = text_of(&working_line(7, 24, &theme()));
        assert!(narrow.contains("Working — 7s"), "{narrow}");
        assert!(display_width(&narrow) <= 24);
    }

    fn jury() -> WorkingStatus<'static> {
        WorkingStatus {
            state: WorkState::Working,
            actor: "jury · sonnet-5.5",
            operation: "Reviewing the change · seat 2 of 2",
            short: Some("Reviewing"),
            verb: Some("Reviewing"),
            elapsed_secs: 44,
            stages: Some(StageTrack {
                marks: [StageMark::Done, StageMark::Active, StageMark::Pending],
                current: 1,
            }),
            queued: 0,
            dismissable: false,
        }
    }

    #[test]
    fn working_line_width_tiers_follow_the_spec() {
        let wide = text_of(&working_status_line(&jury(), 100, &theme()));
        assert!(wide.contains("jury · sonnet-5.5"), "{wide}");
        assert!(wide.contains("Reviewing the change"), "{wide}");
        assert!(
            wide.contains("work ✓") && wide.contains("verify ●") && wide.contains("arbiter ○"),
            "{wide}"
        );
        assert!(wide.contains("0:44 · esc cancel"), "{wide}");
        assert_eq!(display_width(&wide), 100);

        let mid = text_of(&working_status_line(&jury(), 72, &theme()));
        assert!(mid.contains("verify 2/3"), "{mid}");
        assert!(
            mid.contains("Reviewing") && !mid.contains("seat 2"),
            "{mid}"
        );
        assert!(mid.contains("0:44 · esc cancel"), "{mid}");
        assert!(display_width(&mid) <= 72);

        let narrow = text_of(&working_status_line(&jury(), 44, &theme()));
        assert!(
            narrow.contains("sonnet-5.5") && !narrow.contains("jury"),
            "{narrow}"
        );
        assert!(narrow.contains("0:44 esc"), "{narrow}");
        assert!(
            !narrow.contains("cancel") && !narrow.contains("verify"),
            "{narrow}"
        );
        assert!(display_width(&narrow) <= 44);
    }

    #[test]
    fn ordinary_turns_show_no_stage_markers() {
        let mut s = jury();
        s.stages = None;
        s.actor = "gpt-6.1-sol";
        let text = text_of(&working_status_line(&s, 100, &theme()));
        assert!(
            !text.contains("verify") && !text.contains("arbiter"),
            "{text}"
        );
    }

    #[test]
    fn waiting_hides_cancel_and_each_state_has_its_glyph() {
        for (state, glyph) in [
            (WorkState::Working, "●"),
            (WorkState::Waiting, "!"),
            (WorkState::Retry, "↻"),
            (WorkState::Failed, "✗"),
        ] {
            let mut s = jury();
            s.stages = None;
            s.state = state;
            let text = text_of(&working_status_line(&s, 100, &theme()));
            assert!(text.contains(glyph), "{state:?}: {text}");
            assert_eq!(
                text.contains("esc cancel"),
                state != WorkState::Waiting,
                "{text}"
            );
        }
    }

    #[test]
    fn failed_signal_maps_to_failed_state_with_short_reason() {
        let v = signal_view(&RunSignal::Failed {
            reason: "429 too many requests".into(),
            hint: "rate_limited".into(),
        });
        assert_eq!(v.state, WorkState::Failed);
        assert_eq!(v.operation, "rate limited");
        assert_eq!(v.notice, "Turn failed: 429 too many requests");
        let mut s = jury();
        s.stages = None;
        s.state = v.state;
        s.operation = &v.operation;
        let text = text_of(&working_status_line(&s, 100, &theme()));
        assert!(
            text.contains("✗") && text.contains("rate limited"),
            "{text}"
        );
    }

    #[test]
    fn retry_signal_shows_provider_reason_attempt_and_delay() {
        let v = signal_view(&RunSignal::Retrying {
            provider: "openai".into(),
            reason: "overloaded".into(),
            attempt: Some((2, 5)),
            delay_secs: Some(8),
        });
        assert_eq!(v.state, WorkState::Retry);
        assert_eq!(
            v.operation,
            "Retrying · openai · overloaded · attempt 2/5 in 8s"
        );
        assert_eq!(v.short, "Retrying · attempt 2/5 in 8s");
        let bare = signal_view(&RunSignal::Retrying {
            provider: String::new(),
            reason: "server busy".into(),
            attempt: None,
            delay_secs: Some(3),
        });
        assert_eq!(bare.operation, "Retrying · server busy in 3s");
    }

    #[test]
    fn held_failure_row_offers_dismiss_instead_of_cancel() {
        let v = signal_view(&RunSignal::Failed {
            reason: "boom".into(),
            hint: "network".into(),
        });
        let mut s = jury();
        s.stages = None;
        s.state = v.state;
        s.operation = &v.operation;
        s.dismissable = true;
        let text = text_of(&working_status_line(&s, 100, &theme()));
        assert!(
            text.contains("esc dismiss") && !text.contains("esc cancel"),
            "{text}"
        );
    }

    #[test]
    fn compacting_signal_reads_as_in_progress() {
        let v = signal_view(&RunSignal::Compacting);
        assert_eq!(
            (v.state, v.operation.as_str()),
            (WorkState::Working, "Compacting context…")
        );
    }

    #[test]
    fn compaction_signal_keeps_working_state() {
        let v = signal_view(&RunSignal::Compacted { dropped: 12 });
        assert_eq!(v.state, WorkState::Working);
        assert_eq!(v.operation, "Context compacted · 12 messages folded");
        assert_eq!(
            signal_view(&RunSignal::Compacted { dropped: 0 }).operation,
            "Context compacted"
        );
    }

    #[test]
    fn verification_and_check_signals_use_real_results_only() {
        let ok = signal_view(&RunSignal::Verification {
            passed: true,
            tool: "cargo test".into(),
            issues: 0,
        });
        assert_eq!(
            (ok.state, ok.operation.as_str()),
            (WorkState::Working, "Verification passed · cargo test")
        );
        let bad = signal_view(&RunSignal::Verification {
            passed: false,
            tool: "tsc".into(),
            issues: 3,
        });
        assert_eq!(
            (bad.state, bad.operation.as_str()),
            (WorkState::Retry, "Verification failed · tsc · 3 issues")
        );
        let chk = signal_view(&RunSignal::Check {
            passed: false,
            jury_skipped: true,
        });
        assert_eq!(chk.operation, "Check failed · jury skipped");
    }

    #[test]
    fn queued_count_shows_only_when_there_is_room() {
        let mut s = jury();
        s.stages = None;
        s.queued = 2;
        assert!(text_of(&working_status_line(&s, 100, &theme())).contains("2 queued"));
    }

    #[test]
    fn each_state_names_itself_in_words_not_only_colour() {
        for (kind, want) in [
            (StatusKind::Working, "Working"),
            (StatusKind::Retrying(2), "Retrying (attempt 2)"),
            (StatusKind::AwaitingApproval, "Awaiting approval"),
            (StatusKind::Idle, "Idle"),
        ] {
            let line = status_line(kind, None, 60, &theme());
            assert!(text_of(&line).contains(want), "{kind:?}: {line:?}");
        }
    }

    #[test]
    fn the_detail_is_sanitized_and_clipped_first() {
        let line = status_line(
            StatusKind::Working,
            Some("reading \u{1b}[2Jevil.rs"),
            20,
            &theme(),
        );
        let text = text_of(&line);
        assert!(!text.contains('\u{1b}'));
        assert!(display_width(&text) <= 20);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn an_empty_or_control_only_detail_is_dropped() {
        let line = status_line(StatusKind::Idle, Some("\u{7}"), 40, &theme());
        let text = text_of(&line);
        assert!(!text.contains('—'));
    }
}
