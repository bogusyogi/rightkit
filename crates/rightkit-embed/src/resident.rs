//! Resident int8 vector index: one model's vectors held in memory, quantised per row.
//!
//! Each row is stored as `i8` codes with one `f32` scale (`absmax / 127`), so the index costs
//! about a quarter of the f32 matrix. Scores are cosine similarities against the dequantised
//! rows, so the only error is the quantisation of each row. Built from a [`VectorStore`]
//! snapshot with [`VectorStore::resident_index`]; later writes to the store are not seen.
//!
//! [`VectorStore`]: crate::VectorStore

use crate::vector::{validate, Nearest, VectorError};
use std::cmp::Ordering;

/// An in-memory int8 snapshot of the vectors of one model. See the module documentation.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentIndex {
    dimension: usize,
    /// Keys, ascending; the row order of `codes`, `scales` and `norms`.
    keys: Vec<String>,
    /// Row-major `i8` codes, `keys.len() * dimension` values.
    codes: Vec<i8>,
    /// Per row: `absmax / 127`, or 0 for an all-zero row.
    scales: Vec<f32>,
    /// Per row: the L2 norm of the dequantised row, in f64 so large finite scales cannot overflow.
    norms: Vec<f64>,
}

impl ResidentIndex {
    /// Quantises `vectors` (key and vector pairs, keys unique). Each vector must have
    /// `dimension` finite values.
    pub(crate) fn from_vectors(
        dimension: usize,
        mut vectors: Vec<(String, Vec<f32>)>,
    ) -> Result<Self, VectorError> {
        vectors.sort_by(|left, right| left.0.cmp(&right.0));
        let mut index = Self {
            dimension,
            keys: Vec::with_capacity(vectors.len()),
            codes: Vec::with_capacity(vectors.len() * dimension),
            scales: Vec::with_capacity(vectors.len()),
            norms: Vec::with_capacity(vectors.len()),
        };
        for (key, vector) in vectors {
            validate(&vector, dimension)?;
            let absmax = vector
                .iter()
                .fold(0.0_f32, |max, value| max.max(value.abs()));
            let scale = absmax / 127.0;
            let mut sum_squares = 0.0_f64;
            if scale > 0.0 {
                for value in &vector {
                    let code = (value / scale).round().clamp(-127.0, 127.0) as i8;
                    index.codes.push(code);
                    sum_squares += f64::from(code) * f64::from(code);
                }
            } else {
                index.codes.resize(index.codes.len() + dimension, 0);
            }
            index.scales.push(scale);
            index.norms.push(f64::from(scale) * sum_squares.sqrt());
            index.keys.push(key);
        }
        Ok(index)
    }

    /// Vector width of every row.
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when the index holds no rows.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The `limit` rows with the highest cosine similarity to `query`, best first. Ties are
    /// ordered by key, as [`VectorStore::nearest`] orders them. A zero query or zero row scores
    /// 0. `Nearest::vector` is the dequantised row.
    ///
    /// [`VectorStore::nearest`]: crate::VectorStore::nearest
    pub fn nearest(&self, query: &[f32], limit: usize) -> Result<Vec<Nearest>, VectorError> {
        validate(query, self.dimension)?;
        if limit == 0 || self.keys.is_empty() {
            return Ok(Vec::new());
        }
        let query_norm = query
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        let mut scored: Vec<(usize, f32)> = (0..self.keys.len())
            .map(|row| {
                let codes = &self.codes[row * self.dimension..(row + 1) * self.dimension];
                let dot: f64 = query
                    .iter()
                    .zip(codes)
                    .map(|(value, code)| f64::from(*value) * f64::from(*code))
                    .sum();
                let denominator = query_norm * self.norms[row];
                let score = if denominator > 0.0 {
                    (dot * f64::from(self.scales[row]) / denominator) as f32
                } else {
                    0.0
                };
                (row, score)
            })
            .collect();
        let rank = |left: &(usize, f32), right: &(usize, f32)| -> Ordering {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| self.keys[left.0].cmp(&self.keys[right.0]))
        };
        if limit < scored.len() {
            scored.select_nth_unstable_by(limit - 1, &rank);
            scored.truncate(limit);
        }
        scored.sort_by(&rank);
        Ok(scored
            .into_iter()
            .map(|(row, score)| Nearest {
                key: self.keys[row].clone(),
                score,
                vector: self.dequantise(row),
            })
            .collect())
    }

    fn dequantise(&self, row: usize) -> Vec<f32> {
        let scale = self.scales[row];
        self.codes[row * self.dimension..(row + 1) * self.dimension]
            .iter()
            .map(|code| f32::from(*code) * scale)
            .collect()
    }
}
