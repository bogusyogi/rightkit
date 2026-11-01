//! D3 — Claude-Code style rewind: `Esc Esc` on an empty idle composer (or
//! `/rewind`) lists this session's prior prompts; choosing one offers
//! "conversation and files", "conversation only" or "files only". The chosen
//! prompt lands back in the composer to edit and resend. The TUI only renders
//! and emits typed intents; the host authority performs the restoration
//! (conversation truncation + EFF-008-guarded file undo).

use rightkit_terminal_ui::component::Component;
use rightkit_terminal_ui::nav::{ListPicker, PickerAction, PickerParams, SelectionItem};

use super::{Composer, FrameRequester, SessionState, Transcript, TuiEvent, TurnHost};
use crate::api_sessions::rewind::{RewindMode, RewindPoint};

const VIEW_MESSAGES: u32 = 61;
const VIEW_ACTION: u32 = 62;

/// The open rewind picker: first the prompt list, then the restore choice.
pub(super) struct RewindFlow {
    points: Vec<RewindPoint>,
    picker: ListPicker,
    chosen: Option<usize>,
}

impl RewindFlow {
    pub(super) fn picker_mut(&mut self) -> &mut ListPicker {
        &mut self.picker
    }
}

fn excerpt(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

fn messages_picker(points: &[RewindPoint]) -> ListPicker {
    let items = points
        .iter()
        .map(|point| SelectionItem::new(excerpt(&point.text, 90)))
        .collect();
    let mut params = PickerParams::picker(VIEW_MESSAGES, "Rewind", items);
    params.subtitle = Some("Go back to just before one of your messages".to_string());
    params.initial_selected_idx = points.len().checked_sub(1);
    params.footer_hint = Some("↑/↓ select · Enter choose · Esc cancel".to_string());
    ListPicker::new(params)
}

fn action_picker(point: &RewindPoint) -> ListPicker {
    let items = vec![
        SelectionItem::new("Restore conversation and files"),
        SelectionItem::new("Restore conversation only"),
        SelectionItem::new("Restore files only"),
    ];
    let mut params = PickerParams::picker(
        VIEW_ACTION,
        format!("Rewind to: {}", excerpt(&point.text, 70)),
        items,
    );
    params.is_searchable = false;
    params.footer_hint = Some("↑/↓ select · 1-3 pick · Enter confirm · Esc cancel".to_string());
    ListPicker::new(params)
}

/// Open the prompt picker (or say why there is nothing to rewind to).
pub(super) async fn open(
    host: &impl TurnHost,
    session: &mut SessionState,
    transcript: &mut Transcript,
    requester: &FrameRequester,
) {
    match host.rewind_points().await {
        Ok(points) if points.is_empty() => {
            session.notice(
                transcript,
                "Nothing to rewind to yet: no earlier messages in this session.",
            );
        }
        Ok(points) => {
            let picker = messages_picker(&points);
            session.rewind = Some(RewindFlow {
                points,
                picker,
                chosen: None,
            });
        }
        Err(error) => session.notice(transcript, error),
    }
    requester.schedule_immediate();
}

/// Feed one key to the open picker; performs the rewind once confirmed.
pub(super) async fn handle_key(
    host: &impl TurnHost,
    event: &TuiEvent,
    composer: &mut Composer,
    session: &mut SessionState,
    transcript: &mut Transcript,
    requester: &FrameRequester,
) {
    let Some(mut flow) = session.rewind.take() else {
        return;
    };
    let _ = flow.picker.handle(event);
    let actions = flow.picker.drain_actions();
    let mut keep = !flow.picker.is_done();
    let mut run: Option<(RewindPoint, RewindMode)> = None;
    for action in actions {
        match action {
            PickerAction::Accepted { view_id, index } => {
                let Some(position) = flow.picker.filtered_idx(index) else {
                    continue;
                };
                if view_id == VIEW_MESSAGES {
                    if let Some(point) = flow.points.get(position).cloned() {
                        flow.chosen = Some(position);
                        flow.picker = action_picker(&point);
                        keep = true;
                    }
                } else if let Some(point) = flow.chosen.and_then(|c| flow.points.get(c)).cloned() {
                    let mode = match position {
                        0 => RewindMode::Both,
                        1 => RewindMode::Conversation,
                        _ => RewindMode::Files,
                    };
                    run = Some((point, mode));
                    keep = false;
                }
            }
            PickerAction::Cancelled { .. } => keep = false,
            PickerAction::Toggled { .. } => {}
        }
    }
    if keep {
        session.rewind = Some(flow);
    }
    if let Some((point, mode)) = run {
        perform(host, &point, mode, composer, session, transcript).await;
    }
    requester.schedule_immediate();
}

async fn perform(
    host: &impl TurnHost,
    point: &RewindPoint,
    mode: RewindMode,
    composer: &mut Composer,
    session: &mut SessionState,
    transcript: &mut Transcript,
) {
    match host.rewind_to(point, mode).await {
        Ok(report) => {
            if report.conversation_rewound {
                composer.set_text(&report.text);
                session.notice(
                    transcript,
                    format!(
                        "Rewound conversation to before: {}\nThe prompt is back in the composer; edit it and press Enter to resend.",
                        excerpt(&report.text, 80)
                    ),
                );
            }
            if matches!(mode, RewindMode::Files | RewindMode::Both) {
                if report.files_restored.is_empty() && report.file_conflicts.is_empty() {
                    session.notice(transcript, "No file changes to restore after that message.");
                }
                if !report.files_restored.is_empty() {
                    session.notice(
                        transcript,
                        format!(
                            "Restored {} file(s): {}",
                            report.files_restored.len(),
                            report.files_restored.join(", ")
                        ),
                    );
                }
                for (path, reason) in &report.file_conflicts {
                    session.notice(
                        transcript,
                        format!(
                            "Not restored (changed since the agent wrote it): {path} — {reason}"
                        ),
                    );
                }
            }
        }
        Err(error) => session.notice(transcript, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(text: &str, nth: usize) -> RewindPoint {
        RewindPoint {
            text: text.to_string(),
            nth_from_end: nth,
            created_at: String::new(),
        }
    }

    #[test]
    fn excerpt_takes_first_nonblank_line_and_truncates() {
        assert_eq!(excerpt("\n\n  hello world \nsecond", 90), "hello world");
        assert_eq!(excerpt("", 10), "");
        let cut = excerpt("abcdefghij", 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn message_picker_lists_every_prompt_and_starts_on_the_newest() {
        let points = vec![point("first", 1), point("second", 0)];
        let picker = messages_picker(&points);
        assert_eq!(picker.items().len(), 2);
        assert_eq!(picker.selected(), Some(1));
    }

    #[test]
    fn action_picker_offers_both_conversation_and_files_choices() {
        let picker = action_picker(&point("fix the bug", 0));
        assert_eq!(picker.items().len(), 3);
    }

    #[test]
    fn mode_mapping_matches_picker_row_order() {
        // Rows: 0 = both, 1 = conversation only, 2 = files only (see handle_key).
        let modes: Vec<RewindMode> = (0..3)
            .map(|p| match p {
                0 => RewindMode::Both,
                1 => RewindMode::Conversation,
                _ => RewindMode::Files,
            })
            .collect();
        assert_eq!(
            modes,
            [
                RewindMode::Both,
                RewindMode::Conversation,
                RewindMode::Files
            ]
        );
    }
}
