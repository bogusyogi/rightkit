//! Storage-native durable rows for the daemon's Adapt host.
//!
//! This module deliberately uses only strings and serialized receipt payloads;
//! engine proposal types stay outside the storage dependency graph.

use crate::{Result, Storage};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptGuidanceRow {
    pub guidance_id: String,
    pub kind: String,
    pub subject: String,
    pub content: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptGuidanceWrite {
    Applied(AdaptGuidanceRow),
    Missing,
    Stale { observed_digest: String },
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptApplicationReceiptRow {
    pub receipt_id: String,
    pub proposal_id: String,
    pub surface: String,
    pub subject: String,
    pub receipt_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptReceiptAppend {
    Inserted,
    Existing,
    Contradiction,
}

impl Storage {
    pub fn list_adapt_guidance(&self) -> Result<Vec<AdaptGuidanceRow>> {
        let mut statement = self.conn.prepare(
            "SELECT guidance_id, kind, subject, content, digest FROM adapt_guidance_records ORDER BY guidance_id",
        )?;
        let rows = statement
            .query_map([], row_to_guidance)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_adapt_guidance(&self, guidance_id: &str) -> Result<Option<AdaptGuidanceRow>> {
        self.conn
            .query_row(
                "SELECT guidance_id, kind, subject, content, digest FROM adapt_guidance_records WHERE guidance_id = ?1",
                [guidance_id],
                row_to_guidance,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Atomic record CAS.  A miss distinguishes an absent row from a stale
    /// digest so the daemon adapter can retain the engine's typed rejection.
    pub fn replace_adapt_guidance_if_unchanged(
        &self,
        guidance_id: &str,
        expected_digest: &str,
        new_content: &str,
    ) -> Result<AdaptGuidanceWrite> {
        let new_digest = content_digest(new_content);
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let changed = tx.execute(
            "UPDATE adapt_guidance_records SET content = ?1, digest = ?2 WHERE guidance_id = ?3 AND digest = ?4",
            params![new_content, new_digest, guidance_id, expected_digest],
        )?;
        if changed == 1 {
            let row = guidance_tx(&tx, guidance_id)?.expect("updated guidance exists");
            tx.commit()?;
            return Ok(AdaptGuidanceWrite::Applied(row));
        }
        let outcome = match guidance_tx(&tx, guidance_id)? {
            Some(row) => AdaptGuidanceWrite::Stale {
                observed_digest: row.digest,
            },
            None => AdaptGuidanceWrite::Missing,
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// Atomic surface CAS plus insert. The supplied digest must be the
    /// SHA-256 of sorted `guidance_id\\0digest\\n` lines for this kind.
    pub fn append_adapt_guidance_if_surface_unchanged(
        &self,
        expected_surface_digest: &str,
        row: AdaptGuidanceRow,
    ) -> Result<AdaptGuidanceWrite> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let observed = surface_digest_tx(&tx, &row.kind)?;
        if observed != expected_surface_digest {
            tx.commit()?;
            return Ok(AdaptGuidanceWrite::Stale {
                observed_digest: observed,
            });
        }
        if guidance_tx(&tx, &row.guidance_id)?.is_some() {
            tx.commit()?;
            return Ok(AdaptGuidanceWrite::Duplicate);
        }
        let stored = AdaptGuidanceRow {
            digest: content_digest(&row.content),
            ..row
        };
        tx.execute(
            "INSERT INTO adapt_guidance_records (guidance_id, kind, subject, content, digest) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&stored.guidance_id, &stored.kind, &stored.subject, &stored.content, &stored.digest],
        )?;
        tx.commit()?;
        Ok(AdaptGuidanceWrite::Applied(stored))
    }

    pub fn get_adapt_application_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<AdaptApplicationReceiptRow>> {
        self.conn
            .query_row(
                "SELECT receipt_id, proposal_id, surface, subject, receipt_json, created_at FROM adapt_application_receipts WHERE receipt_id = ?1",
                [receipt_id],
                row_to_receipt,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Idempotently records one exact receipt. Reusing an id with changed
    /// payload is a typed contradiction, never an overwrite.
    pub fn append_adapt_application_receipt(
        &self,
        row: &AdaptApplicationReceiptRow,
    ) -> Result<AdaptReceiptAppend> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if let Some(existing) = receipt_tx(&tx, &row.receipt_id)? {
            let outcome = if existing.proposal_id == row.proposal_id
                && existing.surface == row.surface
                && existing.subject == row.subject
                && existing.receipt_json == row.receipt_json
            {
                AdaptReceiptAppend::Existing
            } else {
                AdaptReceiptAppend::Contradiction
            };
            tx.commit()?;
            return Ok(outcome);
        }
        tx.execute(
            "INSERT INTO adapt_application_receipts (receipt_id, proposal_id, surface, subject, receipt_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![&row.receipt_id, &row.proposal_id, &row.surface, &row.subject, &row.receipt_json, &row.created_at],
        )?;
        tx.commit()?;
        Ok(AdaptReceiptAppend::Inserted)
    }
}

fn guidance_tx(tx: &Transaction<'_>, guidance_id: &str) -> Result<Option<AdaptGuidanceRow>> {
    tx.query_row(
        "SELECT guidance_id, kind, subject, content, digest FROM adapt_guidance_records WHERE guidance_id = ?1",
        [guidance_id],
        row_to_guidance,
    )
    .optional()
    .map_err(Into::into)
}

fn receipt_tx(
    tx: &Transaction<'_>,
    receipt_id: &str,
) -> Result<Option<AdaptApplicationReceiptRow>> {
    tx.query_row(
        "SELECT receipt_id, proposal_id, surface, subject, receipt_json, created_at FROM adapt_application_receipts WHERE receipt_id = ?1",
        [receipt_id],
        row_to_receipt,
    )
    .optional()
    .map_err(Into::into)
}

fn surface_digest_tx(tx: &Transaction<'_>, kind: &str) -> Result<String> {
    let mut statement = tx.prepare(
        "SELECT guidance_id, digest FROM adapt_guidance_records WHERE kind = ?1 ORDER BY guidance_id",
    )?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let rows = statement.query_map([kind], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (guidance_id, digest) = row?;
        hasher.update(guidance_id.as_bytes());
        hasher.update(&[0]);
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hasher.finalize())
}

fn content_digest(content: &str) -> String {
    rightkit_fs::digest::sha256_bytes(content.as_bytes())
}

fn row_to_guidance(row: &rusqlite::Row<'_>) -> rusqlite::Result<AdaptGuidanceRow> {
    Ok(AdaptGuidanceRow {
        guidance_id: row.get(0)?,
        kind: row.get(1)?,
        subject: row.get(2)?,
        content: row.get(3)?,
        digest: row.get(4)?,
    })
}

fn row_to_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<AdaptApplicationReceiptRow> {
    Ok(AdaptApplicationReceiptRow {
        receipt_id: row.get(0)?,
        proposal_id: row.get(1)?,
        surface: row.get(2)?,
        subject: row.get(3)?,
        receipt_json: row.get(4)?,
        created_at: row.get(5)?,
    })
}
