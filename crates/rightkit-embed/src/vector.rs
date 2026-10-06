use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use thiserror::Error;

pub const DEFAULT_VECTOR_DIM: usize = 256;
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rightkit_vectors (
    key TEXT PRIMARY KEY NOT NULL,
    model_id TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    vector BLOB NOT NULL
);";

#[derive(Debug, Error)]
pub enum VectorError {
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("vector dimension must be {expected}, got {actual}")]
    Dimension { expected: usize, actual: usize },
    #[error("vector contains non-finite values")]
    NonFinite,
    #[error("stored vector for {key} is malformed")]
    Malformed { key: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Nearest {
    pub key: String,
    pub score: f32,
    pub vector: Vec<f32>,
}

pub struct VectorStore {
    conn: Connection,
    dimension: usize,
}

impl VectorStore {
    pub fn open(path: impl AsRef<Path>, dimension: usize) -> Result<Self, VectorError> {
        Self::from_connection(Connection::open(path)?, dimension)
    }

    pub fn in_memory(dimension: usize) -> Result<Self, VectorError> {
        Self::from_connection(Connection::open_in_memory()?, dimension)
    }

    pub fn from_connection(conn: Connection, dimension: usize) -> Result<Self, VectorError> {
        if dimension == 0 {
            return Err(VectorError::Dimension {
                expected: 1,
                actual: 0,
            });
        }
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, dimension })
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn len(&self) -> Result<usize, VectorError> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM rightkit_vectors", [], |row| {
                    row.get(0)
                })?;
        Ok(count as usize)
    }

    pub fn is_empty(&self) -> Result<bool, VectorError> {
        Ok(self.len()? == 0)
    }

    pub fn put(&self, key: &str, model_id: &str, vector: &[f32]) -> Result<(), VectorError> {
        validate(vector, self.dimension)?;
        self.conn.execute(
            "INSERT INTO rightkit_vectors(key, model_id, dimensions, vector)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET model_id=excluded.model_id,
             dimensions=excluded.dimensions, vector=excluded.vector",
            params![key, model_id, self.dimension as i64, encode(vector)],
        )?;
        Ok(())
    }

    pub fn put_batch<'a, I>(&mut self, rows: I) -> Result<(), VectorError>
    where
        I: IntoIterator<Item = (&'a str, &'a str, &'a [f32])>,
    {
        let tx = self.conn.transaction()?;
        for (key, model_id, vector) in rows {
            validate(vector, self.dimension)?;
            tx.execute(
                "INSERT INTO rightkit_vectors(key, model_id, dimensions, vector)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(key) DO UPDATE SET model_id=excluded.model_id,
                 dimensions=excluded.dimensions, vector=excluded.vector",
                params![key, model_id, self.dimension as i64, encode(vector)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<Nearest>, VectorError> {
        let row = self
            .conn
            .query_row(
                "SELECT dimensions, vector FROM rightkit_vectors WHERE key=?1",
                params![key],
                |row| {
                    let dimensions: i64 = row.get(0)?;
                    let bytes: Vec<u8> = row.get(1)?;
                    Ok((dimensions, bytes))
                },
            )
            .optional()?;
        row.map(|(dimensions, bytes)| {
            if dimensions != self.dimension as i64 {
                return Err(VectorError::Malformed {
                    key: key.to_owned(),
                });
            }
            let vector = decode(&bytes, self.dimension).map_err(|_| VectorError::Malformed {
                key: key.to_owned(),
            })?;
            Ok(Nearest {
                key: key.to_owned(),
                score: 1.0,
                vector,
            })
        })
        .transpose()
    }

    pub fn nearest(
        &self,
        model_id: &str,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<Nearest>, VectorError> {
        validate(query, self.dimension)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT key, dimensions, vector FROM rightkit_vectors
             WHERE model_id=?1 ORDER BY key",
        )?;
        let rows = stmt.query_map(params![model_id], |row| {
            let key: String = row.get(0)?;
            let dimensions: i64 = row.get(1)?;
            let bytes: Vec<u8> = row.get(2)?;
            Ok((key, dimensions, bytes))
        })?;
        let mut candidates = Vec::new();
        for row in rows {
            let (key, dimensions, bytes) = row?;
            if dimensions != self.dimension as i64 {
                return Err(VectorError::Malformed { key });
            }
            let vector = decode(&bytes, self.dimension)
                .map_err(|_| VectorError::Malformed { key: key.clone() })?;
            candidates.push(Nearest {
                score: cosine_similarity(query, &vector),
                key,
                vector,
            });
        }
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
        candidates.truncate(limit);
        Ok(candidates)
    }

    pub fn remove(&self, key: &str) -> Result<bool, VectorError> {
        Ok(self
            .conn
            .execute("DELETE FROM rightkit_vectors WHERE key=?1", params![key])?
            != 0)
    }

    pub fn clear_model(&self, model_id: &str) -> Result<usize, VectorError> {
        Ok(self.conn.execute(
            "DELETE FROM rightkit_vectors WHERE model_id=?1",
            params![model_id],
        )?)
    }
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (&a, &b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

fn validate(vector: &[f32], dimension: usize) -> Result<(), VectorError> {
    if vector.len() != dimension {
        return Err(VectorError::Dimension {
            expected: dimension,
            actual: vector.len(),
        });
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(VectorError::NonFinite);
    }
    Ok(())
}

fn encode(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn decode(bytes: &[u8], dimension: usize) -> Result<Vec<f32>, ()> {
    if bytes.len() != dimension * std::mem::size_of::<f32>() {
        return Err(());
    }
    let mut vector = Vec::with_capacity(dimension);
    for chunk in bytes.chunks_exact(4) {
        vector.push(f32::from_le_bytes(chunk.try_into().map_err(|_| ())?));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(());
    }
    Ok(vector)
}
