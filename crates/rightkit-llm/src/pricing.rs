//! Model pricing (USD per 1M tokens). The bundled table is donor data from
//! CodeRight `model-metadata.toml` (several rows are documented there as
//! doc-asserted rather than verified); apps may merge an override table.
//! Unknown models yield `None` instead of a silent zero.

use serde::Deserialize;

use crate::types::Usage;

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

#[derive(Debug, Clone, PartialEq)]
pub struct PriceTable {
    rows: Vec<ModelPrice>,
}

/// Lower-case, drop a `vendor/` prefix, and treat `.` and `_` like `-`.
fn normalize(slug: &str) -> String {
    let s = slug.rsplit('/').next().unwrap_or(slug).to_lowercase();
    s.replace(['.', '_'], "-")
}

impl PriceTable {
    pub fn bundled() -> Self {
        Self::from_json(BUNDLED).expect("bundled price table is valid")
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str::<Vec<ModelPrice>>(json)
            .map(|rows| Self { rows })
            .map_err(|e| e.to_string())
    }

    /// Rows in `overrides` replace same-slug rows here and add new ones.
    pub fn merged_with(mut self, overrides: PriceTable) -> Self {
        for row in overrides.rows {
            let key = normalize(&row.slug);
            self.rows.retain(|r| normalize(&r.slug) != key);
            self.rows.push(row);
        }
        self
    }

    /// Exact normalized match, else the longest row whose slug is a prefix of
    /// the model (`claude-sonnet-5-5` matches `claude-sonnet-5`).
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        let m = normalize(model);
        if let Some(r) = self.rows.iter().find(|r| normalize(&r.slug) == m) {
            return Some(r);
        }
        self.rows
            .iter()
            .filter(|r| m.starts_with(&normalize(&r.slug)))
            .max_by_key(|r| normalize(&r.slug).len())
    }

    /// USD cost of `usage`, or `None` for an unpriced model.
    pub fn cost(&self, model: &str, usage: &Usage) -> Option<f64> {
        let p = self.lookup(model)?;
        let per = |tokens: u64, price: f64| tokens as f64 / 1_000_000.0 * price;
        Some(
            per(usage.input_tokens, p.input_per_m)
                + per(usage.output_tokens, p.output_per_m)
                + per(usage.cache_read_tokens, p.input_per_m * p.read_mult())
                + per(usage.cache_write_tokens, p.input_per_m * p.write_mult()),
        )
    }

    /// USD cost of a completed response, or `None` when no price applies: the
    /// route is subscription-metered, its usage is unknown, or the model is
    /// unpriced. Prefer this over [`PriceTable::cost`] for provider responses.
    pub fn cost_of(&self, response: &crate::types::ChatResponse) -> Option<f64> {
        if response.metering != crate::types::Metering::PerToken || !response.usage_known {
            return None;
        }
        self.cost(&response.model, &response.usage)
    }

    /// USD saved by cache reads versus paying the full input price.
    pub fn cache_savings(&self, model: &str, usage: &Usage) -> Option<f64> {
        let p = self.lookup(model)?;
        Some(usage.cache_read_tokens as f64 / 1_000_000.0 * p.input_per_m * (1.0 - p.read_mult()))
    }

    pub fn context_window(&self, model: &str) -> Option<u64> {
        self.lookup(model)?.context_window
    }
}
