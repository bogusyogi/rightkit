//! S:I6 — persistent project/model/actor identity strip + model-boundary
//! divider.
//!
//! docs/canon/daemon-master-canon-atoms.md requires a persistent strip showing which project, model, and
//! actor produced the current turn, plus a divider when that attribution
//! changes mid-session (a route decision swapping model or actor). This
//! reads its attribution from [`crate::transcript::timeline::Timeline`]
//! (S:I3) rather than owning its own state: the strip is a projection of
//! the timeline's most recent [`crate::transcript::timeline::TimelineEntry::Route`],
//! never a second source of truth for it.
//!
//! Truncation priority, per the reconciled spec's "Preservation" column for
//! I6: when the strip does not fit the available width, drop the least
//! identifying field first. Actor is the most contextually useful (matches
//! Legion's own authority-role framing), so the priority is the pending
//! next-turn marker, then project, then model — keeping actor as long as
//! anything is shown at all.

use crate::component::RenderFrameExt as _;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::component::{Component, Event, Outcome, Role, Theme};
use crate::render::sanitize;
use crate::transcript::timeline::Timeline;
use crate::transcript::wrap::display_width;

/// The identity strip's own snapshot of the fields it draws, refreshed each
/// render from the timeline's latest route decision. Kept as a distinct
/// struct (not just three `&str`s) so a boundary change is a comparison of
/// two owned values, independent of the timeline's own lifetime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attribution {
    pub project: String,
    pub model: String,
    pub actor: String,
}

impl Attribution {
    fn from_timeline(timeline: &Timeline) -> Option<Self> {
        // Project/model/actor names arrive over the daemon feed; sanitize
        // here so a hostile or malformed route record cannot emit control
        // bytes through the persistent chrome. Pi's `footer.ts` applies the
        // same rule to its status segments.
        timeline.latest_route().map(|(project, model, actor)| Self {
            project: sanitize(project),
            model: sanitize(model),
            actor: sanitize(actor),
        })
    }
}

/// The identity strip component. One row: `project · actor/model`, with a
/// boundary marker rendered on the render *following* an attribution change
/// (so the divider appears once, at the moment the boundary is crossed, not
/// on every subsequent frame).
#[derive(Debug, Default)]
pub struct IdentityStrip {
    current: Option<Attribution>,
    /// Effort of the committed route (`model·effort`), host-fed — the strip
    /// displays it but never derives it.
    effort: Option<String>,
    /// A requested-but-not-yet-committed next-turn selection, rendered as
    /// `→ model·effort` and dropped before anything else under truncation.
    /// Display-only: the route authority still owns what it becomes.
    pending: Option<String>,
    /// Set for exactly one render after `current` changes; consumed by
    /// `render` and cleared immediately after so the divider does not
    /// persist.
    boundary_crossed: bool,
}

impl IdentityStrip {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the committed route's effort label (e.g. "high"), or clear it.
    /// Sanitized here — effort names arrive from the model catalog feed.
    pub fn set_effort(&mut self, effort: Option<String>) {
        self.effort = effort
            .map(|e| sanitize(&e))
            .filter(|e| !e.trim().is_empty());
    }

    /// Set the pending next-turn selection shown as `→ <pending>`, or clear
    /// it once the route commits it (committed state arrives via `sync`).
    pub fn set_pending(&mut self, pending: Option<String>) {
        self.pending = pending
            .map(|p| sanitize(&p))
            .filter(|p| !p.trim().is_empty());
    }

    /// Refresh from the timeline. Call once per tick before `render`. Sets
    /// the one-shot boundary flag when attribution actually changed (not on
    /// the very first attribution, which is a start, not a boundary).
    pub fn sync(&mut self, timeline: &Timeline) {
        let latest = Attribution::from_timeline(timeline);
        match (&self.current, &latest) {
            (Some(prev), Some(next)) if prev != next => {
                self.boundary_crossed = true;
            }
            (_, None) => {
                self.boundary_crossed = false;
            }
            _ => {}
        }
        // This is a projection, never an independent attribution cache: a
        // cleared timeline must clear its strip on the same frame.
        self.current = latest;
    }

    pub fn attribution(&self) -> Option<&Attribution> {
        self.current.as_ref()
    }

    pub fn boundary_crossed(&self) -> bool {
        self.boundary_crossed
    }

    /// Render the strip text at `width`, applying the truncation priority
    /// (drop project, then model, keep actor) documented on this module.
    /// Width is measured in display columns (wide graphemes count double —
    /// the pi `footer.ts` width-safe rule), and a truncated remainder ends
    /// in `…` rather than a bare cut. Exposed standalone (not just via
    /// `Component::render`) so tests can assert the exact string without a
    /// `Frame`.
    pub fn line_at_width(&self, width: u16) -> String {
        let Some(attr) = &self.current else {
            return String::new();
        };
        let width = width as usize;

        let model = match &self.effort {
            Some(effort) => format!("{}·{}", attr.model, effort),
            None => attr.model.clone(),
        };
        let pending = self
            .pending
            .as_ref()
            .map(|p| format!(" → {p}"))
            .unwrap_or_default();

        // Truncation priority (least identifying first): pending marker,
        // then project, then model — actor stays as long as anything does.
        let full = format!("{} · {}/{model}{pending}", attr.project, attr.actor);
        if display_width(&full) <= width {
            return full;
        }

        let no_pending = format!("{} · {}/{model}", attr.project, attr.actor);
        if display_width(&no_pending) <= width {
            return no_pending;
        }

        let without_project = format!("{}/{model}", attr.actor);
        if display_width(&without_project) <= width {
            return without_project;
        }

        if display_width(&attr.actor) <= width {
            return attr.actor.clone();
        }

        // Last resort: hard-clip the actor by columns with an ellipsis, the
        // pi footer clip rule — a remaining tail of width 1 keeps the
        // marker so truncation is still legible.
        let budget = width.saturating_sub(1);
        let mut out = String::new();
        let mut used = 0usize;
        for g in unicode_segmentation::UnicodeSegmentation::graphemes(attr.actor.as_str(), true) {
            let w = display_width(g);
            if used + w > budget {
                break;
            }
            out.push_str(g);
            used += w;
        }
        out.push('…');
        out
    }
}

impl Component for IdentityStrip {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        let text = self.line_at_width(area.width);
        let style = Style::default()
            .fg(theme.color(Role::Dim))
            .add_modifier(Modifier::BOLD);
        frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
        // One-shot: the divider (drawn by the host as a rule above/below
        // this strip when `boundary_crossed()` is true) is consumed here so
        // it fires exactly once per boundary.
        self.boundary_crossed = false;
    }

    fn handle(&mut self, _event: &Event) -> Outcome {
        Outcome::Ignored
    }

    fn desired_height(&self, _width: u16, _available: u16) -> u16 {
        1
    }
}
