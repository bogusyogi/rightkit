use serde::{Deserialize, Serialize};

use crate::rules::RuleError;

/// Editing parameters a rule may set. Each has one value kind and, where
/// stated, a closed integer range or a closed set of text values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parameter {
    /// Pad kept before each kept word, in milliseconds.
    PadBeforeMs,
    /// Pad kept after each kept word, in milliseconds.
    PadAfterMs,
    /// Longest silence kept between words, in milliseconds.
    MaxPauseMs,
    /// Whether soft fillers are removed.
    RemoveFillers,
    /// Longest caption line, in characters.
    CaptionMaxChars,
    /// Delivery pace: `slow`, `normal` or `fast`.
    Pace,
}

/// The kind of value a parameter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Int,
    Bool,
    Text,
}

impl Parameter {
    pub fn kind(self) -> ValueKind {
        match self {
            Self::PadBeforeMs | Self::PadAfterMs | Self::MaxPauseMs | Self::CaptionMaxChars => {
                ValueKind::Int
            }
            Self::RemoveFillers => ValueKind::Bool,
            Self::Pace => ValueKind::Text,
        }
    }

    /// Inclusive integer range for integer parameters.
    pub fn int_range(self) -> Option<(i64, i64)> {
        match self {
            Self::PadBeforeMs | Self::PadAfterMs => Some((0, 2_000)),
            Self::MaxPauseMs => Some((0, 10_000)),
            Self::CaptionMaxChars => Some((8, 80)),
            Self::RemoveFillers | Self::Pace => None,
        }
    }

    /// Closed set of allowed text values for text parameters.
    pub fn text_choices(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Pace => Some(&["slow", "normal", "fast"]),
            _ => None,
        }
    }
}

/// A typed parameter value. Serialised as `{"type": ..., "value": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Int(i64),
    Bool(bool),
    Text(String),
}

impl Value {
    pub fn kind(&self) -> ValueKind {
        match self {
            Value::Int(_) => ValueKind::Int,
            Value::Bool(_) => ValueKind::Bool,
            Value::Text(_) => ValueKind::Text,
        }
    }
}

/// Where a rule lives. The name is the project, platform, content type or
/// channel it targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Scope {
    Project(String),
    Platform(String),
    ContentType(String),
    Channel(String),
}

impl Scope {
    /// Specificity rank. Higher is narrower and wins: project 0, platform 1,
    /// content type 2, channel 3.
    pub fn rank(&self) -> u8 {
        match self {
            Scope::Project(_) => 0,
            Scope::Platform(_) => 1,
            Scope::ContentType(_) => 2,
            Scope::Channel(_) => 3,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Scope::Project(n) | Scope::Platform(n) | Scope::ContentType(n) | Scope::Channel(n) => {
                n.as_str()
            }
        }
    }

    pub fn matches(&self, ctx: &Context) -> bool {
        match self {
            Scope::Project(n) => ctx.project == *n,
            Scope::Platform(n) => ctx.platform.as_deref() == Some(n.as_str()),
            Scope::ContentType(n) => ctx.content_type.as_deref() == Some(n.as_str()),
            Scope::Channel(n) => ctx.channel.as_deref() == Some(n.as_str()),
        }
    }
}

/// The context fields a condition may test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Project,
    Channel,
    ContentType,
    Platform,
}

/// The editing context a matrix resolves for. Optional fields are absent when
/// the caller does not know them; a scope or condition that needs an absent
/// field does not match.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Context {
    pub project: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}

impl Context {
    pub fn field(&self, field: Field) -> Option<&str> {
        match field {
            Field::Project => Some(self.project.as_str()),
            Field::Channel => self.channel.as_deref(),
            Field::ContentType => self.content_type.as_deref(),
            Field::Platform => self.platform.as_deref(),
        }
    }
}

/// An extra test on the context, beyond the scope. The default is `always`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Condition {
    #[default]
    Always,
    Equals {
        field: Field,
        value: String,
    },
    All {
        conditions: Vec<Condition>,
    },
    Any {
        conditions: Vec<Condition>,
    },
}

impl Condition {
    pub fn holds(&self, ctx: &Context) -> bool {
        match self {
            Condition::Always => true,
            Condition::Equals { field, value } => ctx.field(*field) == Some(value.as_str()),
            Condition::All { conditions } => conditions.iter().all(|c| c.holds(ctx)),
            Condition::Any { conditions } => conditions.iter().any(|c| c.holds(ctx)),
        }
    }

    fn validate(&self, rule: &str) -> Result<(), RuleError> {
        match self {
            Condition::Always => Ok(()),
            Condition::Equals { value, .. } => {
                if value.trim().is_empty() {
                    Err(RuleError::EmptyCondition(rule.to_owned()))
                } else {
                    Ok(())
                }
            }
            Condition::All { conditions } | Condition::Any { conditions } => {
                conditions.iter().try_for_each(|c| c.validate(rule))
            }
        }
    }
}

/// One parameter change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub parameter: Parameter,
    pub value: Value,
}

/// Who set a rule and when. Recorded for audit; not a precedence input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub set_by: String,
    /// RFC 3339 timestamp, for example `2026-10-16T09:00:00Z`.
    pub set_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_id: Option<String>,
}

/// A scoped editing rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub scope: Scope,
    #[serde(default)]
    pub condition: Condition,
    pub effect: Effect,
    /// Tie-break inside one scope rank. Higher wins. Defaults to 0.
    #[serde(default)]
    pub priority: i32,
    pub provenance: Provenance,
}

impl Rule {
    /// Checks the id, scope name, provenance, condition and effect range.
    pub fn validate(&self) -> Result<(), RuleError> {
        if self.id.trim().is_empty() {
            return Err(RuleError::EmptyId);
        }
        if self.scope.name().trim().is_empty() {
            return Err(RuleError::EmptyScope(self.id.clone()));
        }
        if self.provenance.set_by.trim().is_empty() || self.provenance.set_at.trim().is_empty() {
            return Err(RuleError::MissingProvenance(self.id.clone()));
        }
        self.condition.validate(&self.id)?;
        validate_effect(&self.effect, &self.id)
    }

    /// True when the scope matches the context and the condition holds.
    pub fn applies(&self, ctx: &Context) -> bool {
        self.scope.matches(ctx) && self.condition.holds(ctx)
    }
}

fn validate_effect(effect: &Effect, rule: &str) -> Result<(), RuleError> {
    let expected = effect.parameter.kind();
    let got = effect.value.kind();
    if expected != got {
        return Err(RuleError::TypeMismatch {
            rule: rule.to_owned(),
            parameter: effect.parameter,
            expected,
            got,
        });
    }
    match &effect.value {
        Value::Int(v) => {
            if let Some((min, max)) = effect.parameter.int_range() {
                if *v < min || *v > max {
                    return Err(RuleError::OutOfRange {
                        rule: rule.to_owned(),
                        parameter: effect.parameter,
                        min,
                        max,
                        got: *v,
                    });
                }
            }
        }
        Value::Text(t) => {
            if let Some(choices) = effect.parameter.text_choices() {
                if !choices.contains(&t.as_str()) {
                    return Err(RuleError::NotAllowed {
                        rule: rule.to_owned(),
                        parameter: effect.parameter,
                        value: t.clone(),
                    });
                }
            }
        }
        Value::Bool(_) => {}
    }
    Ok(())
}
