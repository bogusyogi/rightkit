//! Shared query helper (L15): the `prepare → query_map → collect →
//! map_err(label)` pattern repeated across `db/` call sites. This is a pure
//! extraction — every call site produces the SAME `Result<Vec<T>, String>`
//! with the SAME three error labels ("prepare {label}: {e}", "query {label}:
//! {e}", "row {label}: {e}") it had before, so behavior (including error text)
//! is unchanged.
use rightkit_db::rusqlite::{Connection, Params, Row};

/// Prepare `sql`, run `query_map` with `params`, and collect every row via
/// `row_fn` into a `Vec<T>`. `label` names the operation for the three error
/// variants (prepare / query / row) so a failure still says what it was doing.
pub fn query_map_collect<T, P, F>(
    conn: &Connection,
    sql: &str,
    params: P,
    label: &str,
    mut row_fn: F,
) -> Result<Vec<T>, String>
where
    P: Params,
    F: FnMut(&Row<'_>) -> rightkit_db::rusqlite::Result<T>,
{
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("prepare {label}: {e}"))?;
    let rows = stmt
        .query_map(params, |r| row_fn(r))
        .map_err(|e| format!("query {label}: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("row {label}: {e}"))?);
    }
    Ok(out)
}
