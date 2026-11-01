use crate::error::Result;
use crate::Storage;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

/// A persisted memory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRow {
    pub id: String,
    pub tier: String,
    pub content: String,
    pub score: f64,
    pub created_at: String,
    pub meta_json: String,
}

impl Storage {
    /// Insert or replace a memory record.
    pub fn write_memory(&self, row: &MemoryRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO memory \
             (id, tier, content, score, created_at, meta_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                &row.id,
                &row.tier,
                &row.content,
                row.score,
                &row.created_at,
                &row.meta_json,
            ),
        )?;
        Ok(())
    }

    /// Fetch a single memory record by id.
    pub fn get_memory(&self, id: &str) -> Result<Option<MemoryRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, tier, content, score, created_at, meta_json \
                 FROM memory WHERE id = ?1",
                [id],
                Self::map_memory,
            )
            .optional()?;
        Ok(row)
    }

    /// All memory records in a tier, ordered by score descending.
    pub fn query_memory_by_tier(&self, tier: &str) -> Result<Vec<MemoryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tier, content, score, created_at, meta_json \
             FROM memory WHERE tier = ?1 ORDER BY score DESC",
        )?;
        let rows = stmt.query_map([tier], Self::map_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Top-N memory records across all tiers, ordered by score descending.
    pub fn top_memories(&self, limit: i64) -> Result<Vec<MemoryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tier, content, score, created_at, meta_json \
             FROM memory ORDER BY score DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], Self::map_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn map_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRow> {
        Ok(MemoryRow {
            id: row.get(0)?,
            tier: row.get(1)?,
            content: row.get(2)?,
            score: row.get(3)?,
            created_at: row.get(4)?,
            meta_json: row.get(5)?,
        })
    }
}
