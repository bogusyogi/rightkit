//! Bounded plain-language narration for the hunks in a unified diff.
//!
//! This module deliberately accepts a [`SmallModelClient`] from its caller. It
//! does not construct a provider or make a network request; an unavailable
//! route is retained as the typed [`SmallModelError`] on the affected hunk,
//! rather than being presented as a successful narration.

use crate::small_model_utility::{
    SmallModelBudget, SmallModelClient, SmallModelError, SmallModelInput, SmallModelOutput,
    SmallModelTask, DEFAULT_CALL_TOKEN_ESTIMATE, DEFAULT_INPUT_BYTE_CAP,
};

/// One unified-diff hunk, kept as the smallest unit that can receive a truthful
/// narration without combining unrelated changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// The `@@ ... @@` range line identifying this hunk.
    pub header: String,
    /// Lines following [`Self::header`] up to the next hunk or file.
    pub body: String,
}

impl DiffHunk {
    fn model_text(&self) -> String {
        if self.body.is_empty() {
            self.header.clone()
        } else {
            format!("{}\n{}", self.header, self.body)
        }
    }
}

/// Why a hunk has no narration. Keeping model failures typed prevents callers
/// from mistaking an unavailable external route for an empty success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffHunkUnnarratedReason {
    /// The shared budget could not afford another estimated model call.
    BudgetExhausted,
    /// The injected client failed while attempting this hunk.
    ModelError(SmallModelError),
    /// This hunk was not attempted after an earlier model failure.
    SkippedAfterModelError(SmallModelError),
    /// The client returned a task shape other than `DiffNarration`.
    UnexpectedOutput,
    /// The client returned the right task shape but no usable text.
    EmptyNarration,
}

/// The result of attempting to narrate one hunk, including an explicit
/// non-narrated state so the UI cannot imply coverage that did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffHunkNarration {
    /// A model-produced, non-empty narration for the hunk.
    Narrated { narration: String },
    /// No narration is available for this hunk.
    Unnarrated { reason: DiffHunkUnnarratedReason },
}

/// A hunk paired with the honest result of its narration attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffWalkthroughHunk {
    /// The original diff hunk, preserved for rendering beside its narration.
    pub hunk: DiffHunk,
    /// The narration or the reason it was not produced.
    pub narration: DiffHunkNarration,
}

/// All hunk results from one walkthrough, processed in source order under one
/// caller-owned [`SmallModelBudget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffWalkthrough {
    /// Results remain in the same order as the source unified diff.
    pub hunks: Vec<DiffWalkthroughHunk>,
}

/// Split a unified diff at its standard `@@ ... @@` hunk headers.
///
/// Preamble and file metadata are intentionally not fabricated into a hunk:
/// only actual hunk ranges are sent to the model. A malformed or metadata-only
/// diff therefore produces no narratable hunks.
pub fn split_unified_diff(diff: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_body = Vec::new();

    for line in diff.lines() {
        if line.starts_with("@@") {
            finish_current_hunk(&mut hunks, &mut current_header, &mut current_body);
            current_header = Some(line.to_string());
        } else if line.starts_with("diff --git ") || line.starts_with("Index: ") {
            // These markers begin a new file. Do not let the next file's
            // metadata become part of the preceding hunk's model input.
            finish_current_hunk(&mut hunks, &mut current_header, &mut current_body);
        } else if current_header.is_some() {
            current_body.push(line.to_string());
        }
    }

    finish_current_hunk(&mut hunks, &mut current_header, &mut current_body);

    hunks
}

fn finish_current_hunk(
    hunks: &mut Vec<DiffHunk>,
    current_header: &mut Option<String>,
    current_body: &mut Vec<String>,
) {
    if let Some(header) = current_header.take() {
        hunks.push(DiffHunk {
            header,
            body: current_body.join("\n"),
        });
    }
    current_body.clear();
}

/// Narrate each unified-diff hunk in order with `DiffNarration` calls.
///
/// Every call uses the same mutable budget and the shared helper's conservative
/// token estimate. Once that budget is exhausted, no further client call is
/// attempted; the current and remaining hunks are explicitly marked
/// un-narrated. Provider errors likewise stop the walk rather than inventing
/// narration for the remainder.
pub async fn narrate_diff_walkthrough<C>(
    client: &C,
    diff: &str,
    budget: &mut SmallModelBudget,
) -> DiffWalkthrough
where
    C: SmallModelClient + ?Sized,
{
    let hunks = split_unified_diff(diff);
    let mut results = Vec::with_capacity(hunks.len());

    for (index, hunk) in hunks.iter().enumerate() {
        if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
            mark_remaining(
                &mut results,
                &hunks[index..],
                DiffHunkUnnarratedReason::BudgetExhausted,
            );
            break;
        }

        let input = SmallModelInput::bounded(&hunk.model_text(), DEFAULT_INPUT_BYTE_CAP);
        match client
            .run(SmallModelTask::DiffNarration, &input, budget)
            .await
        {
            Ok(SmallModelOutput::DiffNarration { narration }) => {
                // The call did happen, even when its text is empty, so account
                // for it before deciding how to represent the result.
                budget.spend(DEFAULT_CALL_TOKEN_ESTIMATE);
                let narration = narration.trim().to_string();
                let result = if narration.is_empty() {
                    DiffHunkNarration::Unnarrated {
                        reason: DiffHunkUnnarratedReason::EmptyNarration,
                    }
                } else {
                    DiffHunkNarration::Narrated { narration }
                };
                results.push(DiffWalkthroughHunk {
                    hunk: hunk.clone(),
                    narration: result,
                });
            }
            Ok(_) => {
                budget.spend(DEFAULT_CALL_TOKEN_ESTIMATE);
                results.push(DiffWalkthroughHunk {
                    hunk: hunk.clone(),
                    narration: DiffHunkNarration::Unnarrated {
                        reason: DiffHunkUnnarratedReason::UnexpectedOutput,
                    },
                });
            }
            Err(SmallModelError::BudgetExceeded) => {
                mark_remaining(
                    &mut results,
                    &hunks[index..],
                    DiffHunkUnnarratedReason::BudgetExhausted,
                );
                break;
            }
            Err(error) => {
                results.push(DiffWalkthroughHunk {
                    hunk: hunk.clone(),
                    narration: DiffHunkNarration::Unnarrated {
                        reason: DiffHunkUnnarratedReason::ModelError(error.clone()),
                    },
                });
                mark_remaining(
                    &mut results,
                    &hunks[index + 1..],
                    DiffHunkUnnarratedReason::SkippedAfterModelError(error),
                );
                break;
            }
        }
    }

    DiffWalkthrough { hunks: results }
}

fn mark_remaining(
    results: &mut Vec<DiffWalkthroughHunk>,
    remaining: &[DiffHunk],
    reason: DiffHunkUnnarratedReason,
) {
    for hunk in remaining {
        results.push(DiffWalkthroughHunk {
            hunk: hunk.clone(),
            narration: DiffHunkNarration::Unnarrated {
                reason: reason.clone(),
            },
        });
    }
}
