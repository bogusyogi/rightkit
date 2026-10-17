//! A versioned, dated and qualified price catalogue (Orthic-Labs/rightkit#302).
//!
//! [`crate::pricing::PriceTable`] prices model tokens and nothing else. The catalogue holds
//! price rows for every paid thing an app meters: LLM tokens, transcription, scraping, image
//! and video generation, and platform fees. Each row says
//!
//! - **what** it prices (`kind`, `id`) and in which units (`components`);
//! - **when** it applies (`effective_from`, optional `effective_until`, both `YYYY-MM-DD`);
//! - **how far to trust it** (`qualification`: `Verified` against the vendor's price page or
//!   an invoice, `DocAsserted` from vendor documentation or a donor table, `Estimate`), plus
//!   `source` and `checked_on`.
//!
//! The catalogue as a whole carries a `schema`, a `version` and a `published` date, so a
//! cost report can name the exact price set it used.
//!
//! Lookups never guess. [`PriceCatalogue::price_at`] needs an exact id, a date inside the
//! row's window and a qualification at or above the caller's floor; each miss is its own
//! [`CatalogueError`], so a paid route can be blocked for a stated reason. There is no prefix
//! match and no fallback to an older row.
//!
//! The bundled catalogue holds the LLM rows of the bundled price table only, all
//! `DocAsserted` with no `checked_on`. Rows for the other kinds come from the host (load them
//! with [`PriceCatalogue::from_json`] and [`PriceCatalogue::merge`]); this crate ships no
//! price it cannot source.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::pricing::{normalize, ModelPrice, PriceError, PriceTable};
use crate::types::Usage;

const BUNDLED: &str = include_str!("../assets/price-catalogue.json");

/// The only schema this crate reads.
pub const CATALOGUE_SCHEMA: u32 = 1;

/// What a row prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PriceKind {
    Llm,
    Transcriber,
    Scraper,
    /// Image and video generation.
    Genright,
    /// A fee charged by a platform the app posts to or sells through.
    PlatformFee,
}

/// The unit one [`PriceComponent`] is quoted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PriceUnit {
    PerMillionInputTokens,
    PerMillionOutputTokens,
    PerMillionCacheReadTokens,
    PerMillionCacheWriteTokens,
    PerAudioMinute,
    PerRequest,
    PerPage,
    PerImage,
    PerVideoSecond,
    /// A fixed fee per transaction, in USD.
    PerTransaction,
    /// A share of the transaction value, as a fraction (`0.029` is 2.9 percent).
    Fraction,
}

/// How far a price is trusted. Ordered: `Estimate < DocAsserted < Verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Qualification {
    /// A planning figure, not a published price.
    Estimate,
    /// Stated in vendor documentation or a donor table, not checked against a bill.
    DocAsserted,
    /// Checked against the vendor's price page or an invoice on `checked_on`.
    Verified,
}

/// One quoted amount: USD per `unit` (or a fraction for [`PriceUnit::Fraction`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceComponent {
    pub unit: PriceUnit,
    pub amount: f64,
}

/// One priced thing over one date window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogueRow {
    pub kind: PriceKind,
    /// Model slug, vendor product name or fee name. Matched exactly (case, `vendor/` prefix,
    /// `.` and `_` ignored for [`PriceKind::Llm`]; case only for the others).
    pub id: String,
    pub components: Vec<PriceComponent>,
    /// First day the row applies, `YYYY-MM-DD`.
    pub effective_from: String,
    /// Last day the row applies (inclusive), `YYYY-MM-DD`; absent while it is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<String>,
    pub qualification: Qualification,
    /// Where the price came from: a URL, a document or a donor table.
    pub source: String,
    /// Day the price was last checked against `source`, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_on: Option<String>,
    /// Context window in tokens, for [`PriceKind::Llm`] rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl CatalogueRow {
    /// The amount quoted in `unit`, if the row has it.
    pub fn amount(&self, unit: PriceUnit) -> Option<f64> {
        self.components
            .iter()
            .find(|c| c.unit == unit)
            .map(|c| c.amount)
    }

    fn applies_on(&self, date: &str) -> bool {
        self.effective_from.as_str() <= date
            && self
                .effective_until
                .as_deref()
                .is_none_or(|until| date <= until)
    }
}

/// Why a catalogue could not be read, merged or queried.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CatalogueError {
    /// The JSON could not be read.
    Parse(String),
    /// The catalogue's `schema` is not [`CATALOGUE_SCHEMA`].
    UnsupportedSchema(u32),
    /// A row is malformed (bad date, empty id or source, negative amount, ...).
    InvalidRow { id: String, reason: String },
    /// Two rows for the same kind and id have overlapping date windows.
    Overlap { kind: PriceKind, id: String },
    /// A merge met a row with the same kind, id and start date but different content.
    Conflict { kind: PriceKind, id: String },
    /// A query date is not `YYYY-MM-DD`.
    BadDate(String),
    /// No row for this kind and id at all.
    Unpriced { kind: PriceKind, id: String },
    /// Rows exist for the id, none in effect on `date`.
    NotInEffect {
        kind: PriceKind,
        id: String,
        date: String,
    },
    /// The row in effect is trusted less than the caller's floor.
    BelowQualification {
        kind: PriceKind,
        id: String,
        have: Qualification,
        need: Qualification,
    },
    /// The row has no component for a unit the usage needs.
    MissingComponent {
        kind: PriceKind,
        id: String,
        unit: PriceUnit,
    },
}

impl fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogueError::Parse(m) => write!(f, "price catalogue unreadable: {m}"),
            CatalogueError::UnsupportedSchema(s) => {
                write!(f, "price catalogue schema {s} is not supported")
            }
            CatalogueError::InvalidRow { id, reason } => {
                write!(f, "invalid catalogue row '{id}': {reason}")
            }
            CatalogueError::Overlap { kind, id } => {
                write!(f, "catalogue rows for {kind:?} '{id}' overlap in time")
            }
            CatalogueError::Conflict { kind, id } => {
                write!(f, "catalogue merge conflict for {kind:?} '{id}'")
            }
            CatalogueError::BadDate(d) => write!(f, "'{d}' is not a YYYY-MM-DD date"),
            CatalogueError::Unpriced { kind, id } => write!(f, "no {kind:?} price for '{id}'"),
            CatalogueError::NotInEffect { kind, id, date } => {
                write!(f, "no {kind:?} price for '{id}' in effect on {date}")
            }
            CatalogueError::BelowQualification {
                kind,
                id,
                have,
                need,
            } => write!(
                f,
                "{kind:?} price for '{id}' is {have:?}, below the required {need:?}"
            ),
            CatalogueError::MissingComponent { kind, id, unit } => {
                write!(f, "{kind:?} price for '{id}' has no {unit:?} component")
            }
        }
    }
}

impl std::error::Error for CatalogueError {}

/// A versioned set of [`CatalogueRow`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCatalogue {
    pub schema: u32,
    /// Names this exact price set (for example `2026.10.17-1`). A cost report records it.
    pub version: String,
    /// Day this version was published, `YYYY-MM-DD`.
    pub published: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    rows: Vec<CatalogueRow>,
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if !s.is_ascii() || b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| s[r].bytes().all(|c| c.is_ascii_digit());
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let (y, m, d): (u32, u32, u32) = match (s[0..4].parse(), s[5..7].parse(), s[8..10].parse()) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return false,
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

fn id_key(kind: PriceKind, id: &str) -> String {
    match kind {
        PriceKind::Llm => normalize(id),
        _ => id.trim().to_lowercase(),
    }
}

impl PriceCatalogue {
    /// The catalogue shipped with this crate: the LLM rows of the bundled price table.
    pub fn bundled() -> Self {
        Self::from_json(BUNDLED).expect("bundled price catalogue is valid")
    }

    /// Read and validate a catalogue. Every date must be a real `YYYY-MM-DD` day, every
    /// amount finite and not negative, every row must name an id, a source and at least one
    /// component (no unit twice), `effective_until` must not precede `effective_from`, and
    /// rows for one kind and id must not overlap in time.
    pub fn from_json(json: &str) -> Result<Self, CatalogueError> {
        let catalogue: PriceCatalogue =
            serde_json::from_str(json).map_err(|e| CatalogueError::Parse(e.to_string()))?;
        catalogue.validate()?;
        Ok(catalogue)
    }

    /// Serialize to the JSON [`PriceCatalogue::from_json`] reads.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("catalogue serializes")
    }

    fn validate(&self) -> Result<(), CatalogueError> {
        if self.schema != CATALOGUE_SCHEMA {
            return Err(CatalogueError::UnsupportedSchema(self.schema));
        }
        let bad = |id: &str, reason: &str| CatalogueError::InvalidRow {
            id: id.to_owned(),
            reason: reason.to_owned(),
        };
        if self.version.trim().is_empty() {
            return Err(bad("", "catalogue version is empty"));
        }
        if !is_date(&self.published) {
            return Err(bad("", "published is not a YYYY-MM-DD date"));
        }
        for r in &self.rows {
            if r.id.trim().is_empty() {
                return Err(bad(&r.id, "id is empty"));
            }
            if r.source.trim().is_empty() {
                return Err(bad(&r.id, "source is empty"));
            }
            if !is_date(&r.effective_from) {
                return Err(bad(&r.id, "effective_from is not a YYYY-MM-DD date"));
            }
            if let Some(until) = &r.effective_until {
                if !is_date(until) {
                    return Err(bad(&r.id, "effective_until is not a YYYY-MM-DD date"));
                }
                if until.as_str() < r.effective_from.as_str() {
                    return Err(bad(&r.id, "effective_until precedes effective_from"));
                }
            }
            if let Some(c) = &r.checked_on {
                if !is_date(c) {
                    return Err(bad(&r.id, "checked_on is not a YYYY-MM-DD date"));
                }
            }
            if r.components.is_empty() {
                return Err(bad(&r.id, "no price components"));
            }
            for (i, c) in r.components.iter().enumerate() {
                if !c.amount.is_finite() || c.amount < 0.0 {
                    return Err(bad(&r.id, "amount must be finite and not negative"));
                }
                if r.components[..i].iter().any(|p| p.unit == c.unit) {
                    return Err(bad(&r.id, "a unit is quoted twice"));
                }
            }
        }
        for (i, a) in self.rows.iter().enumerate() {
            for b in &self.rows[i + 1..] {
                if a.kind == b.kind
                    && id_key(a.kind, &a.id) == id_key(b.kind, &b.id)
                    && windows_overlap(a, b)
                {
                    return Err(CatalogueError::Overlap {
                        kind: a.kind,
                        id: a.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn rows(&self) -> &[CatalogueRow] {
        &self.rows
    }

    /// Add the rows of `other`, keeping this catalogue's `schema`, `published` and `notes`
    /// and naming the result `"{self.version}+{other.version}"`. A row identical to one here
    /// is kept once; a row with the same kind, id and start date but different content is
    /// [`CatalogueError::Conflict`]; rows whose windows overlap are
    /// [`CatalogueError::Overlap`]. Nothing is replaced.
    pub fn merge(mut self, other: PriceCatalogue) -> Result<Self, CatalogueError> {
        for row in other.rows {
            if let Some(same) = self.rows.iter().find(|r| {
                r.kind == row.kind
                    && id_key(r.kind, &r.id) == id_key(row.kind, &row.id)
                    && r.effective_from == row.effective_from
            }) {
                if *same == row {
                    continue;
                }
                return Err(CatalogueError::Conflict {
                    kind: row.kind,
                    id: row.id,
                });
            }
            self.rows.push(row);
        }
        self.version = format!("{}+{}", self.version, other.version);
        self.validate()?;
        Ok(self)
    }

    /// The row for `kind` and `id` in effect on `date`, trusted at least `min`.
    ///
    /// Errors, in the order they are checked: [`CatalogueError::BadDate`],
    /// [`CatalogueError::Unpriced`] (no row for the id), [`CatalogueError::NotInEffect`] (rows
    /// exist, none cover `date`), [`CatalogueError::BelowQualification`].
    pub fn price_at(
        &self,
        kind: PriceKind,
        id: &str,
        date: &str,
        min: Qualification,
    ) -> Result<&CatalogueRow, CatalogueError> {
        if !is_date(date) {
            return Err(CatalogueError::BadDate(date.to_owned()));
        }
        let key = id_key(kind, id);
        let mut any = false;
        let mut found = None;
        for r in &self.rows {
            if r.kind == kind && id_key(r.kind, &r.id) == key {
                any = true;
                if r.applies_on(date) {
                    found = Some(r);
                }
            }
        }
        let row = match (any, found) {
            (false, _) => {
                return Err(CatalogueError::Unpriced {
                    kind,
                    id: id.to_owned(),
                })
            }
            (true, None) => {
                return Err(CatalogueError::NotInEffect {
                    kind,
                    id: id.to_owned(),
                    date: date.to_owned(),
                })
            }
            (true, Some(r)) => r,
        };
        if row.qualification < min {
            return Err(CatalogueError::BelowQualification {
                kind,
                id: id.to_owned(),
                have: row.qualification,
                need: min,
            });
        }
        Ok(row)
    }

    /// USD for one model call's `usage` on `date`. Input and output components are required;
    /// a cache component is required only when `usage` has cache tokens of that kind (there
    /// is no default multiplier here, unlike [`PriceTable`]).
    pub fn llm_cost_at(
        &self,
        model: &str,
        usage: &Usage,
        date: &str,
        min: Qualification,
    ) -> Result<f64, CatalogueError> {
        let row = self.price_at(PriceKind::Llm, model, date, min)?;
        let need = |unit: PriceUnit| {
            row.amount(unit)
                .ok_or_else(|| CatalogueError::MissingComponent {
                    kind: row.kind,
                    id: row.id.clone(),
                    unit,
                })
        };
        let per = |tokens: u64, price: f64| tokens as f64 / 1_000_000.0 * price;
        let mut usd = per(usage.input_tokens, need(PriceUnit::PerMillionInputTokens)?)
            + per(usage.output_tokens, need(PriceUnit::PerMillionOutputTokens)?);
        if usage.cache_read_tokens > 0 {
            usd += per(
                usage.cache_read_tokens,
                need(PriceUnit::PerMillionCacheReadTokens)?,
            );
        }
        if usage.cache_write_tokens > 0 {
            usd += per(
                usage.cache_write_tokens,
                need(PriceUnit::PerMillionCacheWriteTokens)?,
            );
        }
        Ok(usd)
    }

    /// The LLM rows in effect on `date` and trusted at least `min`, as a [`PriceTable`]. A
    /// row without input and output components is skipped; cache multipliers are derived
    /// from cache components when the row has them.
    pub fn llm_table_at(
        &self,
        date: &str,
        min: Qualification,
    ) -> Result<PriceTable, CatalogueError> {
        if !is_date(date) {
            return Err(CatalogueError::BadDate(date.to_owned()));
        }
        let rows: Vec<ModelPrice> = self
            .rows
            .iter()
            .filter(|r| r.kind == PriceKind::Llm && r.applies_on(date) && r.qualification >= min)
            .filter_map(|r| {
                let input = r.amount(PriceUnit::PerMillionInputTokens)?;
                let output = r.amount(PriceUnit::PerMillionOutputTokens)?;
                let mult = |unit| {
                    r.amount(unit)
                        .filter(|_| input > 0.0)
                        .map(|a| a / input)
                };
                Some(ModelPrice {
                    slug: r.id.clone(),
                    input_per_m: input,
                    output_per_m: output,
                    context_window: r.context_window,
                    cache_read_multiplier: mult(PriceUnit::PerMillionCacheReadTokens),
                    cache_write_multiplier: mult(PriceUnit::PerMillionCacheWriteTokens),
                })
            })
            .collect();
        PriceTable::from_rows(rows).map_err(|e: PriceError| CatalogueError::InvalidRow {
            id: String::new(),
            reason: e.to_string(),
        })
    }
}

fn windows_overlap(a: &CatalogueRow, b: &CatalogueRow) -> bool {
    let a_until = a.effective_until.as_deref();
    let b_until = b.effective_until.as_deref();
    let a_starts_before_b_ends = b_until.is_none_or(|u| a.effective_from.as_str() <= u);
    let b_starts_before_a_ends = a_until.is_none_or(|u| b.effective_from.as_str() <= u);
    a_starts_before_b_ends && b_starts_before_a_ends
}
