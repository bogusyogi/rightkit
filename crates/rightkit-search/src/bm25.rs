//! In-memory Okapi BM25 ranking over documents the caller holds, for when no FTS5 table exists.
//!
//! Tokens are maximal runs of Unicode alphanumeric characters, lower-cased. Each query term
//! counts once. `score = idf * tf * (k1 + 1) / (tf + k1 * (1 - b + b * len / avg_len))` with
//! `idf = ln(1 + (N - n + 0.5) / (n + 0.5))`. Hits sort by score, highest first, then by id, so
//! equal scores always come back in the same order.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Default term-frequency saturation.
pub const DEFAULT_K1: f64 = 1.2;
/// Default length normalisation.
pub const DEFAULT_B: f64 = 0.75;

/// Why a document could not be indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bm25Error {
    /// The id is already indexed; remove it first to replace it.
    DuplicateId(String),
    /// `k1` must be finite and non-negative, and `b` finite and within `0..=1`.
    BadParameters,
}

impl fmt::Display for Bm25Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "document id is already indexed: {id:?}"),
            Self::BadParameters => write!(f, "BM25 needs finite k1 >= 0 and b in 0..=1"),
        }
    }
}

impl std::error::Error for Bm25Error {}

/// One ranked document.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: String,
    pub score: f64,
}

#[derive(Debug, Clone)]
struct Doc {
    id: String,
    len: usize,
    terms: HashMap<String, u32>,
}

/// A BM25 index over caller-supplied documents, in memory.
#[derive(Debug, Clone)]
pub struct Bm25Index {
    k1: f64,
    b: f64,
    docs: Vec<Doc>,
    /// Number of documents each term appears in.
    df: HashMap<String, usize>,
    total_len: usize,
}

/// Lower-case Unicode alphanumeric runs of `text`.
pub fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
}

impl Default for Bm25Index {
    fn default() -> Self {
        Self {
            k1: DEFAULT_K1,
            b: DEFAULT_B,
            docs: Vec::new(),
            df: HashMap::new(),
            total_len: 0,
        }
    }
}

impl Bm25Index {
    /// An empty index with the given `k1` and `b`.
    pub fn with_parameters(k1: f64, b: f64) -> Result<Self, Bm25Error> {
        if !k1.is_finite() || k1 < 0.0 || !b.is_finite() || !(0.0..=1.0).contains(&b) {
            return Err(Bm25Error::BadParameters);
        }
        Ok(Self {
            k1,
            b,
            ..Self::default()
        })
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// True when no document is indexed.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Index `text` under `id`. A document with no tokens is indexed with length zero and can
    /// never score.
    pub fn insert(&mut self, id: impl Into<String>, text: &str) -> Result<(), Bm25Error> {
        let id = id.into();
        if self.docs.iter().any(|d| d.id == id) {
            return Err(Bm25Error::DuplicateId(id));
        }
        let mut terms: HashMap<String, u32> = HashMap::new();
        let mut len = 0usize;
        for token in tokenize(text) {
            len += 1;
            *terms.entry(token).or_insert(0) += 1;
        }
        for term in terms.keys() {
            *self.df.entry(term.clone()).or_insert(0) += 1;
        }
        self.total_len += len;
        self.docs.push(Doc { id, len, terms });
        Ok(())
    }

    /// Remove the document `id`. Returns `false` when it is not indexed.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(pos) = self.docs.iter().position(|d| d.id == id) else {
            return false;
        };
        let doc = self.docs.remove(pos);
        for term in doc.terms.keys() {
            if let Some(count) = self.df.get_mut(term) {
                *count -= 1;
                if *count == 0 {
                    self.df.remove(term);
                }
            }
        }
        self.total_len -= doc.len;
        true
    }

    /// The `limit` best documents for `query`, highest score first. Documents that share no
    /// term with the query are not returned. A `limit` of zero returns nothing.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        if limit == 0 || self.docs.is_empty() {
            return Vec::new();
        }
        let terms: HashSet<String> = tokenize(query).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let n = self.docs.len() as f64;
        let avg_len = self.total_len as f64 / n;
        let mut hits: Vec<Hit> = Vec::new();
        for doc in &self.docs {
            let mut score = 0.0;
            for term in &terms {
                let Some(&tf) = doc.terms.get(term) else {
                    continue;
                };
                let df = self.df.get(term).copied().unwrap_or(0) as f64;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let tf = f64::from(tf);
                let norm = if avg_len > 0.0 {
                    1.0 - self.b + self.b * (doc.len as f64 / avg_len)
                } else {
                    1.0
                };
                score += idf * tf * (self.k1 + 1.0) / (tf + self.k1 * norm);
            }
            if score > 0.0 {
                hits.push(Hit {
                    id: doc.id.clone(),
                    score,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        hits.truncate(limit);
        hits
    }
}
