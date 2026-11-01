//! Autonomy ladder per (content type, platform) (CutRight #116).
//!
//! Every pairing starts at [`Level::Reviewed`]. Each final is recorded with
//! [`Ladder::record`]:
//!
//! - A rejected final drops to `Reviewed` and resets the streak, at any level.
//! - An approved final with no failed critic check extends the streak. At `Reviewed`,
//!   reaching `approvals_to_review_light` promotes to `ReviewLight`. At `ReviewLight`,
//!   `approvals_to_autonomous` more approvals (counted from that promotion) promote to
//!   `Autonomous`.
//! - A failed critic check resets the streak. On an `Autonomous` final it also drops
//!   one level, to `ReviewLight`.
//!
//! The thresholds live in [`LadderPolicy`]. It defaults to 5 and 10, deserialises from
//! the CutRight config with those defaults for omitted fields, and is validated on load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Autonomy level of one (content type, platform) pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Every final is approved by a reviewer.
    Reviewed,
    /// Approval is needed only for publish and spend-commit classes.
    ReviewLight,
    /// No per-item approval within budget and gate pass.
    Autonomous,
}

/// Promotion thresholds. Overridable from config; omitted fields keep their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LadderPolicy {
    /// Clean approved finals at `Reviewed` before promotion to `ReviewLight`.
    pub approvals_to_review_light: u32,
    /// Clean approved finals at `ReviewLight` before promotion to `Autonomous`.
    pub approvals_to_autonomous: u32,
}

impl Default for LadderPolicy {
    fn default() -> Self {
        Self {
            approvals_to_review_light: 5,
            approvals_to_autonomous: 10,
        }
    }
}

impl LadderPolicy {
    /// Parses a policy from JSON, applying defaults to omitted fields, then validates it.
    pub fn from_json(json: &str) -> Result<Self, LadderError> {
        let policy: Self = serde_json::from_str(json)?;
        validate(&policy)?;
        Ok(policy)
    }
}

/// Validation and serialisation failures for the ladder.
#[derive(Debug, thiserror::Error)]
pub enum LadderError {
    #[error("ladder key needs a non-empty content type and platform")]
    EmptyKey,
    #[error("ladder policy: {0} must be at least 1")]
    InvalidPolicy(&'static str),
    #[error("ladder pairing {0} is listed twice")]
    DuplicateKey(String),
    #[error("ladder JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// A (content type, platform) pairing. Both parts are trimmed and must be non-empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LadderKey {
    content_type: String,
    platform: String,
}

impl LadderKey {
    pub fn new(content_type: &str, platform: &str) -> Result<Self, LadderError> {
        let content_type = content_type.trim();
        let platform = platform.trim();
        if content_type.is_empty() || platform.is_empty() {
            return Err(LadderError::EmptyKey);
        }
        Ok(Self {
            content_type: content_type.to_owned(),
            platform: platform.to_owned(),
        })
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }
}

/// Level and the count of clean approvals since the last promotion, demotion or reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderState {
    pub level: Level,
    pub streak: u32,
}

impl Default for LadderState {
    fn default() -> Self {
        Self {
            level: Level::Reviewed,
            streak: 0,
        }
    }
}

/// The outcome of one final, as the ladder sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Final {
    /// Approved (by a reviewer, or implicitly at `Autonomous`). `critic_failed` marks a
    /// failed critic check on this final.
    Approved { critic_failed: bool },
    /// Rejected by a reviewer.
    Rejected,
}

/// Autonomy ladder state for every pairing that has recorded a final.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Snapshot", into = "Snapshot")]
pub struct Ladder {
    policy: LadderPolicy,
    states: BTreeMap<LadderKey, LadderState>,
}

impl Ladder {
    pub fn new(policy: LadderPolicy) -> Result<Self, LadderError> {
        validate(&policy)?;
        Ok(Self {
            policy,
            states: BTreeMap::new(),
        })
    }

    pub fn policy(&self) -> LadderPolicy {
        self.policy
    }

    /// State of a pairing; a pairing with no recorded finals is at its starting state.
    pub fn state(&self, key: &LadderKey) -> LadderState {
        self.states.get(key).copied().unwrap_or_default()
    }

    pub fn level(&self, key: &LadderKey) -> Level {
        self.state(key).level
    }

    /// Overrides a pairing's level and starts a fresh approval streak.
    pub fn set_level(&mut self, key: &LadderKey, level: Level) -> LadderState {
        let state = LadderState { level, streak: 0 };
        self.states.insert(key.clone(), state);
        state
    }

    /// Applies current app thresholds without discarding recorded pairing state.
    pub fn set_policy(&mut self, policy: LadderPolicy) -> Result<(), LadderError> {
        validate(&policy)?;
        self.policy = policy;
        Ok(())
    }

    /// Applies one final to a pairing and returns its new state.
    pub fn record(&mut self, key: &LadderKey, result: Final) -> LadderState {
        let next = apply(&self.policy, self.state(key), result);
        self.states.insert(key.clone(), next);
        next
    }

    /// Serialises the policy and every touched pairing, in key order.
    pub fn to_json(&self) -> Result<String, LadderError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Restores a ladder written by [`Ladder::to_json`]. Refuses an invalid policy, an
    /// empty pairing part or a pairing listed twice.
    pub fn from_json(json: &str) -> Result<Self, LadderError> {
        let snapshot: Snapshot = serde_json::from_str(json)?;
        Self::try_from(snapshot)
    }
}

impl From<Ladder> for Snapshot {
    fn from(ladder: Ladder) -> Self {
        Self {
            policy: ladder.policy,
            pairings: ladder
                .states
                .into_iter()
                .map(|(key, state)| Pairing {
                    content_type: key.content_type,
                    platform: key.platform,
                    level: state.level,
                    streak: state.streak,
                })
                .collect(),
        }
    }
}

impl TryFrom<Snapshot> for Ladder {
    type Error = LadderError;

    fn try_from(snapshot: Snapshot) -> Result<Self, Self::Error> {
        validate(&snapshot.policy)?;
        let mut states = BTreeMap::new();
        for pairing in snapshot.pairings {
            let key = LadderKey::new(&pairing.content_type, &pairing.platform)?;
            let state = LadderState {
                level: pairing.level,
                streak: pairing.streak,
            };
            if states.insert(key.clone(), state).is_some() {
                return Err(LadderError::DuplicateKey(format!(
                    "{}/{}",
                    key.content_type, key.platform
                )));
            }
        }
        Ok(Self {
            policy: snapshot.policy,
            states,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    policy: LadderPolicy,
    pairings: Vec<Pairing>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pairing {
    content_type: String,
    platform: String,
    level: Level,
    streak: u32,
}

fn validate(policy: &LadderPolicy) -> Result<(), LadderError> {
    if policy.approvals_to_review_light == 0 {
        return Err(LadderError::InvalidPolicy("approvals_to_review_light"));
    }
    if policy.approvals_to_autonomous == 0 {
        return Err(LadderError::InvalidPolicy("approvals_to_autonomous"));
    }
    Ok(())
}

fn apply(policy: &LadderPolicy, state: LadderState, result: Final) -> LadderState {
    match result {
        Final::Rejected => LadderState {
            level: Level::Reviewed,
            streak: 0,
        },
        Final::Approved {
            critic_failed: true,
        } => LadderState {
            level: match state.level {
                Level::Autonomous => Level::ReviewLight,
                other => other,
            },
            streak: 0,
        },
        Final::Approved {
            critic_failed: false,
        } => {
            let streak = state.streak.saturating_add(1);
            match state.level {
                Level::Reviewed if streak >= policy.approvals_to_review_light => LadderState {
                    level: Level::ReviewLight,
                    streak: 0,
                },
                Level::ReviewLight if streak >= policy.approvals_to_autonomous => LadderState {
                    level: Level::Autonomous,
                    streak: 0,
                },
                Level::Autonomous => LadderState {
                    level: Level::Autonomous,
                    streak: 0,
                },
                level => LadderState { level, streak },
            }
        }
    }
}
