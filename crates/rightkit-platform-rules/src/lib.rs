//! `rightkit-platform-rules` - a versioned catalog of per-platform limits and rules.
//!
//! A limit is data: a [`Rule`] record with platform, account type, capability, format, source,
//! dates and revision. Code never holds a platform limit. The bundled catalog
//! ([`Catalog::bundled`]) is `assets/platform-rules.json`; an app merges its own newer records
//! with [`Catalog::merge`].
//!
//! Rules enforced in code:
//!
//! - **Promotion.** A record is `sourced` only if its grade is `official_fetched`, it names a
//!   source URL and a fetch date, its value is stated, and it carries no conflict note. An
//!   `unsourced` record carries no value, so a number from memory cannot sit in one.
//! - **No edits.** A change is a new revision: `rule_revision` is the previous latest plus one and
//!   `supersedes` names it. [`Catalog::merge`] refuses anything else.
//! - **Unsourced and stale block use.** [`Catalog::check`] fails for a rule that is unsourced,
//!   stale (stored `stale`, or past its `sunsetDate`), or absent.
//! - **Revision binding.** [`Catalog::revision`] is a stable hash of the limits of one platform.
//!   A brief, export or approval stores it; [`Catalog::is_current`] and [`Catalog::check`] refuse
//!   work bound to an older one.
//!
//! The app keeps: fetching and citing sources, what to do when a revision changes (hold or
//! regenerate), and the revision field on its own records.
//!
//! Units: a planned number is in the unit of the rule. [`Planned::unit`] lets the caller state its
//! unit so a mismatch is a [`Violation::UnitMismatch`] rather than a wrong pass.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const BUNDLED: &str = include_str!("../assets/platform-rules.json");
const SCHEMA: u32 = 1;

/// How a rule's source was obtained. Only [`SourceGrade::OfficialFetched`] can support `sourced`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceGrade {
    OfficialFetched,
    SearchSnippet,
    DonorSource,
    DocAsserted,
    None,
}

/// Stored status of a record. Staleness by sunset date is computed, see [`Rule::status_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Sourced,
    Unsourced,
    Stale,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Status::Sourced => "sourced",
            Status::Unsourced => "unsourced",
            Status::Stale => "stale",
        })
    }
}

/// The shape of a rule's limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Min,
    Max,
    Range,
    Exact,
    Boolean,
    Enum,
}

/// A quota window, for example 24 hours.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub length: u32,
    pub unit: String,
}

/// The decoded limit of a rule.
#[derive(Clone, Debug, PartialEq)]
pub enum Limit {
    Min(f64),
    Max(f64),
    Range { min: f64, max: f64 },
    ExactNumber(f64),
    ExactText(String),
    Boolean(bool),
    OneOf(Vec<String>),
    /// No stated value (an unsourced record).
    Unstated,
}

/// One platform rule record (one revision).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rule {
    pub rule_id: String,
    pub platform: String,
    /// `all` when the source does not split by account type.
    pub account_type: String,
    pub capability: String,
    /// `text`, `image`, `carousel`, `video`, `reel`, `dm`, or `any`.
    pub format: String,
    pub limit_kind: LimitKind,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    /// Number, string, bool or (for `enum`) an array of strings.
    #[serde(default)]
    pub value: Option<Value>,
    /// Bounds are inclusive unless this is `Some(false)`.
    #[serde(default)]
    pub inclusive: Option<bool>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub window: Option<Window>,
    /// ISO date `YYYY-MM-DD`; the rule is stale after it.
    #[serde(default)]
    pub sunset_date: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    /// Where the source is cited in the owner's research, for traceability.
    #[serde(default)]
    pub source_document: Option<String>,
    pub source_grade: SourceGrade,
    /// ISO date the platform gives for the rule, or `not_stated`.
    pub source_date: String,
    /// ISO date of the fetch.
    pub fetched_date: String,
    /// At least 1; increments on any change to limit, unit, window, sunset, status or grade.
    pub rule_revision: u32,
    #[serde(default)]
    pub supersedes: Option<u32>,
    pub status: Status,
    /// Required when sources disagree; a conflict makes the record `unsourced`.
    #[serde(default)]
    pub conflict_note: Option<String>,
}

impl Rule {
    pub fn revision(&self) -> u32 {
        self.rule_revision
    }

    /// The fetch date (`fetchedDate`).
    pub fn fetched(&self) -> &str {
        &self.fetched_date
    }

    /// The decoded limit.
    pub fn limit(&self) -> Limit {
        let num = |v: &Option<Value>| v.as_ref().and_then(Value::as_f64);
        match self.limit_kind {
            LimitKind::Min => self.min.map_or(Limit::Unstated, Limit::Min),
            LimitKind::Max => self.max.map_or(Limit::Unstated, Limit::Max),
            LimitKind::Range => match (self.min, self.max) {
                (Some(min), Some(max)) => Limit::Range { min, max },
                _ => Limit::Unstated,
            },
            LimitKind::Exact => match &self.value {
                Some(Value::String(s)) => Limit::ExactText(s.clone()),
                other => num(other).map_or(Limit::Unstated, Limit::ExactNumber),
            },
            LimitKind::Boolean => match &self.value {
                Some(Value::Bool(b)) => Limit::Boolean(*b),
                _ => Limit::Unstated,
            },
            LimitKind::Enum => match &self.value {
                Some(Value::String(s)) => Limit::OneOf(vec![s.clone()]),
                Some(Value::Array(items)) => {
                    let list: Option<Vec<String>> = items
                        .iter()
                        .map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    list.filter(|l| !l.is_empty()).map_or(Limit::Unstated, Limit::OneOf)
                }
                _ => Limit::Unstated,
            },
        }
    }

    /// Status on `today` (`YYYY-MM-DD`): a `sourced` record past its sunset date is `stale`.
    pub fn status_at(&self, today: &str) -> Status {
        match (self.status, self.sunset_date.as_deref()) {
            (Status::Sourced, Some(sunset)) if today > sunset => Status::Stale,
            (status, _) => status,
        }
    }

    /// True unless the record is `sourced` on `today`.
    pub fn blocks_use_at(&self, today: &str) -> bool {
        self.status_at(today) != Status::Sourced
    }

    fn key(&self) -> Key<'_> {
        (&self.platform, &self.account_type, &self.capability, &self.format)
    }

    /// Canonical text hashed into a [`RuleRevision`]. Source URL and dates are left out: a change
    /// to them alone has no content effect, so it does not change the revision.
    fn canonical(&self) -> String {
        let n = |v: Option<f64>| v.map_or("-".to_owned(), |x| format!("{x}"));
        let value = self
            .value
            .as_ref()
            .map_or("-".to_owned(), |v| serde_json::to_string(v).unwrap_or_default());
        let window = self
            .window
            .as_ref()
            .map_or("-".to_owned(), |w| format!("{}{}", w.length, w.unit));
        format!(
            "{}|{}|{}|{}|{:?}|{}|{}|{}|{:?}|{}|{}|{}|{:?}|{:?}",
            self.rule_id,
            self.account_type,
            self.capability,
            self.format,
            self.limit_kind,
            n(self.min),
            n(self.max),
            value,
            self.inclusive,
            self.unit.as_deref().unwrap_or("-"),
            window,
            self.sunset_date.as_deref().unwrap_or("-"),
            self.source_grade,
            self.status,
        )
    }
}

type Key<'a> = (&'a String, &'a String, &'a String, &'a String);

/// Stable hash of one platform's latest rules. Bind briefs, exports and approvals to it.
///
/// It is a change detector (two FNV-1a 64-bit lanes), not a cryptographic digest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleRevision {
    pub platform: String,
    pub hash: String,
}

impl fmt::Display for RuleRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.platform, self.hash)
    }
}

/// A planned value to check against the rules of one platform.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub account_type: String,
    pub capability: String,
    pub format: String,
    pub value: PlannedValue,
    /// The unit the caller measured in; when set it must equal the rule's unit.
    pub unit: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlannedValue {
    Number(f64),
    Bool(bool),
    Text(String),
}

impl Planned {
    pub fn number(account_type: &str, capability: &str, format: &str, value: f64) -> Self {
        Self::new(account_type, capability, format, PlannedValue::Number(value))
    }

    pub fn boolean(account_type: &str, capability: &str, format: &str, value: bool) -> Self {
        Self::new(account_type, capability, format, PlannedValue::Bool(value))
    }

    pub fn text(account_type: &str, capability: &str, format: &str, value: &str) -> Self {
        Self::new(account_type, capability, format, PlannedValue::Text(value.to_owned()))
    }

    fn new(account_type: &str, capability: &str, format: &str, value: PlannedValue) -> Self {
        Self {
            account_type: account_type.to_owned(),
            capability: capability.to_owned(),
            format: format.to_owned(),
            value,
            unit: None,
        }
    }

    /// State the unit of a planned number.
    pub fn in_unit(mut self, unit: &str) -> Self {
        self.unit = Some(unit.to_owned());
        self
    }
}

/// Why a planned value was refused.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Violation {
    /// The bound revision is not the catalog's current revision for the platform.
    StaleRevision { bound: RuleRevision, current: RuleRevision },
    /// No rule exists for the planned value. An absent rule blocks use like an unsourced one.
    NoRule { platform: String, account_type: String, capability: String, format: String },
    /// The rule is unsourced or stale on the check date.
    Blocked { rule_id: String, status: Status },
    UnitMismatch { rule_id: String, rule_unit: Option<String>, planned_unit: String },
    /// The planned value has a different type than the rule's limit.
    WrongType { rule_id: String },
    BelowMin { rule_id: String, planned: f64, min: f64 },
    AboveMax { rule_id: String, planned: f64, max: f64 },
    NotExact { rule_id: String, planned: String, expected: String },
    NotAllowed { rule_id: String, planned: String, allowed: Vec<String> },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::StaleRevision { bound, current } => {
                write!(f, "rules changed: bound to {bound}, current is {current}")
            }
            Violation::NoRule { platform, account_type, capability, format } => write!(
                f,
                "no rule for {platform}/{account_type}/{capability}/{format}; an absent rule blocks use"
            ),
            Violation::Blocked { rule_id, status } => {
                write!(f, "rule {rule_id} is {status} and blocks use")
            }
            Violation::UnitMismatch { rule_id, rule_unit, planned_unit } => write!(
                f,
                "rule {rule_id} is in {}, planned value is in {planned_unit}",
                rule_unit.as_deref().unwrap_or("no unit")
            ),
            Violation::WrongType { rule_id } => {
                write!(f, "planned value has the wrong type for rule {rule_id}")
            }
            Violation::BelowMin { rule_id, planned, min } => {
                write!(f, "{planned} is below the minimum {min} of rule {rule_id}")
            }
            Violation::AboveMax { rule_id, planned, max } => {
                write!(f, "{planned} is above the maximum {max} of rule {rule_id}")
            }
            Violation::NotExact { rule_id, planned, expected } => {
                write!(f, "{planned} is not {expected} (rule {rule_id})")
            }
            Violation::NotAllowed { rule_id, planned, allowed } => {
                write!(f, "{planned} is not one of {allowed:?} (rule {rule_id})")
            }
        }
    }
}

impl std::error::Error for Violation {}

/// A catalog or merge failure.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CatalogError {
    Json(String),
    Schema(u32),
    /// A record breaks a promotion or shape rule.
    Invalid { rule_id: String, reason: String },
    /// A record repeats an existing (key, revision).
    DuplicateRevision { rule_id: String, revision: u32 },
    /// A merged record is not the previous latest revision plus one naming it in `supersedes`.
    NotNextRevision { rule_id: String, expected: u32, got: u32 },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Json(e) => write!(f, "catalog JSON: {e}"),
            CatalogError::Schema(v) => write!(f, "catalog schema {v} is not supported"),
            CatalogError::Invalid { rule_id, reason } => write!(f, "rule {rule_id}: {reason}"),
            CatalogError::DuplicateRevision { rule_id, revision } => {
                write!(f, "rule {rule_id} revision {revision} already exists")
            }
            CatalogError::NotNextRevision { rule_id, expected, got } => write!(
                f,
                "rule {rule_id}: a change is a new revision; expected revision {expected}, got {got}"
            ),
        }
    }
}

impl std::error::Error for CatalogError {}

#[derive(Serialize, Deserialize)]
struct File {
    schema: u32,
    rules: Vec<Rule>,
}

/// A set of rule records, every revision kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    rules: Vec<Rule>,
}

impl Catalog {
    /// The catalog bundled with the crate. Valid by construction (a unit test parses it).
    pub fn bundled() -> Self {
        Self::from_json(BUNDLED).expect("bundled platform rules are valid; see unit test")
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse and validate a catalog document `{"schema": 1, "rules": [...]}`.
    pub fn from_json(text: &str) -> Result<Self, CatalogError> {
        let file: File = serde_json::from_str(text).map_err(|e| CatalogError::Json(e.to_string()))?;
        if file.schema != SCHEMA {
            return Err(CatalogError::Schema(file.schema));
        }
        let mut rules = file.rules;
        for rule in &rules {
            validate(rule)?;
        }
        rules.sort_by(|a, b| a.key().cmp(&b.key()).then(a.rule_revision.cmp(&b.rule_revision)));
        for pair in rules.windows(2) {
            if pair[0].key() == pair[1].key() && pair[0].rule_revision == pair[1].rule_revision {
                return Err(CatalogError::DuplicateRevision {
                    rule_id: pair[0].rule_id.clone(),
                    revision: pair[0].rule_revision,
                });
            }
        }
        // Each key's history must be 1, 2, 3 ... with `supersedes` chaining.
        let mut last: Option<&Rule> = None;
        for rule in &rules {
            let expected_prev = match last {
                Some(prev) if prev.key() == rule.key() => Some(prev.rule_revision),
                _ => None,
            };
            if rule.supersedes != expected_prev
                || rule.rule_revision != expected_prev.map_or(1, |p| p + 1)
            {
                return Err(CatalogError::NotNextRevision {
                    rule_id: rule.rule_id.clone(),
                    expected: expected_prev.map_or(1, |p| p + 1),
                    got: rule.rule_revision,
                });
            }
            last = Some(rule);
        }
        Ok(Self { rules })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&File { schema: SCHEMA, rules: self.rules.clone() })
            .unwrap_or_default()
    }

    /// Every record, every revision, ordered by key then revision.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Add override records. Returns a new catalog; `self` is unchanged. A record for a known
    /// key must be the next revision and name the previous one in `supersedes`; a record for a
    /// new key must be revision 1. Anything else is refused: a change is never an edit.
    pub fn merge(&self, overrides: Catalog) -> Result<Catalog, CatalogError> {
        let mut out = self.clone();
        let mut incoming = overrides.rules;
        incoming.sort_by(|a, b| a.key().cmp(&b.key()).then(a.rule_revision.cmp(&b.rule_revision)));
        for rule in incoming {
            validate(&rule)?;
            let latest = out.latest_for_key(rule.key()).map(|r| (r.rule_revision, r.rule_id.clone()));
            let expected = latest.as_ref().map_or(1, |(rev, _)| rev + 1);
            if rule.rule_revision != expected || rule.supersedes != latest.as_ref().map(|(r, _)| *r) {
                return Err(CatalogError::NotNextRevision {
                    rule_id: rule.rule_id.clone(),
                    expected,
                    got: rule.rule_revision,
                });
            }
            if let Some((_, id)) = &latest {
                if *id != rule.rule_id {
                    return Err(CatalogError::Invalid {
                        rule_id: rule.rule_id.clone(),
                        reason: format!("rule id must stay {id} across revisions"),
                    });
                }
            }
            out.rules.push(rule);
        }
        out.rules.sort_by(|a, b| a.key().cmp(&b.key()).then(a.rule_revision.cmp(&b.rule_revision)));
        Ok(out)
    }

    /// [`Catalog::merge`] with the overrides given as a catalog document.
    pub fn merge_json(&self, overrides: &str) -> Result<Catalog, CatalogError> {
        // Override documents hold only the new revisions, so chain validation is done by merge.
        let file: File = serde_json::from_str(overrides).map_err(|e| CatalogError::Json(e.to_string()))?;
        if file.schema != SCHEMA {
            return Err(CatalogError::Schema(file.schema));
        }
        self.merge(Catalog { rules: file.rules })
    }

    fn latest_for_key(&self, key: Key<'_>) -> Option<&Rule> {
        self.rules
            .iter()
            .filter(|r| r.key() == key)
            .max_by_key(|r| r.rule_revision)
    }

    /// The latest revision of the rule for these coordinates. Falls back from the given account
    /// type to `all`, and from the given format to `any`. Returns the record even when it blocks
    /// use; read [`Rule::status_at`].
    pub fn get(
        &self,
        platform: &str,
        account_type: &str,
        capability: &str,
        format: &str,
    ) -> Option<&Rule> {
        let accounts = [account_type, "all"];
        let formats = [format, "any"];
        for acct in accounts {
            for fmt in formats {
                let hit = self
                    .rules
                    .iter()
                    .filter(|r| {
                        r.platform == platform
                            && r.account_type == acct
                            && r.capability == capability
                            && r.format == fmt
                    })
                    .max_by_key(|r| r.rule_revision);
                if hit.is_some() {
                    return hit;
                }
            }
        }
        None
    }

    /// The rule revision of one platform: a stable hash over its latest records. It changes when
    /// a limit, unit, window, sunset, status or grade changes, and not for a source-only change.
    /// A platform with no records hashes the empty set.
    pub fn revision(&self, platform: &str) -> RuleRevision {
        let mut latest: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| r.platform == platform)
            .filter(|r| {
                self.latest_for_key(r.key()).is_some_and(|l| l.rule_revision == r.rule_revision)
            })
            .collect();
        latest.sort_by(|a, b| a.key().cmp(&b.key()));
        let mut a: u64 = 0xcbf2_9ce4_8422_2325;
        let mut b: u64 = 0x8422_2325_cbf2_9ce4;
        for rule in latest {
            for byte in rule.canonical().bytes().chain(std::iter::once(b'\n')) {
                a = (a ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
                b = (b ^ u64::from(byte.rotate_left(3))).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        RuleRevision { platform: platform.to_owned(), hash: format!("{a:016x}{b:016x}") }
    }

    /// True when `bound` equals this catalog's current revision for its platform.
    pub fn is_current(&self, bound: &RuleRevision) -> bool {
        self.revision(&bound.platform) == *bound
    }

    /// Check planned values against the rules, dated today (UTC, system clock). See
    /// [`Catalog::check_at`].
    pub fn check(&self, bound: &RuleRevision, planned: &[Planned]) -> Result<(), Violation> {
        self.check_at(bound, planned, &today_utc())
    }

    /// Check planned values for the platform of `bound` on `today` (`YYYY-MM-DD`). The first
    /// failure returns. The revision must be current, every value needs a rule, and an
    /// unsourced or stale rule blocks.
    pub fn check_at(
        &self,
        bound: &RuleRevision,
        planned: &[Planned],
        today: &str,
    ) -> Result<(), Violation> {
        let current = self.revision(&bound.platform);
        if current != *bound {
            return Err(Violation::StaleRevision { bound: bound.clone(), current });
        }
        for p in planned {
            let rule = self
                .get(&bound.platform, &p.account_type, &p.capability, &p.format)
                .ok_or_else(|| Violation::NoRule {
                    platform: bound.platform.clone(),
                    account_type: p.account_type.clone(),
                    capability: p.capability.clone(),
                    format: p.format.clone(),
                })?;
            check_one(rule, p, today)?;
        }
        Ok(())
    }
}

fn check_one(rule: &Rule, p: &Planned, today: &str) -> Result<(), Violation> {
    let id = || rule.rule_id.clone();
    let status = rule.status_at(today);
    if status != Status::Sourced {
        return Err(Violation::Blocked { rule_id: id(), status });
    }
    if let Some(planned_unit) = &p.unit {
        if rule.unit.as_ref() != Some(planned_unit) {
            return Err(Violation::UnitMismatch {
                rule_id: id(),
                rule_unit: rule.unit.clone(),
                planned_unit: planned_unit.clone(),
            });
        }
    }
    let inclusive = rule.inclusive.unwrap_or(true);
    let wrong = || Violation::WrongType { rule_id: id() };
    let below = |v: f64, min: f64| {
        if inclusive { v < min } else { v <= min }
    };
    let above = |v: f64, max: f64| {
        if inclusive { v > max } else { v >= max }
    };
    match (rule.limit(), &p.value) {
        (Limit::Min(min), PlannedValue::Number(v)) => {
            if below(*v, min) {
                return Err(Violation::BelowMin { rule_id: id(), planned: *v, min });
            }
        }
        (Limit::Max(max), PlannedValue::Number(v)) => {
            if above(*v, max) {
                return Err(Violation::AboveMax { rule_id: id(), planned: *v, max });
            }
        }
        (Limit::Range { min, max }, PlannedValue::Number(v)) => {
            if below(*v, min) {
                return Err(Violation::BelowMin { rule_id: id(), planned: *v, min });
            }
            if above(*v, max) {
                return Err(Violation::AboveMax { rule_id: id(), planned: *v, max });
            }
        }
        (Limit::ExactNumber(x), PlannedValue::Number(v)) => {
            if (*v - x).abs() > f64::EPSILON {
                return Err(Violation::NotExact {
                    rule_id: id(),
                    planned: v.to_string(),
                    expected: x.to_string(),
                });
            }
        }
        (Limit::ExactText(x), PlannedValue::Text(v)) => {
            if *v != x {
                return Err(Violation::NotExact { rule_id: id(), planned: v.clone(), expected: x });
            }
        }
        (Limit::Boolean(x), PlannedValue::Bool(v)) => {
            if *v != x {
                return Err(Violation::NotExact {
                    rule_id: id(),
                    planned: v.to_string(),
                    expected: x.to_string(),
                });
            }
        }
        (Limit::OneOf(allowed), PlannedValue::Text(v)) => {
            if !allowed.contains(v) {
                return Err(Violation::NotAllowed { rule_id: id(), planned: v.clone(), allowed });
            }
        }
        (Limit::Unstated, _) => return Err(Violation::Blocked { rule_id: id(), status: Status::Unsourced }),
        _ => return Err(wrong()),
    }
    Ok(())
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let month: u32 = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn validate(rule: &Rule) -> Result<(), CatalogError> {
    let bad = |reason: &str| CatalogError::Invalid {
        rule_id: rule.rule_id.clone(),
        reason: reason.to_owned(),
    };
    for (name, field) in [
        ("ruleId", &rule.rule_id),
        ("platform", &rule.platform),
        ("accountType", &rule.account_type),
        ("capability", &rule.capability),
        ("format", &rule.format),
    ] {
        if field.trim().is_empty() {
            return Err(bad(&format!("{name} is empty")));
        }
    }
    if rule.rule_revision < 1 {
        return Err(bad("ruleRevision must be at least 1"));
    }
    if let Some(s) = rule.supersedes {
        if s >= rule.rule_revision {
            return Err(bad("supersedes must be below ruleRevision"));
        }
    }
    if !is_iso_date(&rule.fetched_date) {
        return Err(bad("fetchedDate must be YYYY-MM-DD"));
    }
    if rule.source_date != "not_stated" && !is_iso_date(&rule.source_date) {
        return Err(bad("sourceDate must be YYYY-MM-DD or not_stated"));
    }
    if rule.sunset_date.as_deref().is_some_and(|d| !is_iso_date(d)) {
        return Err(bad("sunsetDate must be YYYY-MM-DD"));
    }
    if let (Some(a), Some(b)) = (rule.min, rule.max) {
        if a > b {
            return Err(bad("min is above max"));
        }
    }
    let stated = rule.limit() != Limit::Unstated;
    match rule.status {
        Status::Sourced => {
            if rule.source_grade != SourceGrade::OfficialFetched {
                return Err(bad("sourced needs sourceGrade official_fetched"));
            }
            if rule.source_url.as_deref().is_none_or(|u| u.trim().is_empty()) {
                return Err(bad("sourced needs a sourceUrl"));
            }
            if !stated {
                return Err(bad("sourced needs a stated value"));
            }
            if rule.conflict_note.is_some() {
                return Err(bad("a record with a conflictNote is unsourced"));
            }
        }
        Status::Unsourced => {
            if rule.min.is_some() || rule.max.is_some() || rule.value.is_some() {
                return Err(bad("an unsourced record carries no value"));
            }
        }
        Status::Stale => {
            if !stated {
                return Err(bad("a stale record keeps its last stated value"));
            }
        }
    }
    Ok(())
}

/// Today's UTC date as `YYYY-MM-DD` from the system clock.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: &str = "2026-10-10";

    fn sourced(id: &str, platform: &str, cap: &str, kind: &str, extra: &str) -> String {
        format!(
            r#"{{"ruleId":"{id}","platform":"{platform}","accountType":"all","capability":"{cap}","format":"text","limitKind":"{kind}",{extra},"sourceUrl":"https://example.test/doc","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-08","ruleRevision":1,"status":"sourced"}}"#
        )
    }

    fn doc(rules: &[String]) -> String {
        format!(r#"{{"schema":1,"rules":[{}]}}"#, rules.join(","))
    }

    #[test]
    fn bundled_parses_and_is_valid() {
        let c = Catalog::bundled();
        assert!(c.rules().len() >= 18);
        let r = c.get("threads", "all", "text.length", "text").unwrap();
        assert_eq!(r.limit(), Limit::Max(500.0));
        assert_eq!(r.revision(), 1);
        assert_eq!(r.fetched(), "2026-10-08");
    }

    #[test]
    fn get_falls_back_to_all_accounts_and_any_format() {
        let c = Catalog::bundled();
        // threads.publish.window is format any, account all.
        assert!(c.get("threads", "member", "publish.window", "text").is_some());
        // x video limits differ by account type and must not fall through to each other.
        let d = c.get("x", "default", "video.duration", "video").unwrap();
        let p = c.get("x", "premium_or_verified", "video.duration", "video").unwrap();
        assert_eq!(d.limit(), Limit::Max(20.0));
        assert_eq!(p.limit(), Limit::Max(125.0));
        assert!(c.get("x", "other", "video.duration", "video").is_none());
        assert!(c.get("threads", "all", "no.such", "text").is_none());
    }

    #[test]
    fn check_passes_at_limit_and_fails_over_it() {
        let c = Catalog::bundled();
        let rev = c.revision("threads");
        let at = Planned::number("all", "text.length", "text", 500.0).in_unit("characters");
        assert_eq!(c.check_at(&rev, &[at], TODAY), Ok(()));
        let over = Planned::number("all", "text.length", "text", 501.0);
        assert!(matches!(c.check_at(&rev, &[over], TODAY), Err(Violation::AboveMax { .. })));
        let low = Planned::number("all", "carousel.item_count", "carousel", 1.0);
        assert!(matches!(c.check_at(&rev, &[low], TODAY), Err(Violation::BelowMin { .. })));
        let top = Planned::number("all", "carousel.item_count", "carousel", 20.0);
        assert_eq!(c.check_at(&rev, &[top], TODAY), Ok(()));
    }

    #[test]
    fn x_checks_both_account_types() {
        let c = Catalog::bundled();
        let rev = c.revision("x");
        let ok = Planned::number("premium_or_verified", "video.duration", "video", 125.0);
        assert_eq!(c.check_at(&rev, &[ok], TODAY), Ok(()));
        let over = Planned::number("default", "video.duration", "video", 21.0);
        assert!(matches!(c.check_at(&rev, &[over], TODAY), Err(Violation::AboveMax { .. })));
    }

    #[test]
    fn unit_mismatch_type_mismatch_and_missing_rule() {
        let c = Catalog::bundled();
        let rev = c.revision("threads");
        let wrong_unit = Planned::number("all", "text.length", "text", 10.0).in_unit("words");
        assert!(matches!(c.check_at(&rev, &[wrong_unit], TODAY), Err(Violation::UnitMismatch { .. })));
        let wrong_type = Planned::text("all", "text.length", "text", "ten");
        assert!(matches!(c.check_at(&rev, &[wrong_type], TODAY), Err(Violation::WrongType { .. })));
        let none = Planned::number("all", "no.such", "text", 1.0);
        assert!(matches!(c.check_at(&rev, &[none], TODAY), Err(Violation::NoRule { .. })));
    }

    #[test]
    fn unsourced_rule_blocks_use() {
        let c = Catalog::bundled();
        let rev = c.revision("instagram");
        let p = Planned::number("all", "video.duration", "reel", 30.0);
        assert!(matches!(
            c.check_at(&rev, &[p], TODAY),
            Err(Violation::Blocked { status: Status::Unsourced, .. })
        ));
    }

    #[test]
    fn sunset_makes_a_sourced_rule_stale() {
        let c = Catalog::bundled();
        let rev = c.revision("linkedin");
        let p = Planned::text("all", "api_version", "any", "202510");
        assert_eq!(c.check_at(&rev, std::slice::from_ref(&p), "2026-10-15"), Ok(()));
        assert!(matches!(
            c.check_at(&rev, &[p], "2026-10-16"),
            Err(Violation::Blocked { status: Status::Stale, .. })
        ));
    }

    #[test]
    fn enum_and_boolean_rules() {
        let c = Catalog::bundled();
        let rev = c.revision("instagram");
        let ok = Planned::text("all", "trial_reel.graduation", "reel", "MANUAL");
        assert_eq!(c.check_at(&rev, &[ok], TODAY), Ok(()));
        let bad = Planned::text("all", "trial_reel.graduation", "reel", "AUTO");
        assert!(matches!(c.check_at(&rev, &[bad], TODAY), Err(Violation::NotAllowed { .. })));
        let rev = c.revision("threads");
        let host = Planned::boolean("all", "media.public_host", "any", false);
        assert!(matches!(c.check_at(&rev, &[host], TODAY), Err(Violation::NotExact { .. })));
    }

    #[test]
    fn revision_is_stable_and_platform_scoped() {
        let c = Catalog::bundled();
        assert_eq!(c.revision("threads"), Catalog::bundled().revision("threads"));
        assert_ne!(c.revision("threads").hash, c.revision("x").hash);
        assert_eq!(c.revision("threads").hash.len(), 32);
        assert!(c.is_current(&c.revision("threads")));
    }

    #[test]
    fn new_revision_changes_the_hash_and_refuses_old_bindings() {
        let base = Catalog::bundled();
        let old = base.revision("threads");
        let next = r#"{"ruleId":"threads.text.length","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":600,"inclusive":true,"unit":"characters","sourceUrl":"https://example.test/new","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":2,"supersedes":1,"status":"sourced"}"#;
        let merged = base.merge_json(&doc(&[next.to_owned()])).unwrap();
        assert_eq!(base.revision("threads"), old, "merge returns a new catalog");
        assert!(!merged.is_current(&old));
        assert_ne!(merged.revision("threads"), old);
        assert_eq!(merged.get("threads", "all", "text.length", "text").unwrap().revision(), 2);
        assert_eq!(merged.rules().len(), base.rules().len() + 1, "history is kept");
        let p = Planned::number("all", "text.length", "text", 550.0);
        assert!(matches!(merged.check_at(&old, std::slice::from_ref(&p), TODAY), Err(Violation::StaleRevision { .. })));
        assert_eq!(merged.check_at(&merged.revision("threads"), &[p], TODAY), Ok(()));
        // Other platforms are untouched.
        assert_eq!(merged.revision("x"), base.revision("x"));
    }

    #[test]
    fn source_only_change_keeps_the_hash() {
        let base = Catalog::bundled();
        let same_limit = r#"{"ruleId":"threads.text.length","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":500,"inclusive":true,"unit":"characters","sourceUrl":"https://example.test/moved","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":2,"supersedes":1,"status":"sourced"}"#;
        let merged = base.merge_json(&doc(&[same_limit.to_owned()])).unwrap();
        assert_eq!(merged.revision("threads"), base.revision("threads"));
    }

    #[test]
    fn merge_refuses_edits_and_skipped_revisions() {
        let base = Catalog::bundled();
        let edit = sourced("threads.text.length", "threads", "text.length", "max", r#""max":900"#);
        assert!(matches!(
            base.merge_json(&doc(&[edit])),
            Err(CatalogError::NotNextRevision { expected: 2, got: 1, .. })
        ));
        let skip = r#"{"ruleId":"threads.text.length","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":900,"sourceUrl":"https://example.test/x","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":3,"supersedes":2,"status":"sourced"}"#;
        assert!(matches!(
            base.merge_json(&doc(&[skip.to_owned()])),
            Err(CatalogError::NotNextRevision { expected: 2, got: 3, .. })
        ));
        let renamed = r#"{"ruleId":"threads.other","platform":"threads","accountType":"all","capability":"text.length","format":"text","limitKind":"max","max":900,"sourceUrl":"https://example.test/x","sourceGrade":"official_fetched","sourceDate":"not_stated","fetchedDate":"2026-10-20","ruleRevision":2,"supersedes":1,"status":"sourced"}"#;
        assert!(matches!(base.merge_json(&doc(&[renamed.to_owned()])), Err(CatalogError::Invalid { .. })));
    }

    #[test]
    fn merge_adds_a_new_key_at_revision_one() {
        let base = Catalog::bundled();
        let new = sourced("bluesky.text.length", "bluesky", "text.length", "max", r#""max":300"#);
        let merged = base.merge_json(&doc(&[new])).unwrap();
        assert_eq!(merged.get("bluesky", "all", "text.length", "text").unwrap().limit(), Limit::Max(300.0));
    }

    #[test]
    fn promotion_rule_is_enforced_on_load() {
        // Wrong grade.
        let snippet = sourced("a.b", "a", "b", "max", r#""max":1"#).replace("official_fetched", "search_snippet");
        assert!(matches!(Catalog::from_json(&doc(&[snippet])), Err(CatalogError::Invalid { .. })));
        // No source URL.
        let no_url = sourced("a.b", "a", "b", "max", r#""max":1"#).replace(r#""sourceUrl":"https://example.test/doc","#, "");
        assert!(matches!(Catalog::from_json(&doc(&[no_url])), Err(CatalogError::Invalid { .. })));
        // No stated value.
        let no_value = sourced("a.b", "a", "b", "max", r#""unit":"x""#);
        assert!(matches!(Catalog::from_json(&doc(&[no_value])), Err(CatalogError::Invalid { .. })));
        // Conflict note on a sourced record.
        let conflict = sourced("a.b", "a", "b", "max", r#""max":1,"conflictNote":"sources disagree""#);
        assert!(matches!(Catalog::from_json(&doc(&[conflict])), Err(CatalogError::Invalid { .. })));
        // An unsourced record may not carry a number.
        let unsourced = r#"{"ruleId":"a.b","platform":"a","accountType":"all","capability":"b","format":"text","limitKind":"max","max":95,"sourceGrade":"none","sourceDate":"not_stated","fetchedDate":"2026-10-08","ruleRevision":1,"status":"unsourced"}"#;
        assert!(matches!(Catalog::from_json(&doc(&[unsourced.to_owned()])), Err(CatalogError::Invalid { .. })));
    }

    #[test]
    fn shape_errors_are_refused() {
        assert!(matches!(Catalog::from_json("{"), Err(CatalogError::Json(_))));
        assert!(matches!(Catalog::from_json(r#"{"schema":2,"rules":[]}"#), Err(CatalogError::Schema(2))));
        let bad_date = sourced("a.b", "a", "b", "max", r#""max":1"#).replace("2026-10-08", "2026-13-40");
        assert!(matches!(Catalog::from_json(&doc(&[bad_date])), Err(CatalogError::Invalid { .. })));
        let dup = sourced("a.b", "a", "b", "max", r#""max":1"#);
        assert!(matches!(
            Catalog::from_json(&doc(&[dup.clone(), dup])),
            Err(CatalogError::DuplicateRevision { .. })
        ));
    }

    #[test]
    fn exclusive_bounds() {
        let r = sourced("a.b", "a", "b", "max", r#""max":10,"inclusive":false"#);
        let c = Catalog::from_json(&doc(&[r])).unwrap();
        let rev = c.revision("a");
        let at = Planned::number("all", "b", "text", 10.0);
        assert!(matches!(c.check_at(&rev, &[at], TODAY), Err(Violation::AboveMax { .. })));
        let under = Planned::number("all", "b", "text", 9.0);
        assert_eq!(c.check_at(&rev, &[under], TODAY), Ok(()));
    }

    #[test]
    fn json_round_trip_keeps_the_revision() {
        let c = Catalog::bundled();
        let again = Catalog::from_json(&c.to_json()).unwrap();
        assert_eq!(again, c);
        assert_eq!(again.revision("linkedin"), c.revision("linkedin"));
    }

    #[test]
    fn civil_date_conversion() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_743), (2026, 10, 17));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert!(is_iso_date(&today_utc()));
    }
}
