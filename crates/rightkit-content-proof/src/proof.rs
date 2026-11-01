//! Proof semantics, schema v35 of the host channel store. Extends the host proof store and text gate;
//! replaces neither.
//!
//! What this module adds:
//! - `proof.claim_review` (§5): the 13-step deterministic match for every claim annotation, the unbound-number
//!   scan, the review epoch (`E_STALE_REVIEW`), and the retired-claim-text rule. The review is recorded.
//!   Number values are compared as exact decimals by digit string, never as floats: `1,240` equals `1240.00`
//!   and `1.24k` equals `1240`. Periods are compared by exact equality (decision 2026-10-16).
//! - `proof.review_epoch_bump`, `proof.review_records`: the epoch a retrieval or script change moves on.
//! - `proof.expire_collected`: a collected proof not approved within 90 days is marked expired. The record
//!   stays; it can no longer be approved (decision 2026-10-16). The mark lives in `proof_expired` because the
//!   crate's status set has no `expired`.
//! - `proof.approval_check` and [`approval_policy`]: single-approver approval, a withdrawn proof is never
//!   re-approved under the same id, an expired collected proof cannot be approved, and every approval needs an
//!   explicit `validUntil` (the UI suggests 12 months; decision 2026-10-16).
//! - A superseded or withdrawn proof retires the claim text it permitted: copy that still states that text
//!   fails with `E_RETIRED_CLAIM_TEXT` unless an approved proof permits the same text.
//!
//! Errors: `foreign_scope`, `bad_input`, `db_error`, `unknown_method`, plus the `E_` codes above as `Fail`
//! codes from `proof.approval_check`. Review findings are data in the output, never an `Err`.
use std::collections::BTreeSet;
use std::path::Path;

use crate::protocol::{
    ApprovalCheckInput, ClaimReviewInput, ClaimReviewOutput, EpochBumpInput, EpochOutput,
    ExpireCollectedInput, ExpireCollectedOutput, ProofAnnotation, ReviewFinding,
    ReviewRecordsInput, Scope, COLLECTED_EXPIRY_DAYS,
};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_db::Migration;
use serde_json::{json, Value};

use crate::host::{approver, open_channels, text_hash};
use crate::Fail;
use rightkit_social_publish::calendar_time as time;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &[
    "proof.claim_review",
    "proof.review_epoch_bump",
    "proof.expire_collected",
];

pub const METHODS: &[&str] = &[
    "proof.claim_review",
    "proof.review_epoch_bump",
    "proof.review_records",
    "proof.expire_collected",
    "proof.approval_check",
];

const SCHEMA_V35: &str = "
CREATE TABLE proof_review_epochs (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    brief_id TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch >= 0),
    PRIMARY KEY (venture, channel, account, brief_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
CREATE TABLE proof_claim_reviews (
    review_id INTEGER PRIMARY KEY AUTOINCREMENT,
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    brief_id TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    text_hash TEXT NOT NULL,
    disposition TEXT NOT NULL CHECK (disposition IN ('pass', 'blocked')),
    findings_json TEXT NOT NULL,
    reviewed_at TEXT NOT NULL,
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
CREATE INDEX proof_claim_reviews_by_brief ON proof_claim_reviews (venture, channel, account, brief_id, review_id);
CREATE TABLE proof_expired (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    proof_id TEXT NOT NULL,
    expired_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, proof_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
CREATE TABLE proof_conflicts (
    conflict_id TEXT NOT NULL,
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    proof_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'resolved')),
    record_json TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, conflict_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
";

/// Schema v35, appended to the channel store's migration list in channels.rs after v34.
pub const MIGRATION_V35: Migration = Migration::sql(35, "proof-semantics", SCHEMA_V35);

fn fail(code: &'static str, msg: impl Into<String>) -> Fail {
    (code, msg.into())
}

fn bad(msg: impl Into<String>) -> Fail {
    fail("bad_input", msg)
}

fn db_err(e: impl std::fmt::Display) -> Fail {
    fail("db_error", e.to_string())
}

pub fn dispatch(root: &Path, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "proof.claim_review" => claim_review(root, input),
        "proof.review_epoch_bump" => epoch_bump(root, input),
        "proof.review_records" => review_records(root, input),
        "proof.expire_collected" => expire_collected(root, input),
        "proof.approval_check" => approval_check(root, input),
        other => Err(fail("unknown_method", format!("unknown method: {other}"))),
    }
}

fn parse<T: serde::de::DeserializeOwned>(input: Value, what: &str) -> Result<T, Fail> {
    serde_json::from_value(input).map_err(|e| bad(format!("{what}: {e}")))
}

fn require_scope(conn: &Connection, scope: &Scope) -> Result<(), Fail> {
    let known: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM channels WHERE venture = ?1 AND channel = ?2 AND account = ?3",
            params![scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    if known == 0 {
        return Err(fail(
            "foreign_scope",
            format!(
                "no stored channel for venture={} channel={} account={}",
                scope.venture, scope.channel, scope.account
            ),
        ));
    }
    Ok(())
}

/// The UTC instant for `now` (the clock when absent) and its date.
fn clock(now: Option<&str>) -> Result<(i64, String), Fail> {
    let secs = time::now_secs(now)?;
    let stamp = time::format_instant(secs);
    Ok((secs, stamp[..10].to_string()))
}

// ---------------------------------------------------------------------------------------------
// Exact decimals and number tokens (§5.2, §5.3 step 8)
// ---------------------------------------------------------------------------------------------

const WORD_VALUES: &[(&str, &str)] = &[
    ("zero", "0"),
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
    ("eleven", "11"),
    ("twelve", "12"),
    ("thirteen", "13"),
    ("fourteen", "14"),
    ("fifteen", "15"),
    ("sixteen", "16"),
    ("seventeen", "17"),
    ("eighteen", "18"),
    ("nineteen", "19"),
    ("twenty", "20"),
    ("hundred", "100"),
    ("thousand", "1000"),
    ("million", "1000000"),
    ("billion", "1000000000"),
];

/// Canonical exact decimal of a token as §5.2 reads it: optional currency sign, thousands commas, a decimal
/// point, a `k`, `m` or `bn` suffix and `%`; or a listed number word. `None` when it is not a number.
/// No rounding: the digits are shifted as text.
fn canonical(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if let Some((_, v)) = WORD_VALUES.iter().find(|(w, _)| *w == lower) {
        return Some((*v).to_string());
    }
    let mut s: &str = &lower;
    s = s.strip_prefix(['$', '£', '€']).unwrap_or(s);
    s = s.strip_suffix('%').unwrap_or(s);
    let (digits, shift) = if let Some(d) = s.strip_suffix("bn") {
        (d, 9)
    } else if let Some(d) = s.strip_suffix('k') {
        (d, 3)
    } else if let Some(d) = s.strip_suffix('m') {
        (d, 6)
    } else {
        (s, 0)
    };
    let digits = digits.replace(',', "");
    let (int, frac) = match digits.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (digits.clone(), String::new()),
    };
    if int.is_empty()
        || !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let all = format!("{int}{frac}");
    let point = int.len() + shift;
    let (head, tail) = if point >= all.len() {
        (
            format!("{all}{}", "0".repeat(point - all.len())),
            String::new(),
        )
    } else {
        (all[..point].to_string(), all[point..].to_string())
    };
    let head = head.trim_start_matches('0');
    let head = if head.is_empty() { "0" } else { head };
    let tail = tail.trim_end_matches('0');
    Some(if tail.is_empty() {
        head.to_string()
    } else {
        format!("{head}.{tail}")
    })
}

/// One numeric token in the text: character range, the text of it and its canonical value.
struct Token {
    start: usize,
    end: usize,
    text: String,
    value: Option<String>,
}

fn tokens(text: &str) -> Vec<Token> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let currency =
            matches!(c[i], '$' | '£' | '€') && c.get(i + 1).is_some_and(|d| d.is_ascii_digit());
        if c[i].is_ascii_digit() || currency {
            let start = i;
            if currency {
                i += 1;
            }
            while i < c.len() {
                if c[i].is_ascii_digit() {
                    i += 1;
                } else if c[i] == ',' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1;
                } else if c[i] == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1;
                } else {
                    break;
                }
            }
            let rest: String = c[i..c.len().min(i + 3)]
                .iter()
                .collect::<String>()
                .to_lowercase();
            let suffix = if rest.starts_with("bn") {
                2
            } else if rest.starts_with('k') || rest.starts_with('m') {
                1
            } else {
                0
            };
            if suffix > 0 && !c.get(i + suffix).is_some_and(|d| d.is_alphanumeric()) {
                i += suffix;
            }
            if c.get(i) == Some(&'%') {
                i += 1;
            }
            let raw: String = c[start..i].iter().collect();
            out.push(Token {
                start,
                end: i,
                value: canonical(&raw),
                text: raw,
            });
        } else if c[i].is_alphabetic() {
            let start = i;
            while i < c.len() && c[i].is_alphabetic() {
                i += 1;
            }
            let word: String = c[start..i].iter().collect();
            if let Some(value) = canonical(&word)
                .filter(|_| WORD_VALUES.iter().any(|(w, _)| *w == word.to_lowercase()))
            {
                out.push(Token {
                    start,
                    end: i,
                    value: Some(value),
                    text: word,
                });
            }
        } else {
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Stored proofs
// ---------------------------------------------------------------------------------------------

/// The stored proof record of this scope, as the crate serialised it.
fn stored_record(conn: &Connection, scope: &Scope, proof_id: &str) -> Result<Option<Value>, Fail> {
    let json: Option<Option<String>> = conn
        .query_row(
            "SELECT record_json FROM proofs WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND proof_id = ?4",
            params![scope.venture, scope.channel, scope.account, proof_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    match json.flatten() {
        Some(raw) => serde_json::from_str(&raw).map(Some).map_err(db_err),
        None => Ok(None),
    }
}

fn exists_in_other_scope(conn: &Connection, scope: &Scope, proof_id: &str) -> Result<bool, Fail> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM proofs WHERE proof_id = ?1 AND NOT (venture = ?2 AND channel = ?3 AND account = ?4)",
            params![proof_id, scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok(n > 0)
}

fn is_observation(conn: &Connection, scope: &Scope, id: &str) -> Result<bool, Fail> {
    let doc: Option<String> = conn
        .query_row(
            "SELECT document_json FROM channel_memory_revisions
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 ORDER BY revision DESC LIMIT 1",
            params![scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    let Some(doc) = doc else { return Ok(false) };
    let doc: Value = serde_json::from_str(&doc).map_err(db_err)?;
    Ok(doc
        .get("observations")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|o| o.get("id").and_then(Value::as_str) == Some(id))
        }))
}

fn is_expired_collected(conn: &Connection, scope: &Scope, proof_id: &str) -> Result<bool, Fail> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM proof_expired WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND proof_id = ?4",
            params![scope.venture, scope.channel, scope.account, proof_id],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok(n > 0)
}

fn has_open_conflict(conn: &Connection, scope: &Scope, proof_id: &str) -> Result<bool, Fail> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM proof_conflicts
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND proof_id = ?4 AND status = 'open'",
            params![scope.venture, scope.channel, scope.account, proof_id],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok(n > 0)
}

fn s<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Claim review
// ---------------------------------------------------------------------------------------------

fn finding(
    step: u8,
    code: &str,
    claim: Option<&str>,
    expected: Option<String>,
    found: Option<String>,
    detail: String,
) -> ReviewFinding {
    ReviewFinding {
        step,
        code: code.to_string(),
        claim_id: claim.map(str::to_string),
        expected,
        found,
        detail,
    }
}

/// Steps 1 to 13 of §5.3 for one annotation. The first failing step is the finding; `None` is a resolved claim.
fn review_annotation(
    conn: &Connection,
    scope: &Scope,
    today: &str,
    chars: &[char],
    a: &ProofAnnotation,
) -> Result<Option<ReviewFinding>, Fail> {
    let cid = Some(a.claim_id.as_str());
    let pid = a.proof_id.as_str();
    // Steps 1 to 3. An observation id never reaches the proof bank (O3).
    if is_observation(conn, scope, pid)? {
        return Ok(Some(finding(
            3,
            "E_OBSERVATION_AS_FACT",
            cid,
            None,
            Some(pid.into()),
            format!("`{pid}` is an observation, not a proof"),
        )));
    }
    let Some(record) = stored_record(conn, scope, pid)? else {
        return Ok(Some(if exists_in_other_scope(conn, scope, pid)? {
            finding(
                2,
                "E_FOREIGN_SCOPE",
                cid,
                None,
                Some(pid.into()),
                format!("`{pid}` belongs to another venture, channel or account"),
            )
        } else {
            finding(
                1,
                "E_UNKNOWN_PROOF",
                cid,
                None,
                Some(pid.into()),
                format!("`{pid}` is not in this scope's proof bank"),
            )
        }));
    };
    // Step 4. Withdrawn is terminal, superseded retires the proof (value and claim text), collected is not usable.
    let status = s(&record, "/status");
    match status {
        "approved" => {}
        "collected" => {
            let (code, why) = if is_expired_collected(conn, scope, pid)? {
                (
                    "E_COLLECTED_EXPIRED",
                    "was collected and not approved within 90 days",
                )
            } else {
                ("E_PROOF_NOT_APPROVED", "is collected, not approved")
            };
            return Ok(Some(finding(
                4,
                code,
                cid,
                None,
                Some(status.into()),
                format!("`{pid}` {why}"),
            )));
        }
        "withdrawn" => {
            return Ok(Some(finding(
                4,
                "E_PROOF_WITHDRAWN",
                cid,
                None,
                Some(status.into()),
                format!("`{pid}` was withdrawn"),
            )))
        }
        _ => {
            let by = s(&record, "/supersededBy");
            return Ok(Some(finding(
                4,
                "E_PROOF_SUPERSEDED",
                cid,
                Some(by.into()),
                Some(pid.into()),
                format!("`{pid}` was superseded by `{by}`; its value and claim text are retired"),
            )));
        }
    }
    // Step 5.
    let revision = record.get("revision").and_then(Value::as_u64).unwrap_or(0);
    if u64::from(a.proof_revision) != revision {
        return Ok(Some(finding(
            5,
            "E_PROOF_REVISION_STALE",
            cid,
            Some(revision.to_string()),
            Some(a.proof_revision.to_string()),
            format!(
                "annotation names revision {} of `{pid}`, current is {revision}",
                a.proof_revision
            ),
        )));
    }
    // Step 6. An open `validUntil` (null) never expires; approval requires one (see `approval_policy`).
    let from = s(&record, "/validFrom");
    if today < from {
        return Ok(Some(finding(
            6,
            "E_PROOF_NOT_YET_VALID",
            cid,
            Some(from.into()),
            Some(today.into()),
            format!("`{pid}` is valid from {from}"),
        )));
    }
    if let Some(until) = record.get("validUntil").and_then(Value::as_str) {
        if today > until {
            return Ok(Some(finding(
                6,
                "E_PROOF_EXPIRED",
                cid,
                Some(until.into()),
                Some(today.into()),
                format!("`{pid}` was valid until {until}"),
            )));
        }
    }
    // Step 7.
    let claims = record
        .get("permittedClaims")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(cited) = a.claim_text.as_deref() {
        let want = cited.trim().to_lowercase();
        if !claims
            .iter()
            .any(|c| s(c, "/text").trim().to_lowercase() == want)
        {
            return Ok(Some(finding(
                7,
                "E_CLAIM_NOT_PERMITTED",
                cid,
                None,
                Some(cited.into()),
                format!("claim text `{cited}` is not one of the texts `{pid}` permits"),
            )));
        }
    }
    let Some(claim) = claims.iter().find(|c| s(c, "/claimId") == a.claim_id) else {
        return Ok(Some(finding(
            7,
            "E_CLAIM_NOT_PERMITTED",
            cid,
            None,
            Some(a.claim_id.clone()),
            format!("claim `{}` is not permitted for `{pid}`", a.claim_id),
        )));
    };
    // Step 8. Exact decimal equality; an annotation that is not a number cannot equal one.
    let claim_value = s(claim, "/value");
    let annotated = canonical(&a.value);
    if annotated.is_none() || annotated != canonical(claim_value) {
        return Ok(Some(finding(
            8,
            "E_NUMBER_MISMATCH",
            cid,
            Some(claim_value.into()),
            Some(a.value.clone()),
            format!("claim permits {claim_value}, annotation states {}", a.value),
        )));
    }
    // Steps 9 to 11. Period equality is exact on both inclusive dates (decision 2026-10-16).
    if a.units != s(claim, "/units") {
        return Ok(Some(finding(
            9,
            "E_UNIT_MISMATCH",
            cid,
            Some(s(claim, "/units").into()),
            Some(a.units.clone()),
            "units differ".into(),
        )));
    }
    if a.subject != s(claim, "/subject")
        || (!s(&record, "/subject").is_empty() && a.subject != s(&record, "/subject"))
    {
        return Ok(Some(finding(
            10,
            "E_SUBJECT_MISMATCH",
            cid,
            Some(s(claim, "/subject").into()),
            Some(a.subject.clone()),
            "subject differs".into(),
        )));
    }
    let record_period_differs = !s(&record, "/period/from").is_empty()
        && (a.period.from != s(&record, "/period/from") || a.period.to != s(&record, "/period/to"));
    if a.period.from != s(claim, "/period/from")
        || a.period.to != s(claim, "/period/to")
        || record_period_differs
    {
        return Ok(Some(finding(
            11,
            "E_PERIOD_MISMATCH",
            cid,
            Some(format!(
                "{}..{}",
                s(claim, "/period/from"),
                s(claim, "/period/to")
            )),
            Some(format!("{}..{}", a.period.from, a.period.to)),
            "period differs; periods match exactly, never by overlap".into(),
        )));
    }
    // Step 12. The number at the span is the annotated number.
    if a.span.start >= a.span.end || a.span.end > chars.len() {
        return Ok(Some(finding(
            12,
            "E_TEXT_ANNOTATION_MISMATCH",
            cid,
            Some(a.value.clone()),
            None,
            "the span is empty or outside the text".into(),
        )));
    }
    let slice: String = chars[a.span.start..a.span.end].iter().collect();
    let inside: Vec<Token> = tokens(&slice);
    let read = inside.first().and_then(|t| t.value.clone());
    if read.is_none() || read != annotated {
        return Ok(Some(finding(
            12,
            "E_TEXT_ANNOTATION_MISMATCH",
            cid,
            Some(a.value.clone()),
            Some(slice),
            "the number in the text at the span is not the annotated number".into(),
        )));
    }
    // Step 13.
    if has_open_conflict(conn, scope, pid)? {
        return Ok(Some(finding(
            13,
            "E_PROOF_CONFLICT",
            cid,
            None,
            Some(pid.into()),
            format!("`{pid}` is in an open conflict"),
        )));
    }
    Ok(None)
}

/// Claim text that a superseded or withdrawn proof permitted and no approved proof permits any more.
fn retired_texts(conn: &Connection, scope: &Scope) -> Result<Vec<(String, String)>, Fail> {
    let mut stmt = conn
        .prepare(
            "SELECT proof_id, status, record_json FROM proofs
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND record_json IS NOT NULL
             ORDER BY proof_id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![scope.venture, scope.channel, scope.account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    let mut live = BTreeSet::new();
    let mut dead = Vec::new();
    for (id, status, raw) in rows {
        let record: Value = serde_json::from_str(&raw).map_err(db_err)?;
        for claim in record
            .get("permittedClaims")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = s(claim, "/text").trim().to_lowercase();
            if text.is_empty() {
                continue;
            }
            match status.as_str() {
                "approved" => {
                    live.insert(text);
                }
                "superseded" | "withdrawn" => dead.push((id.clone(), text)),
                _ => {}
            }
        }
    }
    // The RightKit schema gives a superseded or withdrawn record no `permittedClaims`, so the texts it
    // permitted while approved are kept in `proof_retired_claims` (see `retire_claims`).
    ensure_retired_table(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT r.proof_id, r.text FROM proof_retired_claims r
             JOIN proofs p ON p.venture = r.venture AND p.channel = r.channel AND p.account = r.account AND p.proof_id = r.proof_id
             WHERE r.venture = ?1 AND r.channel = ?2 AND r.account = ?3 AND p.status IN ('superseded', 'withdrawn')
             ORDER BY r.proof_id, r.text",
        )
        .map_err(db_err)?;
    let kept = stmt
        .query_map(params![scope.venture, scope.channel, scope.account], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    dead.extend(kept);
    dead.retain(|(_, text)| !live.contains(text));
    Ok(dead)
}

fn ensure_retired_table(conn: &Connection) -> Result<(), Fail> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS proof_retired_claims (
            venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
            proof_id TEXT NOT NULL, text TEXT NOT NULL,
            PRIMARY KEY (venture, channel, account, proof_id, text)
        )",
    )
    .map_err(db_err)
}

/// Keeps the claim texts of an approved record that is about to become superseded or withdrawn.
/// Call it with the approved record before the status change drops its `permittedClaims`
/// (superseded and withdrawn records carry none under the RightKit schema). Idempotent.
pub fn retire_claims(
    conn: &Connection,
    scope: &Scope,
    proof_id: &str,
    approved_record: &Value,
) -> Result<(), Fail> {
    ensure_retired_table(conn)?;
    for claim in approved_record
        .get("permittedClaims")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let text = s(claim, "/text").trim().to_lowercase();
        if text.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO proof_retired_claims (venture, channel, account, proof_id, text) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![scope.venture, scope.channel, scope.account, proof_id, text],
        )
        .map_err(db_err)?;
    }
    Ok(())
}

fn current_epoch(conn: &Connection, scope: &Scope, brief_id: &str) -> Result<u32, Fail> {
    let epoch: Option<i64> = conn
        .query_row(
            "SELECT epoch FROM proof_review_epochs WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND brief_id = ?4",
            params![scope.venture, scope.channel, scope.account, brief_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    Ok(epoch.unwrap_or(0) as u32)
}

pub fn claim_review(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ClaimReviewInput = parse(
        input,
        "input needs scope, brief_id, epoch, text and annotations",
    )?;
    if req.brief_id.trim().is_empty() {
        return Err(bad("brief_id must not be empty"));
    }
    if req.text.trim().is_empty() {
        return Err(bad("text must not be empty"));
    }
    let conn = open_channels(root)?;
    let scope = req.scope.clone();
    require_scope(&conn, &scope)?;
    let (secs, today) = clock(req.now.as_deref())?;
    let current = current_epoch(&conn, &scope, &req.brief_id)?;
    if req.epoch > current {
        return Err(bad(format!(
            "epoch {} is ahead of the stored epoch {current} for `{}`; only proof.review_epoch_bump moves it",
            req.epoch, req.brief_id
        )));
    }
    let chars: Vec<char> = req.text.chars().collect();
    let mut findings = Vec::new();
    if req.epoch < current {
        findings.push(finding(
            0,
            "E_STALE_REVIEW",
            None,
            Some(current.to_string()),
            Some(req.epoch.to_string()),
            format!(
                "review ran in epoch {}, the brief is at epoch {current}; run it again",
                req.epoch
            ),
        ));
    }
    for a in &req.annotations {
        if let Some(f) = review_annotation(&conn, &scope, &today, &chars, a)? {
            findings.push(f);
        }
    }
    // Every numeric token must sit inside an annotation span (E_UNBOUND_NUMBER).
    for t in tokens(&req.text) {
        let bound = req
            .annotations
            .iter()
            .any(|a| t.start < a.span.end && a.span.start < t.end);
        if !bound {
            findings.push(finding(
                0,
                "E_UNBOUND_NUMBER",
                None,
                None,
                Some(t.text.clone()),
                format!(
                    "`{}` at {}..{} has no claim annotation",
                    t.text, t.start, t.end
                ),
            ));
        }
    }
    // A retired claim text must not survive in copy.
    let lower = req.text.to_lowercase();
    for (proof_id, text) in retired_texts(&conn, &scope)? {
        if lower.contains(&text) {
            findings.push(finding(4, "E_RETIRED_CLAIM_TEXT", None, None, Some(text.clone()), format!("`{proof_id}` is superseded or withdrawn; its claim text is retired until a current proof permits it again")));
        }
    }
    let disposition = if findings.is_empty() {
        "pass"
    } else {
        "blocked"
    };
    let hash = text_hash(&req.text);
    let findings_json = serde_json::to_string(&findings).map_err(db_err)?;
    conn.execute(
        "INSERT INTO proof_claim_reviews (venture, channel, account, brief_id, epoch, text_hash, disposition, findings_json, reviewed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![scope.venture, scope.channel, scope.account, req.brief_id, req.epoch, hash, disposition, findings_json, time::format_instant(secs)],
    )
    .map_err(db_err)?;
    let review_id = conn.last_insert_rowid();
    serde_json::to_value(ClaimReviewOutput {
        scope,
        brief_id: req.brief_id,
        epoch: req.epoch,
        current_epoch: current,
        text_hash: hash,
        disposition: disposition.to_string(),
        findings,
        review_id,
    })
    .map_err(db_err)
}

pub fn epoch_bump(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: EpochBumpInput = parse(input, "input needs scope and brief_id")?;
    if req.brief_id.trim().is_empty() {
        return Err(bad("brief_id must not be empty"));
    }
    let conn = open_channels(root)?;
    require_scope(&conn, &req.scope)?;
    let next = current_epoch(&conn, &req.scope, &req.brief_id)? + 1;
    conn.execute(
        "INSERT INTO proof_review_epochs (venture, channel, account, brief_id, epoch) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (venture, channel, account, brief_id) DO UPDATE SET epoch = excluded.epoch",
        params![req.scope.venture, req.scope.channel, req.scope.account, req.brief_id, next],
    )
    .map_err(db_err)?;
    serde_json::to_value(EpochOutput {
        scope: req.scope,
        brief_id: req.brief_id,
        epoch: next,
    })
    .map_err(db_err)
}

pub fn review_records(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ReviewRecordsInput = parse(input, "input needs scope and brief_id")?;
    let conn = open_channels(root)?;
    require_scope(&conn, &req.scope)?;
    let mut stmt = conn
        .prepare(
            "SELECT review_id, epoch, text_hash, disposition, findings_json, reviewed_at FROM proof_claim_reviews
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND brief_id = ?4 ORDER BY review_id DESC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.brief_id
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    let mut reviews = Vec::new();
    for (id, epoch, hash, disposition, findings, at) in rows {
        let findings: Value = serde_json::from_str(&findings).map_err(db_err)?;
        reviews.push(json!({
            "review_id": id, "epoch": epoch, "text_hash": hash,
            "disposition": disposition, "findings": findings, "reviewed_at": at,
        }));
    }
    Ok(json!({ "scope": req.scope, "brief_id": req.brief_id, "reviews": reviews }))
}

// ---------------------------------------------------------------------------------------------
// Expiry of collected proofs and the approval policy
// ---------------------------------------------------------------------------------------------

pub fn expire_collected(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ExpireCollectedInput = parse(input, "input needs scope")?;
    let conn = open_channels(root)?;
    require_scope(&conn, &req.scope)?;
    let (secs, _) = clock(req.now.as_deref())?;
    let scope = &req.scope;
    let mut stmt = conn
        .prepare(
            "SELECT proof_id, record_json FROM proofs
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND status = 'collected' AND record_json IS NOT NULL
             ORDER BY proof_id",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![scope.venture, scope.channel, scope.account], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    let mut expired = Vec::new();
    for (id, raw) in rows {
        let record: Value = serde_json::from_str(&raw).map_err(db_err)?;
        let updated = time::parse_instant(s(&record, "/updatedAt"), "proof updatedAt")?;
        if secs - updated > COLLECTED_EXPIRY_DAYS * 86_400
            && !is_expired_collected(&conn, scope, &id)?
        {
            conn.execute(
                "INSERT INTO proof_expired (venture, channel, account, proof_id, expired_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![scope.venture, scope.channel, scope.account, id, time::format_instant(secs)],
            )
            .map_err(db_err)?;
            expired.push(id);
        }
    }
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM proof_expired WHERE venture = ?1 AND channel = ?2 AND account = ?3",
            params![scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    serde_json::to_value(ExpireCollectedOutput {
        scope: req.scope,
        expired,
        expired_total: total as u64,
    })
    .map_err(db_err)
}

/// The approval policy of the 2026-10-16 decisions, for a write that moves `proof_id` to `approved`.
/// `author` must be `adrian` (no delegate). A withdrawn proof is never re-approved under the same id
/// (`E_PROOF_WITHDRAWN`); only a collected proof can be approved (`E_ILLEGAL_TRANSITION` otherwise); a
/// collected proof past its 90 days cannot be approved (`E_COLLECTED_EXPIRED`); the approval needs an explicit
/// `validUntil` (`E_VALID_UNTIL_REQUIRED`), taken from `next_valid_until` or, when absent, the stored record.
/// channels.rs calls this before writing an approval.
pub fn approval_policy(
    conn: &Connection,
    scope: &Scope,
    proof_id: &str,
    author: &str,
    next_valid_until: Option<&str>,
    now: Option<&str>,
) -> Result<(), Fail> {
    if author != approver() {
        return Err(fail(
            "E_AUTHOR_FORBIDDEN",
            format!("only {} approves a proof; `{author}` may not", approver()),
        ));
    }
    let record = stored_record(conn, scope, proof_id)?.ok_or_else(|| {
        fail(
            "E_UNKNOWN_PROOF",
            format!("`{proof_id}` is not stored for this scope"),
        )
    })?;
    match s(&record, "/status") {
        "collected" => {}
        "withdrawn" => {
            return Err(fail(
                "E_PROOF_WITHDRAWN",
                format!("`{proof_id}` was withdrawn; a corrected fact is a new proof id"),
            ))
        }
        other => {
            return Err(fail(
                "E_ILLEGAL_TRANSITION",
                format!("`{proof_id}` is {other}; only a collected proof is approved"),
            ))
        }
    }
    let (secs, _) = clock(now)?;
    let updated = time::parse_instant(s(&record, "/updatedAt"), "proof updatedAt")?;
    if is_expired_collected(conn, scope, proof_id)?
        || secs - updated > COLLECTED_EXPIRY_DAYS * 86_400
    {
        return Err(fail("E_COLLECTED_EXPIRED", format!("`{proof_id}` was collected over {COLLECTED_EXPIRY_DAYS} days ago and cannot be approved; collect it again")));
    }
    let until = next_valid_until.or_else(|| record.get("validUntil").and_then(Value::as_str));
    if until.map_or(true, |u| u.trim().is_empty()) {
        return Err(fail(
            "E_VALID_UNTIL_REQUIRED",
            format!("approving `{proof_id}` needs an explicit validUntil date (suggest 12 months)"),
        ));
    }
    Ok(())
}

pub fn approval_check(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ApprovalCheckInput = parse(input, "input needs scope, proof_id and author")?;
    let conn = open_channels(root)?;
    require_scope(&conn, &req.scope)?;
    approval_policy(
        &conn,
        &req.scope,
        &req.proof_id,
        &req.author,
        req.valid_until.as_deref(),
        req.now.as_deref(),
    )?;
    Ok(json!({ "scope": req.scope, "proof_id": req.proof_id, "allowed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_are_exact() {
        assert_eq!(canonical("1,240").as_deref(), Some("1240"));
        assert_eq!(canonical("1240.00").as_deref(), Some("1240"));
        assert_eq!(canonical("$1,240").as_deref(), Some("1240"));
        assert_eq!(canonical("1.24k").as_deref(), Some("1240"));
        assert_eq!(canonical("2.5m").as_deref(), Some("2500000"));
        assert_eq!(canonical("0.5").as_deref(), Some("0.5"));
        assert_eq!(canonical("12%").as_deref(), Some("12"));
        assert_eq!(canonical("twelve").as_deref(), Some("12"));
        assert_ne!(canonical("1420"), canonical("1240.00"));
        assert_eq!(canonical("abc"), None);
    }

    #[test]
    fn tokens_cover_signs_suffixes_and_words() {
        let t = tokens("Made $1,240 in 7 days, up 12% and 1.2k views, nine wins.");
        let found: Vec<&str> = t.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(found, ["$1,240", "7", "12%", "1.2k", "nine"]);
    }
}
