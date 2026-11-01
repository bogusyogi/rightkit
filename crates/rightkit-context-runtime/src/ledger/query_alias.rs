//! Revision/span-bound future-question aliases for Ledger.
//!
//! Aliases are separately weighted retrieval metadata. They never enter authoritative document
//! text or FTS rows.

use rightkit_db::rusqlite::{self, Transaction};
use rightkit_fs::digest::sha256_bytes;
use std::collections::BTreeSet;

#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_node_aliases_tx(
    tx: &Transaction<'_>,
    doc_id: &str,
    node_id: &str,
    anchor_id: &str,
    _heading: &str,
    body: &str,
    section_start_byte: usize,
    source_revision: &str,
    span_hash: &str,
    generation: i64,
) -> rusqlite::Result<()> {
    let mut seen = BTreeSet::new();
    for fact in declarative_fact_aliases(body) {
        if !seen.insert(fact.alias.clone()) {
            continue;
        }
        tx.execute(
            "INSERT INTO ledger_query_aliases (
                doc_id,node_id,anchor_id,alias,weight,derivation,source_revision,span_hash,
                ledger_generation
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                doc_id,
                node_id,
                anchor_id,
                fact.alias,
                fact.weight,
                fact.derivation,
                source_revision,
                span_hash,
                generation,
            ],
        )?;
        tx.execute(
            "INSERT INTO ledger_query_alias_evidence (
                doc_id,node_id,alias,evidence_quote,evidence_start_byte,evidence_end_byte,
                evidence_sha256
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                doc_id,
                node_id,
                fact.alias,
                fact.quote,
                (section_start_byte + fact.start_byte) as i64,
                (section_start_byte + fact.end_byte) as i64,
                sha256_bytes(fact.quote.as_bytes()),
            ],
        )?;
    }
    Ok(())
}

struct DeclarativeAlias {
    alias: String,
    quote: String,
    start_byte: usize,
    end_byte: usize,
    weight: f64,
    derivation: &'static str,
}

fn declarative_fact_aliases(body: &str) -> Vec<DeclarativeAlias> {
    let mut output = Vec::new();
    let mut line_offset = 0usize;
    for line_with_newline in body.split_inclusive('\n') {
        let line = line_with_newline.trim_end_matches(['\r', '\n']);
        let mut sentence_start = 0usize;
        for (period, _) in line.match_indices('.') {
            let raw = &line[sentence_start..=period];
            let leading = raw.len() - raw.trim_start().len();
            let quote = raw.trim();
            let start = line_offset + sentence_start + leading;
            sentence_start = period + 1;
            if quote.len() < 8 || quote.len() > 320 || quote.contains('?') {
                continue;
            }
            let statement = quote.trim_end_matches('.').trim();
            let lower = statement.to_ascii_lowercase();
            let (separator, question_word, derivation) = if let Some(index) = lower.find(" is ") {
                (index, "What is", "declarative_copula_is")
            } else if let Some(index) = lower.find(" are ") {
                (index, "What are", "declarative_copula_are")
            } else {
                continue;
            };
            let subject = statement[..separator]
                .trim()
                .trim_start_matches(['-', '*', '>', '#'])
                .trim();
            if subject.len() < 2
                || subject.len() > 120
                || subject
                    .chars()
                    .any(|character| matches!(character, '[' | ']' | '(' | ')' | ':'))
            {
                continue;
            }
            output.push(DeclarativeAlias {
                alias: format!("{question_word} {subject}?"),
                quote: quote.to_owned(),
                start_byte: start,
                end_byte: start + quote.len(),
                weight: 1.0,
                derivation,
            });
        }
        line_offset += line_with_newline.len();
    }
    output
}
