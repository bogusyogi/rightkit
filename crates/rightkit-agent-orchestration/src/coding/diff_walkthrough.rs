//! Bounded plain-language narration for the hunks in a unified diff.
//!
//! This module deliberately accepts a [`SmallModelClient`] from its caller. It
//! does not construct a provider or make a network request; an unavailable
//! route is retained as the typed [`SmallModelError`] on the affected hunk,
//! rather than being presented as a successful narration.

use crate::coding::small_model_utility::{
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MULTI_HUNK_DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n fn first() {\n-    old_first();\n+    new_first();\n }\n@@ -20,3 +21,4 @@\n fn second() {\n-    old_second();\n+    new_second();\n }\n";

    #[test]
    fn split_unified_diff_keeps_real_multi_hunk_boundaries() {
        let hunks = split_unified_diff(MULTI_HUNK_DIFF);

        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].header, "@@ -1,3 +1,4 @@");
        assert!(hunks[0].body.contains("-    old_first();"));
        assert!(hunks[0].body.contains("+    new_first();"));
        assert_eq!(hunks[1].header, "@@ -20,3 +21,4 @@");
        assert!(hunks[1].body.contains("-    old_second();"));
        assert!(hunks[1].body.contains("+    new_second();"));
    }

    #[test]
    fn split_unified_diff_does_not_merge_adjacent_files() {
        let diff = [
            MULTI_HUNK_DIFF,
            "diff --git a/src/other.rs b/src/other.rs\n",
            "index 3333333..4444444 100644\n",
            "--- a/src/other.rs\n",
            "+++ b/src/other.rs\n",
            "@@ -4,1 +4,1 @@\n",
            "-old\n",
            "+new\n",
        ]
        .concat();

        let hunks = split_unified_diff(&diff);

        assert_eq!(hunks.len(), 3);
        assert!(!hunks[1].body.contains("diff --git"));
        assert_eq!(hunks[2].header, "@@ -4,1 +4,1 @@");
        assert_eq!(hunks[2].body, "-old\n+new");
    }

    struct StubClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl SmallModelClient for StubClient {
        async fn run(
            &self,
            task: SmallModelTask,
            input: &SmallModelInput,
            budget: &SmallModelBudget,
        ) -> Result<SmallModelOutput, SmallModelError> {
            assert_eq!(task, SmallModelTask::DiffNarration);
            assert!(budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE));
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            assert!(input.content.starts_with("@@"));
            Ok(SmallModelOutput::DiffNarration {
                narration: format!("narration {call}"),
            })
        }
    }

    #[tokio::test]
    async fn budget_exhaustion_marks_the_remainder_without_calling_the_client() {
        let client = StubClient {
            calls: AtomicUsize::new(0),
        };
        let mut budget = SmallModelBudget::new(1, DEFAULT_CALL_TOKEN_ESTIMATE);

        let walkthrough = narrate_diff_walkthrough(&client, MULTI_HUNK_DIFF, &mut budget).await;

        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        assert_eq!(walkthrough.hunks.len(), 2);
        assert!(matches!(
            walkthrough.hunks[0].narration,
            DiffHunkNarration::Narrated { .. }
        ));
        assert_eq!(
            walkthrough.hunks[1].narration,
            DiffHunkNarration::Unnarrated {
                reason: DiffHunkUnnarratedReason::BudgetExhausted
            }
        );
        assert_eq!(budget.remaining_calls, 0);
        assert_eq!(budget.remaining_tokens, 0);
    }
}
