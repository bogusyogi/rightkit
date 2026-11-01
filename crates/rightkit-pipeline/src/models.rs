//! The engine's model catalog (`genright-engine models`) and spec validation
//! against it.
//!
//! Wire shape (GenRight `docs/ENGINE-CLI.md`, contract v2): a bare JSON array
//! `[{provider, model, kind, label, est_usd}]`, `kind` being `image`,
//! `video`, `voice` or `avatar`.
//!
//! Durations come in two flavours. An engine that advertises the
//! `models.durations` capability (v1) gives structured fields per row:
//! `durations_s` (a closed set, always an array, so `[4]` is a one-value
//! closed set), or `min_duration_s`/`max_duration_s`, plus an optional
//! `default_duration_s`; rows without them take no duration (or, like
//! InfiniteTalk, follow their input audio). With the capability the rules
//! are enforced strictly from those fields and the label is never parsed
//! ([`ModelCatalog::from_value_structured`]).
//!
//! An older engine has only the human label ("(5 or 8 s, ...)", "(3-15 s,
//! ...)", "(4 s, ...)"). [`ModelCatalog::from_value`] reads durations from
//! there (and honors any structured fields that happen to be present). A
//! label with a single value ("4 s") names the model's default, not a closed
//! set, so it is NOT enforced; the engine stays authoritative and its
//! `invalid_request` rejection is recorded on the journaled request.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::{GenerateSpec, MediaKind};
use crate::error::Error;

/// Which video durations (whole seconds) a model accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum DurationRule {
    /// The catalog does not say; nothing is enforced.
    Unspecified,
    /// The label states one value: the default, not enforced.
    Default { secs: u32 },
    /// Exactly these values.
    Set { secs: Vec<u32> },
    /// Any whole number in `min..=max`.
    Range { min: u32, max: u32 },
}

impl DurationRule {
    /// `Some(allowed?)` when the rule is enforceable, `None` otherwise.
    pub fn allows(&self, secs: u32) -> Option<bool> {
        match self {
            Self::Unspecified | Self::Default { .. } => None,
            Self::Set { secs: s } => Some(s.contains(&secs)),
            Self::Range { min, max } => Some((*min..=*max).contains(&secs)),
        }
    }

    /// Human description of the enforced set, empty when not enforced.
    pub fn describe(&self) -> String {
        match self {
            Self::Set { secs } => secs
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(" or "),
            Self::Range { min, max } => format!("{min}-{max}"),
            _ => String::new(),
        }
    }
}

/// One catalog row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    /// `image`, `video`, `voice` or `avatar` (avatar is not a [`MediaKind`]).
    pub kind: String,
    pub label: String,
    pub est_usd: Option<f64>,
    pub durations: DurationRule,
    /// `default_duration_s` from an engine advertising `models.durations`;
    /// used when a video request omits its duration.
    #[serde(default)]
    pub default_duration_s: Option<u32>,
}

impl ModelInfo {
    pub fn media_kind(&self) -> Option<MediaKind> {
        match self.kind.as_str() {
            "image" => Some(MediaKind::Image),
            "video" => Some(MediaKind::Video),
            "voice" => Some(MediaKind::Voice),
            _ => None,
        }
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        Self::parse(v, false)
    }

    /// Parse a row from an engine that advertises `models.durations`: only
    /// the structured fields are read, never the label.
    pub fn from_value_structured(v: &Value) -> Option<Self> {
        Self::parse(v, true)
    }

    fn parse(v: &Value, structured: bool) -> Option<Self> {
        let provider = v["provider"].as_str()?.to_string();
        let model = v["model"].as_str()?.to_string();
        let label = v["label"].as_str().unwrap_or("").to_string();
        let durations = if structured {
            capability_durations(v).unwrap_or(DurationRule::Unspecified)
        } else {
            structured_durations(v).unwrap_or_else(|| parse_label_durations(&label))
        };
        let default_duration_s = if structured {
            v["default_duration_s"]
                .as_u64()
                .and_then(|d| u32::try_from(d).ok())
        } else {
            None
        };
        Some(Self {
            provider,
            model,
            kind: v["kind"].as_str().unwrap_or("").to_string(),
            label,
            est_usd: v["est_usd"].as_f64(),
            durations,
            default_duration_s,
        })
    }
}

/// The `models.durations` fields: `durations_s` (a closed set, even with one
/// value) or `min_duration_s` + `max_duration_s`.
fn capability_durations(v: &Value) -> Option<DurationRule> {
    if let Some(a) = v["durations_s"].as_array() {
        let mut secs: Vec<u32> = a
            .iter()
            .filter_map(|x| x.as_u64())
            .filter_map(|x| u32::try_from(x).ok())
            .collect();
        secs.sort_unstable();
        secs.dedup();
        if !secs.is_empty() {
            return Some(DurationRule::Set { secs });
        }
    }
    let min = u32::try_from(v["min_duration_s"].as_u64()?).ok()?;
    let max = u32::try_from(v["max_duration_s"].as_u64()?).ok()?;
    (min <= max).then_some(DurationRule::Range { min, max })
}

fn structured_durations(v: &Value) -> Option<DurationRule> {
    if let Some(a) = v["durations"].as_array() {
        let mut secs: Vec<u32> = a
            .iter()
            .filter_map(|x| x.as_u64())
            .map(|x| x as u32)
            .collect();
        secs.sort_unstable();
        if !secs.is_empty() {
            return Some(DurationRule::Set { secs });
        }
    }
    let (min, max) = (v["min_duration_s"].as_u64()?, v["max_duration_s"].as_u64()?);
    Some(DurationRule::Range {
        min: min as u32,
        max: max as u32,
    })
}

/// Read a duration rule from a label such as `Wan 2.2 (5 or 8 s, $0.01/s)`.
pub fn parse_label_durations(label: &str) -> DurationRule {
    let Some(open) = label.find('(') else {
        return DurationRule::Unspecified;
    };
    let inner = label[open + 1..].split(')').next().unwrap_or("");
    for part in inner.split(',') {
        let Some(body) = part.trim().strip_suffix(" s") else {
            continue;
        };
        let nums = |sep: &str| -> Option<Vec<u32>> {
            body.split(sep).map(|n| n.trim().parse().ok()).collect()
        };
        if body.contains(" or ") {
            if let Some(mut secs) = nums(" or ") {
                secs.sort_unstable();
                return DurationRule::Set { secs };
            }
        } else if body.contains('-') {
            if let Some(r) = nums("-").filter(|r| r.len() == 2 && r[0] <= r[1]) {
                return DurationRule::Range {
                    min: r[0],
                    max: r[1],
                };
            }
        } else if let Ok(secs) = body.trim().parse() {
            return DurationRule::Default { secs };
        }
    }
    DurationRule::Unspecified
}

/// A spec that the catalog says the engine would refuse. Converts into
/// [`Error::InvalidParams`] (the allowed set is in the message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecViolation {
    pub field: String,
    pub message: String,
    /// The accepted values for `field` (kinds, or durations in seconds).
    pub allowed: Vec<String>,
}

impl From<SpecViolation> for Error {
    fn from(v: SpecViolation) -> Self {
        Error::InvalidParams {
            field: v.field,
            message: v.message,
        }
    }
}

/// The catalog as returned by `models`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub models: Vec<ModelInfo>,
    /// True when the rows came from an engine advertising `models.durations`:
    /// duration rules are strict and `default_duration_s` is applied.
    #[serde(default)]
    pub structured_durations: bool,
}

impl ModelCatalog {
    pub fn from_value(v: &Value) -> Self {
        Self::build(v, false)
    }

    /// The catalog of an engine advertising `models.durations`.
    pub fn from_value_structured(v: &Value) -> Self {
        Self::build(v, true)
    }

    fn build(v: &Value, structured: bool) -> Self {
        // Contract: a bare array; tolerate `{models:[...]}`.
        let rows = v.as_array().or_else(|| v["models"].as_array());
        let parse = if structured {
            ModelInfo::from_value_structured
        } else {
            ModelInfo::from_value
        };
        Self {
            models: rows
                .map(|a| a.iter().filter_map(parse).collect())
                .unwrap_or_default(),
            structured_durations: structured,
        }
    }

    /// `spec` with the model's `default_duration_s` filled in when a video
    /// request omits its duration. `None` when nothing changes (not a
    /// structured catalog, not video, unknown model, no default, or a
    /// duration already present).
    pub fn with_default_duration(&self, spec: &GenerateSpec) -> Option<GenerateSpec> {
        if !self.structured_durations || spec.kind != MediaKind::Video {
            return None;
        }
        let d = self.get(&spec.provider, &spec.model)?.default_duration_s?;
        if spec.params.get("duration").is_some_and(|v| !v.is_null()) {
            return None;
        }
        let mut out = spec.clone();
        if out.params.is_null() {
            out.params = serde_json::json!({});
        }
        out.params
            .as_object_mut()?
            .insert("duration".into(), Value::from(d));
        Some(out)
    }

    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelInfo> {
        self.models
            .iter()
            .find(|m| m.provider == provider && m.model == model)
    }

    /// Models of one kind (`image`, `video`, `voice`, `avatar`), optionally
    /// for one provider.
    pub fn of_kind<'a>(
        &'a self,
        kind: &'a str,
        provider: Option<&'a str>,
    ) -> impl Iterator<Item = &'a ModelInfo> + 'a {
        self.models
            .iter()
            .filter(move |m| m.kind == kind && provider.is_none_or(|p| m.provider == p))
    }

    /// Cheapest listed model of a kind (by `est_usd`).
    pub fn cheapest(&self, kind: &str) -> Option<&ModelInfo> {
        self.models
            .iter()
            .filter(|m| m.kind == kind)
            .filter(|m| m.est_usd.is_some())
            .min_by(|a, b| {
                a.est_usd
                    .partial_cmp(&b.est_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Check `spec` against the catalog. Unknown provider+model pairs pass
    /// (the engine's `unknown_model` is authoritative); a known model must
    /// match the requested kind, and a video `params.duration` must satisfy
    /// the model's enforceable duration rule.
    pub fn validate(&self, spec: &GenerateSpec) -> Result<(), SpecViolation> {
        let Some(m) = self.get(&spec.provider, &spec.model) else {
            return Ok(());
        };
        if m.kind != spec.kind.as_str() {
            return Err(SpecViolation {
                field: "kind".into(),
                message: format!(
                    "{}/{} is a {} model, not {} (allowed kind: {})",
                    m.provider,
                    m.model,
                    m.kind,
                    spec.kind.as_str(),
                    m.kind
                ),
                allowed: vec![m.kind.clone()],
            });
        }
        if spec.kind != MediaKind::Video {
            return Ok(());
        }
        let Some(v) = spec.params.get("duration").filter(|v| !v.is_null()) else {
            return Ok(());
        };
        let secs = v
            .as_u64()
            .or_else(|| {
                v.as_str()
                    .and_then(|s| s.trim().trim_end_matches('s').parse().ok())
            })
            .and_then(|s| u32::try_from(s).ok());
        if matches!(
            m.durations,
            DurationRule::Unspecified | DurationRule::Default { .. }
        ) || secs.is_some_and(|s| m.durations.allows(s) == Some(true))
        {
            return Ok(());
        }
        let allowed = match &m.durations {
            DurationRule::Set { secs } => secs.iter().map(u32::to_string).collect(),
            DurationRule::Range { min, max } => (*min..=*max).map(|s| s.to_string()).collect(),
            _ => Vec::new(),
        };
        Err(SpecViolation {
            field: "params.duration".into(),
            message: format!(
                "{}/{} accepts video durations of {} seconds (got {v})",
                m.provider,
                m.model,
                m.durations.describe()
            ),
            allowed,
        })
    }
}
