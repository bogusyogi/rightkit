//! Full-screen transcript and unified-diff readers.
//!
//! The host supplies a read-only snapshot; this component owns paging,
//! searching, highlighting, and close/navigation intents.

use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::component::{Component, Event, Outcome, RenderFrameExt, Role, Theme};
use crate::render::sanitize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectionKind {
    Transcript,
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectionIntent {
    Close,
}

/// Searchable, scrollable full-screen reader used for transcript history and
/// actual workspace diffs.
#[derive(Debug, Clone)]
pub struct InspectionViewer {
    kind: InspectionKind,
    lines: Vec<String>,
    offset: usize,
    search: String,
    searching: bool,
    matches: Vec<usize>,
    selected_match: usize,
    intents: Vec<InspectionIntent>,
}

impl InspectionViewer {
    pub fn transcript(lines: impl IntoIterator<Item = String>) -> Self {
        Self::new(InspectionKind::Transcript, lines)
    }

    pub fn diff(lines: impl IntoIterator<Item = String>) -> Self {
        Self::new(InspectionKind::Diff, lines)
    }

    fn new(kind: InspectionKind, lines: impl IntoIterator<Item = String>) -> Self {
        Self {
            kind,
            lines: lines.into_iter().map(|line| sanitize(&line)).collect(),
            offset: 0,
            search: String::new(),
            searching: false,
            matches: Vec::new(),
            selected_match: 0,
            intents: Vec::new(),
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn drain_intents(&mut self) -> Vec<InspectionIntent> {
        std::mem::take(&mut self.intents)
    }

    fn update_matches(&mut self) {
        let needle = self.search.to_lowercase();
        self.matches = if needle.is_empty() {
            Vec::new()
        } else {
            self.lines
                .iter()
                .enumerate()
                .filter_map(|(index, line)| line.to_lowercase().contains(&needle).then_some(index))
                .collect()
        };
        self.selected_match = 0;
        if let Some(first) = self.matches.first() {
            self.offset = *first;
        }
    }

    fn next_match(&mut self, reverse: bool) {
        if self.matches.is_empty() {
            return;
        }
        if reverse {
            self.selected_match = self
                .selected_match
                .checked_sub(1)
                .unwrap_or(self.matches.len() - 1);
        } else {
            self.selected_match = (self.selected_match + 1) % self.matches.len();
        }
        self.offset = self.matches[self.selected_match];
    }
}

impl Component for InspectionViewer {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        let title = match self.kind {
            InspectionKind::Transcript => "Transcript",
            InspectionKind::Diff => "Workspace diff",
        };
        let header = if self.searching {
            format!("{title} · search: {}", self.search)
        } else if !self.search.is_empty() {
            let position = if self.matches.is_empty() {
                "no matches".to_string()
            } else {
                format!("{}/{} matches", self.selected_match + 1, self.matches.len())
            };
            format!("{title} · /{} · {position}", self.search)
        } else {
            title.to_string()
        };
        let footer = if self.searching {
            "type to search · Enter finish · Esc clear"
        } else {
            "↑↓/PgUp/PgDn scroll · / search · n/N next/previous · Esc/q close"
        };
        let body_height = area.height.saturating_sub(2) as usize;
        let text = self
            .lines
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(body_height)
            .map(|(index, line)| {
                let focused = self.matches.get(self.selected_match).copied() == Some(index)
                    && !self.searching;
                let matching = self.matches.binary_search(&index).is_ok();
                let style = if focused {
                    Style::default()
                        .fg(theme.color(Role::Attention))
                        .add_modifier(Modifier::BOLD)
                } else if matching {
                    Style::default()
                        .fg(theme.color(Role::Success))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.color(Role::Text))
                };
                Line::from(Span::styled(line.clone(), style))
            })
            .collect::<Vec<_>>();
        let mut rows = vec![Line::from(Span::styled(
            header,
            Style::default()
                .fg(theme.color(Role::Running))
                .add_modifier(Modifier::BOLD),
        ))];
        rows.extend(text);
        rows.push(Line::from(Span::styled(
            footer,
            Style::default().fg(theme.color(Role::Dim)),
        )));
        frame.render_widget(
            Paragraph::new(Text::from(rows))
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        let Event::Key(key) = event else {
            return Outcome::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return Outcome::Ignored;
        }
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.searching = false;
                    self.update_matches();
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.search.pop();
                    self.update_matches();
                }
                KeyCode::Char(character) => {
                    self.search.push(character);
                    self.update_matches();
                }
                _ => {}
            }
            return Outcome::Redraw;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.intents.push(InspectionIntent::Close),
            KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::Down => {
                self.offset = (self.offset + 1).min(self.lines.len().saturating_sub(1))
            }
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(10),
            KeyCode::PageDown => {
                self.offset = (self.offset + 10).min(self.lines.len().saturating_sub(1))
            }
            KeyCode::Home => self.offset = 0,
            KeyCode::End => self.offset = self.lines.len().saturating_sub(1),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('n') => self.next_match(false),
            KeyCode::Char('N') => self.next_match(true),
            _ => return Outcome::Ignored,
        }
        Outcome::Redraw
    }
}
