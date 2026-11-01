//! Token budget for the LLM ranking prompt.
//!
//! Token counts are an estimate (one token per four characters, rounded up), taken on
//! the exact text of the prompt: the fixed system and header text plus each sentence
//! line. No tokenizer runs here, so the estimate is deterministic and portable, and it
//! errs on the side of counting more tokens than the provider will.
//!
//! [`plan_requests`] packs sentences in order into requests that each stay within the
//! budget. A request cut short by the budget is reported as truncated: the sentences it
//! leaves out are sent in the following request, so none are lost. A sentence that
//! cannot fit even alone is refused and named in the report. If the fixed prompt alone is
//! over budget, nothing is sent and the report says so.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Characters per estimated token.
pub const CHARS_PER_TOKEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    /// Every sentence went out in a request that fit the budget.
    Within,
    /// Requests were cut at the budget, or sentences were refused; the rest were sent.
    Truncated,
    /// Nothing could be sent: the fixed prompt, or every sentence, exceeds the budget.
    Refused,
}

/// What the budget did to one ranking run. Stored on the proposal set so a reader can see
/// whether the model saw the whole transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetReport {
    pub action: BudgetAction,
    pub budget_tokens: u32,
    pub estimated_fixed_tokens: u32,
    pub requests: usize,
    pub max_request_tokens: u32,
    pub sentences_offered: usize,
    pub sentences_sent: usize,
    /// Requests that stopped early because the next sentence would exceed the budget.
    pub requests_cut_by_budget: usize,
    /// Indices (into the offered sentences) of sentences too large to send even alone.
    pub refused_sentences: Vec<usize>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPlan {
    /// Consecutive ranges into the offered sentences, one per request, in order.
    pub requests: Vec<Range<usize>>,
    pub report: BudgetReport,
}

/// Estimated tokens for `chars` characters of prompt text.
pub fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(CHARS_PER_TOKEN)
}

fn to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Packs sentences into requests under `budget_tokens`.
///
/// - `line_chars[i]`: characters of sentence `i`'s prompt line.
/// - `fixed_chars`: characters of the prompt that is sent whatever the sentences are.
/// - `max_per_request`: the sentence-count cap per request (at least one is used).
pub fn plan_requests(
    line_chars: &[usize],
    fixed_chars: usize,
    budget_tokens: u32,
    max_per_request: usize,
) -> RequestPlan {
    let budget = budget_tokens as usize;
    let per = max_per_request.max(1);
    let n = line_chars.len();
    let fixed_tokens = estimate_tokens(fixed_chars);
    let mut report = BudgetReport {
        action: BudgetAction::Within,
        budget_tokens,
        estimated_fixed_tokens: to_u32(fixed_tokens),
        requests: 0,
        max_request_tokens: 0,
        sentences_offered: n,
        sentences_sent: 0,
        requests_cut_by_budget: 0,
        refused_sentences: Vec::new(),
        note: String::new(),
    };
    if fixed_tokens > budget {
        report.action = BudgetAction::Refused;
        report.note = format!(
            "the fixed prompt is about {fixed_tokens} tokens, over the {budget_tokens}-token \
             budget; nothing was sent"
        );
        return RequestPlan {
            requests: Vec::new(),
            report,
        };
    }

    let mut requests = Vec::new();
    let mut i = 0;
    while i < n {
        let start = i;
        let mut chars = fixed_chars;
        while i < n && i - start < per {
            let next = chars + line_chars[i];
            if estimate_tokens(next) > budget {
                break;
            }
            chars = next;
            i += 1;
        }
        if i == start {
            // Sentence `i` alone does not fit: refuse it and move on.
            report.refused_sentences.push(i);
            i += 1;
            continue;
        }
        if i < n && i - start < per {
            report.requests_cut_by_budget += 1;
        }
        report.max_request_tokens = report
            .max_request_tokens
            .max(to_u32(estimate_tokens(chars)));
        requests.push(start..i);
    }

    report.requests = requests.len();
    report.sentences_sent = requests.iter().map(|r: &Range<usize>| r.len()).sum();
    report.action = if n > 0 && report.sentences_sent == 0 {
        BudgetAction::Refused
    } else if report.requests_cut_by_budget > 0 || !report.refused_sentences.is_empty() {
        BudgetAction::Truncated
    } else {
        BudgetAction::Within
    };
    report.note = match report.action {
        BudgetAction::Within => format!(
            "{} sentence(s) in {} request(s), each within the {budget_tokens}-token budget",
            report.sentences_sent, report.requests
        ),
        BudgetAction::Truncated => format!(
            "{} of {} sentence(s) sent in {} request(s); {} request(s) cut at the \
             {budget_tokens}-token budget; {} sentence(s) too large to send alone were refused",
            report.sentences_sent,
            n,
            report.requests,
            report.requests_cut_by_budget,
            report.refused_sentences.len()
        ),
        BudgetAction::Refused => format!(
            "no sentence fits the {budget_tokens}-token budget alongside the fixed prompt; \
             nothing was sent"
        ),
    };
    RequestPlan { requests, report }
}
