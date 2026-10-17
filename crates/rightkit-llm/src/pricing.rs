//! Model pricing (USD per 1M tokens). The bundled table is donor data from
//! CodeRight `model-metadata.toml` (several rows are documented there as
//! doc-asserted rather than verified). For rows with a version, a date and a
//! qualification, see [`crate::catalogue`].
//!
//! Two rules keep a paid route from running on a guessed price:
//!
//! - A lookup is an exact match on the normalized slug. A prefix match
//!   (`claude-sonnet-5-5` priced as `claude-sonnet-5`) happens only when the caller asks for
//!   it with [`Match::LongestPrefix`] or [`PriceTable::lookup_prefix`].
//! - Merging two tables never replaces a row silently. [`PriceTable::merge`] fails with
//!   [`PriceError::Conflict`] on a same-slug row that differs; [`PriceTable::merge_overriding`]
//!   replaces on purpose and returns the list of rows it replaced.
//!
//! An unpriced model is `None` from the `Option` methods and [`PriceError::Unpriced`] from the
//! `try_` methods, which a host uses to block a paid route. It is never a silent zero.

use std::fmt;

use serde::Deserialize;

use crate::types::{ChatResponse, Metering, Usage};

const BUNDLED: &str = include_str!("../assets/model-prices.json");

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelPrice {
    pub slug: String,
    pub input_per_m: f64,
    pub output_per_m: f64,
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Cache-read price as a fraction of `input_per_m` (default 0.1).
    #[serde(default)]
    pub cache_read_multiplier: Option<f64>,
    /// Cache-write price as a fraction of `input_per_m` (default 1.25 for Claude, else 1.0).
    #[serde(default)]
    pub cache_write_multiplier: Option<f64>,
}

impl ModelPrice {
    fn read_mult(&self) -> f64 {
        self.cache_read_multiplier.unwrap_or(0.1)
    }
    fn write_mult(&self) -> f64 {
        self.cache_write_multiplier
            .unwrap_or(if normalize(&self.slug).starts_with("claude") {
                1.25
            } else {
                1.0
            })
    }
}

/// How a model name is matched against the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Match {
    /// The normalized slug equals a row's normalized slug. The default.
    #[default]
    Exact,
    /// Exact match first, else the longest row whose slug is a prefix of the model. A caller
    /// chooses this knowing the price may belong to a different model version.
    LongestPrefix,
}

/// A pricing failure the caller can act on.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PriceError {
    /// No row for the model under the requested [`Match`]. Block the paid route.
    Unpriced { model: String },
    /// A merge met a same-slug row with different values.
    Conflict {
        slug: String,
        existing: ModelPrice,
        incoming: ModelPrice,
    },
    /// A table holds two rows whose normalized slugs are equal.
    Duplicate { slug: String },
    /// A row has a negative or non-finite price or multiplier.
    Invalid { slug: String, reason: String },
    /// The JSON could not be read.
    Parse(String),
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PriceError::Unpriced { model } => write!(f, "no price for model '{model}'"),
            PriceError::Conflict { slug, .. } => {
                write!(f, "price row '{slug}' exists with different values")
            }
            PriceError::Duplicate { slug } => write!(f, "duplicate price row '{slug}'"),
            PriceError::Invalid { slug, reason } => {
                write!(f, "invalid price row '{slug}': {reason}")
            }
            PriceError::Parse(m) => write!(f, "price table unreadable: {m}"),
        }
    }
}

impl std::error::Error for PriceError {}

/// One row a [`PriceTable::merge_overriding`] replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceOverride {
    pub replaced: ModelPrice,
    pub with: ModelPrice,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriceTable {
    rows: Vec<ModelPrice>,
}

/// Lower-case, drop a `vendor/` prefix, and treat `.` and `_` like `-`.
fn normalize(slug: &str) -> String {
    let s = slug.rsplit('/').next().unwrap_or(slug).to_lowercase();
    s.replace(['.', '_'], "-")
}

fn validate_rows(rows: &[ModelPrice]) -> Result<(), PriceError> {
    let mut seen: Vec<String> = Vec::new();
    for r in rows {
        let bad = |what: &str, v: f64| {
            (!v.is_finite() || v < 0.0).then(|| PriceError::Invalid {
                slug: r.slug.clone(),
                reason: format!("{what} must be finite and not negative"),
            })
        };
        if let Some(e) = bad("input_per_m", r.input_per_m)
            .or_else(|| bad("output_per_m", r.output_per_m))
            .or_else(|| {
                r.cache_read_multiplier
                    .and_then(|v| bad("cache_read_multiplier", v))
            })
            .or_else(|| {
                r.cache_write_multiplier
                    .and_then(|v| bad("cache_write_multiplier", v))
            })
        {
            return Err(e);
        }
        let key = normalize(&r.slug);
        if seen.contains(&key) {
            return Err(PriceError::Duplicate {
                slug: r.slug.clone(),
            });
        }
        seen.push(key);
    }
    Ok(())
}

impl PriceTable {
    pub fn bundled() -> Self {
        Self::from_json(BUNDLED).expect("bundled price table is valid")
    }

    /// Read rows from JSON. Rejects duplicate normalized slugs and negative or non-finite
    /// prices.
    pub fn from_json(json: &str) -> Result<Self, PriceError> {
        let rows = serde_json::from_str::<Vec<ModelPrice>>(json)
            .map_err(|e| PriceError::Parse(e.to_string()))?;
        validate_rows(&rows)?;
        Ok(Self { rows })
    }

    /// The rows, in table order.
    pub fn rows(&self) -> &[ModelPrice] {
        &self.rows
    }

    /// Add the rows of `other`. A model that is new is added; a same-slug row identical to
    /// the one here is accepted; a same-slug row that differs fails with
    /// [`PriceError::Conflict`] and nothing is merged. Use [`PriceTable::merge_overriding`]
    /// to replace on purpose.
    pub fn merge(mut self, other: PriceTable) -> Result<Self, PriceError> {
        for row in &other.rows {
            let key = normalize(&row.slug);
            if let Some(existing) = self.rows.iter().find(|r| normalize(&r.slug) == key) {
                if existing != row {
                    return Err(PriceError::Conflict {
                        slug: row.slug.clone(),
                        existing: existing.clone(),
                        incoming: row.clone(),
                    });
                }
            }
        }
        for row in other.rows {
            let key = normalize(&row.slug);
            if !self.rows.iter().any(|r| normalize(&r.slug) == key) {
                self.rows.push(row);
            }
        }
        Ok(self)
    }

    /// Rows in `overrides` replace same-slug rows here and add new ones, on purpose. Returns
    /// the merged table and one [`PriceOverride`] per row that was replaced with different
    /// values, so the caller can log or refuse them.
    pub fn merge_overriding(mut self, overrides: PriceTable) -> (Self, Vec<PriceOverride>) {
        let mut replaced = Vec::new();
        for row in overrides.rows {
            let key = normalize(&row.slug);
            if let Some(i) = self.rows.iter().position(|r| normalize(&r.slug) == key) {
                let old = self.rows.remove(i);
                if old != row {
                    replaced.push(PriceOverride {
                        replaced: old,
                        with: row.clone(),
                    });
                }
            }
            self.rows.push(row);
        }
        (self, replaced)
    }

    /// Exact match on the normalized slug (case, a `vendor/` prefix, `.` and `_` are
    /// ignored). No prefix fallback: `claude-sonnet-5-5` is not priced as `claude-sonnet-5`.
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        let m = normalize(model);
        self.rows.iter().find(|r| normalize(&r.slug) == m)
    }

    /// Exact match, else the longest row whose slug is a prefix of the model. Opt-in: the
    /// price found may belong to another version of the model.
    pub fn lookup_prefix(&self, model: &str) -> Option<&ModelPrice> {
        if let Some(r) = self.lookup(model) {
            return Some(r);
        }
        let m = normalize(model);
        self.rows
            .iter()
            .filter(|r| m.starts_with(&normalize(&r.slug)))
            .max_by_key(|r| normalize(&r.slug).len())
    }

    /// The row for `model` under `how`, or [`PriceError::Unpriced`].
    pub fn require(&self, model: &str, how: Match) -> Result<&ModelPrice, PriceError> {
        let found = match how {
            Match::Exact => self.lookup(model),
            Match::LongestPrefix => self.lookup_prefix(model),
        };
        found.ok_or_else(|| PriceError::Unpriced {
            model: model.to_owned(),
        })
    }

    fn cost_with(p: &ModelPrice, usage: &Usage) -> f64 {
        let per = |tokens: u64, price: f64| tokens as f64 / 1_000_000.0 * price;
        per(usage.input_tokens, p.input_per_m)
            + per(usage.output_tokens, p.output_per_m)
            + per(usage.cache_read_tokens, p.input_per_m * p.read_mult())
            + per(usage.cache_write_tokens, p.input_per_m * p.write_mult())
    }

    /// USD cost of `usage` by exact match, or `None` for an unpriced model.
    pub fn cost(&self, model: &str, usage: &Usage) -> Option<f64> {
        self.lookup(model).map(|p| Self::cost_with(p, usage))
    }

    /// USD cost of `usage`, or [`PriceError::Unpriced`] when no row matches under `how`.
    pub fn try_cost(&self, model: &str, usage: &Usage, how: Match) -> Result<f64, PriceError> {
        self.require(model, how).map(|p| Self::cost_with(p, usage))
    }

    /// USD cost of a completed response by exact match, or `None` when no price applies:
    /// the route is subscription-metered, its usage is unknown, or the model is unpriced.
    /// A paid route that must not run unpriced uses [`PriceTable::try_cost_of`].
    pub fn cost_of(&self, response: &ChatResponse) -> Option<f64> {
        if response.metering != Metering::PerToken || !response.usage_known {
            return None;
        }
        self.cost(&response.model, &response.usage)
    }

    /// Cost of a response with the unpriced case typed. A per-token response whose model has
    /// no row under `how` is [`PriceError::Unpriced`], even when its usage is unknown, so the
    /// route can be blocked. `Ok(None)` means no price applies (subscription-metered, or a
    /// priced model whose usage was not reported).
    pub fn try_cost_of(
        &self,
        response: &ChatResponse,
        how: Match,
    ) -> Result<Option<f64>, PriceError> {
        if response.metering != Metering::PerToken {
            return Ok(None);
        }
        let p = self.require(&response.model, how)?;
        Ok(response
            .usage_known
            .then(|| Self::cost_with(p, &response.usage)))
    }

    /// USD saved by cache reads versus paying the full input price (exact match).
    pub fn cache_savings(&self, model: &str, usage: &Usage) -> Option<f64> {
        let p = self.lookup(model)?;
        Some(usage.cache_read_tokens as f64 / 1_000_000.0 * p.input_per_m * (1.0 - p.read_mult()))
    }

    /// Context window of the exactly matched row.
    pub fn context_window(&self, model: &str) -> Option<u64> {
        self.lookup(model)?.context_window
    }
}
