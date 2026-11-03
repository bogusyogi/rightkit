//! The jury system — aggregates multiple juror verdicts into a single decision.

/// A single juror's vote.
#[derive(Debug, Clone, PartialEq)]
pub struct JurorVerdict {
    pub juror: String,
    pub approve: bool,
    pub confidence: f64,
    pub reasoning: String,
}

impl JurorVerdict {
    pub fn new(
        juror: impl Into<String>,
        approve: bool,
        confidence: f64,
        reasoning: impl Into<String>,
    ) -> Self {
        Self {
            juror: juror.into(),
            approve,
            confidence,
            reasoning: reasoning.into(),
        }
    }

    fn verdict_label(&self) -> &'static str {
        if self.approve {
            "APPROVE"
        } else {
            "REJECT"
        }
    }
}

/// The aggregated decision of a jury.
#[derive(Debug, Clone, PartialEq)]
pub enum JuryDecision {
    Approved { confidence: f64 },
    Rejected { confidence: f64 },
    NoConsensus,
}

/// Aggregates juror verdicts requiring a minimum quorum.
#[derive(Debug, Clone)]
pub struct JurySystem {
    quorum: usize,
}

impl JurySystem {
    pub fn new(quorum: usize) -> Self {
        Self { quorum }
    }

    /// Decide based on majority. Requires at least `quorum` verdicts.
    ///
    /// - Below quorum -> [`JuryDecision::NoConsensus`].
    /// - Unanimous approve/reject -> immediate decision.
    /// - Majority approve -> Approved with average confidence of approvers.
    /// - Majority reject -> Rejected with average confidence of rejecters.
    /// - Tie -> NoConsensus.
    pub fn decide(&self, verdicts: &[JurorVerdict]) -> JuryDecision {
        if !self.has_quorum(verdicts) {
            return JuryDecision::NoConsensus;
        }

        if self.is_unanimous(verdicts) {
            let unanimous: Vec<&JurorVerdict> = verdicts.iter().collect();
            return if verdicts[0].approve {
                JuryDecision::Approved {
                    confidence: average_confidence(&unanimous),
                }
            } else {
                JuryDecision::Rejected {
                    confidence: average_confidence(&unanimous),
                }
            };
        }

        let approvers: Vec<&JurorVerdict> = verdicts.iter().filter(|v| v.approve).collect();
        let rejecters: Vec<&JurorVerdict> = verdicts.iter().filter(|v| !v.approve).collect();

        if approvers.len() > rejecters.len() {
            JuryDecision::Approved {
                confidence: average_confidence(&approvers),
            }
        } else if rejecters.len() > approvers.len() {
            JuryDecision::Rejected {
                confidence: average_confidence(&rejecters),
            }
        } else {
            JuryDecision::NoConsensus
        }
    }

    /// A split first ballot is any quorum-satisfying, non-unanimous ballot.
    pub fn should_deliberate(&self, verdicts: &[JurorVerdict]) -> bool {
        self.has_quorum(verdicts) && !self.is_unanimous(verdicts)
    }

    /// Build the peer broadcast for a second ballot.
    pub fn deliberation_brief(&self, verdicts: &[JurorVerdict]) -> Option<String> {
        if !self.should_deliberate(verdicts) {
            return None;
        }

        let mut brief = String::from("First ballot peer verdicts:\n");
        for verdict in verdicts {
            brief.push_str(&format!(
                "- {}: {}, confidence {:.2}\n  reasoning: {}\n",
                verdict.juror,
                verdict.verdict_label(),
                verdict.confidence,
                compact_reasoning(&verdict.reasoning)
            ));
        }
        Some(brief)
    }

    pub fn is_unanimous(&self, verdicts: &[JurorVerdict]) -> bool {
        self.has_quorum(verdicts) && verdicts.iter().all(|v| v.approve == verdicts[0].approve)
    }

    fn has_quorum(&self, verdicts: &[JurorVerdict]) -> bool {
        verdicts.len() >= self.quorum && !verdicts.is_empty()
    }
}

fn average_confidence(verdicts: &[&JurorVerdict]) -> f64 {
    if verdicts.is_empty() {
        return 0.0;
    }
    let sum: f64 = verdicts.iter().map(|v| v.confidence).sum();
    sum / verdicts.len() as f64
}

fn compact_reasoning(reasoning: &str) -> String {
    let trimmed = reasoning.trim();
    if trimmed.is_empty() {
        "(no reasoning supplied)".into()
    } else {
        trimmed.replace('\n', " ")
    }
}
