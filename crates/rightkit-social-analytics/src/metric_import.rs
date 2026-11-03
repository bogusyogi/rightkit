//! Manual metric import: a platform export that the account owner downloads by hand, handed to the engine as
//! CSV text with an explicit column mapping. This module never logs in, never fetches a URL and never
//! scrapes: it parses the text it is given. Commands: `metric_import.preview` (read; writes nothing),
//! `metric_import.commit` (write; all or nothing) and `metric_import.list` (read).
//!
//! Storage (two tables of this module only, kept by the host behind [`MetricImportStore`]): `metric_imports` holds one row per import with the
//! canonical request for the idempotency check; `metric_import_values` holds one row per stored value. Rows
//! do not go to `metric_observations`: that table accepts only source `api` and needs an analytics pull row
//! for every observation, so a manual value would need a schema change to live there.
//!
//! Every command checks its scope against a stored channel (`foreign_scope` otherwise).

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::protocol::PublishRemote;
use rightkit_social_publish::util::{
    bad, db_err, internal, parse_input, require_channel, to_value, WriteTxn,
};
use serde_json::{json, Value};

use crate::protocol::{
    MetricImportCommitInput, MetricImportCommitOutput, MetricImportCounts, MetricImportListInput,
    MetricImportListOutput, MetricImportMapping, MetricImportPreviewInput,
    MetricImportPreviewOutput, MetricImportProblem, MetricImportRecord, MetricImportRow,
    MetricImportValue, Scope,
};
use crate::Fail;

/// Methods that write. A dry run reports them and writes nothing.
pub const WRITE_METHODS: &[&str] = &["metric_import.commit"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "metric_import.preview",
    "metric_import.commit",
    "metric_import.list",
];

/// The only source a manual import stores.
const SOURCE: &str = "manual_export";
/// The largest CSV text accepted, in bytes.
const MAX_BYTES: usize = 1024 * 1024;
/// The most data rows accepted in one import.
const MAX_ROWS: usize = 10_000;
/// The longest import id accepted, in bytes.
const IMPORT_ID_MAX: usize = 128;
/// The longest decimal literal accepted, in bytes (the analytics store uses the same bound).
const DECIMAL_MAX: usize = 40;

/// A published job as the store lists it: id, platform and the stored `remote` JSON text.
#[derive(Debug, Clone)]
pub struct PublishedJob {
    pub job_id: String,
    pub platform: String,
    pub remote_json: String,
}

/// One stored value of an import, with the job it belongs to.
#[derive(Debug, Clone)]
pub struct StoredValue<'a> {
    pub row_no: i64,
    pub job_id: &'a str,
    pub remote_post_id: &'a str,
    pub metric: &'a str,
    pub observed_at: &'a str,
    pub value: &'a str,
}

/// What the host app provides: the `metric_imports` and `metric_import_values` tables and the published
/// jobs of a scope. The two tables are written only here; values do not go to the analytics observations.
pub trait MetricImportStore: WriteTxn {
    /// Jobs in scope that are published (complete), oldest first (by creation, then id), with their
    /// stored `remote` JSON.
    fn published_jobs(&self, scope: &Scope) -> Result<Vec<PublishedJob>, Fail>;
    /// The stored import with this id in scope, with its canonical request text.
    fn load_import(
        &self,
        scope: &Scope,
        import_id: &str,
    ) -> Result<Option<(String, MetricImportRecord)>, Fail>;
    /// Every import in scope, ordered by creation then id.
    fn list_imports(&self, scope: &Scope) -> Result<Vec<MetricImportRecord>, Fail>;
    /// Stores the import row with its canonical request text.
    fn insert_import(&self, record: &MetricImportRecord, request: &str) -> Result<(), Fail>;
    /// Stores one value of an import.
    fn insert_value(
        &self,
        record: &MetricImportRecord,
        value: &StoredValue<'_>,
    ) -> Result<(), Fail>;
}

/// `metric_import.*` dispatch. The commands are read or written by the caller's method name.
pub fn dispatch(store: &dyn MetricImportStore, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "metric_import.preview" => preview(store, input),
        "metric_import.commit" => commit(store, input),
        "metric_import.list" => list(store, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// The import id is the caller's key: 1 to 128 ASCII letters, digits, `-`, `_` or `.`.
fn import_id_ok(id: &str) -> Result<(), Fail> {
    let fine = !id.is_empty()
        && id.len() <= IMPORT_ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if fine {
        Ok(())
    } else {
        Err(bad(format!(
            "import_id must be 1 to {IMPORT_ID_MAX} letters, digits, '-', '_' or '.'"
        )))
    }
}

fn platform_of(raw: &str) -> Result<String, Fail> {
    let platform = raw.trim();
    if platform.is_empty() {
        return Err(bad("platform must not be empty"));
    }
    Ok(platform.to_string())
}

/// Metric names are lowercase ASCII letters, digits, underscores and dots, at most 64 bytes (as analytics).
fn metric_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// A non-negative exact decimal: digits, optionally one `.` followed by digits. No sign, no exponent, no
/// separators and no float, so a value is never rounded.
fn is_decimal(text: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    text.len() <= DECIMAL_MAX
        && match text.split_once('.') {
            None => digits(text),
            Some((whole, fraction)) => digits(whole) && digits(fraction),
        }
}

/// The CSV text, parsed: the trimmed header names and the data records, each with one field per header name.
struct Csv {
    header: Vec<String>,
    records: Vec<Vec<String>>,
}

/// Parses the CSV text (RFC 4180): quoted fields with doubled quotes, CRLF or LF line endings, a UTF-8 byte
/// order mark ignored. The header row is required and its names must be non-empty and unique after trimming.
/// Blank lines are skipped. A ragged row, a malformed quote, a bare carriage return or more than 10,000 data
/// rows is `bad_input`. The whole text is refused above 1 MiB.
fn parse_csv(text: &str) -> Result<Csv, Fail> {
    if text.len() > MAX_BYTES {
        return Err(bad(format!("csv is larger than {MAX_BYTES} bytes")));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = split_records(text)?.into_iter();
    let header: Vec<String> = records
        .next()
        .ok_or_else(|| bad("csv has no header row"))?
        .iter()
        .map(|name| name.trim().to_string())
        .collect();
    if let Some(blank) = header.iter().position(String::is_empty) {
        return Err(bad(format!("header column {} has no name", blank + 1)));
    }
    for (i, name) in header.iter().enumerate() {
        if header[..i].contains(name) {
            return Err(bad(format!("duplicate header name {name:?}")));
        }
    }
    let mut data = Vec::new();
    for record in records {
        if record.len() == 1 && record[0].is_empty() {
            continue;
        }
        if record.len() != header.len() {
            return Err(bad(format!(
                "data row {} has {} fields; the header has {}",
                data.len() + 1,
                record.len(),
                header.len()
            )));
        }
        if data.len() == MAX_ROWS {
            return Err(bad(format!("csv has more than {MAX_ROWS} data rows")));
        }
        data.push(record);
    }
    Ok(Csv {
        header,
        records: data,
    })
}

/// The RFC 4180 state machine. A quote opens a field only at its start, and inside a quoted field a doubled
/// quote is one quote. Anything after a closing quote other than a comma or a line ending is refused.
fn split_records(text: &str) -> Result<Vec<Vec<String>>, Fail> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Start,
        Unquoted,
        Quoted,
        AfterQuote,
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut state = State::Start;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if state == State::Quoted {
            if c == '"' {
                state = State::AfterQuote;
            } else {
                field.push(c);
            }
            continue;
        }
        if state == State::AfterQuote && c == '"' {
            field.push('"');
            state = State::Quoted;
            continue;
        }
        match c {
            ',' => {
                record.push(std::mem::take(&mut field));
                state = State::Start;
            }
            '\r' | '\n' => {
                if c == '\r' {
                    if chars.peek() != Some(&'\n') {
                        return Err(bad("a bare carriage return is not a line ending"));
                    }
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut record));
                state = State::Start;
            }
            '"' if state == State::Start => state = State::Quoted,
            '"' => {
                return Err(bad(
                    "a quote may only open a field or be doubled inside a quoted field",
                ))
            }
            _ if state == State::AfterQuote => {
                return Err(bad(
                    "a closing quote must be followed by a comma or a line ending",
                ))
            }
            _ => {
                field.push(c);
                state = State::Unquoted;
            }
        }
    }
    match state {
        State::Quoted => return Err(bad("a quoted field is never closed")),
        State::Start if record.is_empty() && field.is_empty() => {}
        _ => {
            record.push(field);
            out.push(record);
        }
    }
    Ok(out)
}

/// One published job in scope with a remote post id: the only jobs a row can match.
struct PublishedPost {
    job_id: String,
    platform: String,
    post_id: String,
}

/// Published jobs in scope with a remote post id, oldest first. Reads only; no platform call.
fn published_posts(
    store: &dyn MetricImportStore,
    scope: &Scope,
) -> Result<Vec<PublishedPost>, Fail> {
    let mut posts = Vec::new();
    for PublishedJob {
        job_id,
        platform,
        remote_json,
    } in store.published_jobs(scope)?
    {
        let remote: PublishRemote = serde_json::from_str(&remote_json).map_err(db_err)?;
        if let Some(post_id) = remote.post_id.filter(|id| !id.trim().is_empty()) {
            posts.push(PublishedPost {
                job_id,
                platform,
                post_id,
            });
        }
    }
    Ok(posts)
}

fn problem(row: i64, column: Option<&str>, code: &str, message: String) -> MetricImportProblem {
    MetricImportProblem {
        row,
        column: column.map(str::to_string),
        code: code.to_string(),
        message,
    }
}

/// The index of a mapped column in the header. A missing name is a row-0 `missing_column` problem.
fn find_column(csv: &Csv, name: &str, problems: &mut Vec<MetricImportProblem>) -> Option<usize> {
    let name = name.trim();
    let found = csv.header.iter().position(|h| h == name);
    if found.is_none() {
        problems.push(problem(
            0,
            Some(name),
            "missing_column",
            format!("column {name:?} is not in the header"),
        ));
    }
    found
}

/// The checked import: every data row with its job and values, and every problem found.
struct Checked {
    rows: Vec<MetricImportRow>,
    problems: Vec<MetricImportProblem>,
}

/// Checks the rows against the mapping and the published jobs in scope. Reads only. A missing mapped column
/// stops the check with the missing columns as the only problems. Otherwise every row is checked, and every
/// problem is returned, so a preview shows them all.
fn check(
    store: &dyn MetricImportStore,
    scope: &Scope,
    platform: &str,
    csv: &Csv,
    mapping: &MetricImportMapping,
    now: i64,
) -> Result<Checked, Fail> {
    if mapping.metric_columns.is_empty() {
        return Err(bad("mapping.metric_columns needs at least one metric"));
    }
    let mut problems = Vec::new();
    let post_idx = find_column(csv, &mapping.post_id_column, &mut problems);
    let observed_idx = mapping
        .observed_at_column
        .as_deref()
        .and_then(|name| find_column(csv, name, &mut problems));
    let mut metric_idx: Vec<(&str, usize)> = Vec::new();
    for (metric, column) in &mapping.metric_columns {
        if !metric_name_ok(metric) {
            return Err(bad(format!(
                "metric name {metric:?} must be lowercase letters, digits, underscores or dots"
            )));
        }
        if let Some(i) = find_column(csv, column, &mut problems) {
            metric_idx.push((metric.as_str(), i));
        }
    }
    let Some(post_idx) = post_idx.filter(|_| problems.is_empty()) else {
        return Ok(Checked {
            rows: Vec::new(),
            problems,
        });
    };

    let posts = published_posts(store, scope)?;
    let default_observed = time::format_instant(now);
    let post_column = csv.header[post_idx].as_str();
    let observed_name = observed_idx.map(|i| csv.header[i].as_str());
    let mut rows = Vec::with_capacity(csv.records.len());
    for (n, record) in csv.records.iter().enumerate() {
        let row = (n + 1) as i64;
        let post_id = record[post_idx].trim().to_string();
        let matches: Vec<&PublishedPost> = posts
            .iter()
            .filter(|p| p.platform == platform && p.post_id == post_id)
            .collect();
        let job_id = match matches.as_slice() {
            [only] => Some(only.job_id.clone()),
            [] => {
                problems.push(problem(
                    row,
                    Some(post_column),
                    "unknown_post_id",
                    format!("row {row}: post id {post_id:?} matches no published {platform} job in this scope"),
                ));
                None
            }
            _ => {
                problems.push(problem(
                    row,
                    Some(post_column),
                    "ambiguous_post_id",
                    format!(
                        "row {row}: post id {post_id:?} matches {} published {platform} jobs",
                        matches.len()
                    ),
                ));
                None
            }
        };
        let observed_at = match observed_idx {
            None => Some(default_observed.clone()),
            Some(i) => match time::parse_instant(record[i].trim(), "observed_at") {
                Ok(secs) => Some(time::format_instant(secs)),
                Err(_) => {
                    problems.push(problem(
                        row,
                        observed_name,
                        "bad_observed_at",
                        format!("row {row}: observed_at is not a UTC instant YYYY-MM-DDTHH:MM:SSZ"),
                    ));
                    None
                }
            },
        };
        let mut values = Vec::new();
        for (metric, i) in &metric_idx {
            let cell = record[*i].trim();
            let column = csv.header[*i].as_str();
            if cell.is_empty() {
                problems.push(problem(
                    row,
                    Some(column),
                    "missing_value",
                    format!("row {row}: {metric} has no value"),
                ));
            } else if is_decimal(cell) {
                values.push(MetricImportValue {
                    metric: metric.to_string(),
                    value: cell.to_string(),
                });
            } else {
                problems.push(problem(
                    row,
                    Some(column),
                    "non_numeric_value",
                    format!("row {row}: {metric} is not a non-negative decimal number"),
                ));
            }
        }
        rows.push(MetricImportRow {
            row,
            post_id,
            job_id,
            observed_at,
            values,
        });
    }
    Ok(Checked { rows, problems })
}

/// Every problem as one line, for the `import_invalid` refusal.
fn invalid_summary(problems: &[MetricImportProblem]) -> String {
    let listed: Vec<String> = problems
        .iter()
        .map(|p| format!("{}: {}", p.code, p.message))
        .collect();
    format!(
        "{} problem(s), nothing stored: {}",
        problems.len(),
        listed.join("; ")
    )
}

/// `metric_import.preview`: parses and checks the CSV and returns the rows, the problems and the counts.
/// Writes nothing.
fn preview(store: &dyn MetricImportStore, input: Value) -> Result<Value, Fail> {
    let req: MetricImportPreviewInput = parse_input(input, "metric_import.preview")?;
    let platform = platform_of(&req.platform)?;
    let now = time::now_secs(req.now.as_deref())?;
    require_channel(store, &req.scope)?;
    let csv = parse_csv(&req.csv)?;
    let checked = check(store, &req.scope, &platform, &csv, &req.mapping, now)?;
    let observations: i64 = checked.rows.iter().map(|r| r.values.len() as i64).sum();
    let counts = MetricImportCounts {
        rows: csv.records.len() as i64,
        rows_with_problems: checked
            .problems
            .iter()
            .filter(|p| p.row > 0)
            .map(|p| p.row)
            .collect::<BTreeSet<_>>()
            .len() as i64,
        problems: checked.problems.len() as i64,
        observations,
    };
    to_value(&MetricImportPreviewOutput {
        scope: req.scope,
        platform,
        rows: checked.rows,
        problems: checked.problems,
        counts,
    })
}

/// `metric_import.commit`: stores every row of the import, or refuses the whole import. The same id with the
/// same request returns the stored import (`created` false) and writes nothing. The same id with a different
/// request is `import_mismatch`. A row problem is `import_invalid`, and nothing is stored.
fn commit(store: &dyn MetricImportStore, input: Value) -> Result<Value, Fail> {
    let req: MetricImportCommitInput = parse_input(input, "metric_import.commit")?;
    import_id_ok(&req.import_id)?;
    let source_note = req.source_note.trim().to_string();
    if source_note.is_empty() {
        return Err(bad("source_note must say where the file came from"));
    }
    let platform = platform_of(&req.platform)?;
    let now = time::now_secs(req.now.as_deref())?;
    // The request as compared on replay. Same request, same import.
    let request = json!({
        "csv": req.csv,
        "mapping": to_value(&req.mapping)?,
        "platform": platform,
        "source_note": source_note,
        "now": req.now,
    })
    .to_string();

    require_channel(store, &req.scope)?;
    if let Some((stored_request, record)) = store.load_import(&req.scope, &req.import_id)? {
        if stored_request == request {
            return to_value(&MetricImportCommitOutput {
                import: record,
                created: false,
            });
        }
        return Err((
            "import_mismatch",
            format!(
                "import {} already exists in this scope with different content; use a new import_id",
                req.import_id
            ),
        ));
    }

    let csv = parse_csv(&req.csv)?;
    if csv.records.is_empty() {
        return Err(bad("csv has no data rows"));
    }
    let checked = check(store, &req.scope, &platform, &csv, &req.mapping, now)?;
    if !checked.problems.is_empty() {
        return Err(("import_invalid", invalid_summary(&checked.problems)));
    }
    // With no problem every row has its job and its instant; a missing one is an internal fault, not data.
    let mut stored = Vec::with_capacity(checked.rows.len());
    for row in &checked.rows {
        let (Some(job_id), Some(observed_at)) = (row.job_id.as_deref(), row.observed_at.as_deref())
        else {
            return Err(internal("a checked row has no job or observation instant"));
        };
        stored.push((row, job_id, observed_at));
    }
    let observations: i64 = checked.rows.iter().map(|r| r.values.len() as i64).sum();
    let record = MetricImportRecord {
        scope: req.scope.clone(),
        import_id: req.import_id.clone(),
        platform,
        source_note,
        source: SOURCE.to_string(),
        created_at: time::format_instant(now),
        rows_written: checked.rows.len() as i64,
        observations_written: observations,
    };

    store.write_txn(&mut || {
        store.insert_import(&record, &request)?;
        for (row, job_id, observed_at) in &stored {
            for value in &row.values {
                store.insert_value(
                    &record,
                    &StoredValue {
                        row_no: row.row,
                        job_id,
                        remote_post_id: &row.post_id,
                        metric: &value.metric,
                        observed_at,
                        value: &value.value,
                    },
                )?;
            }
        }
        Ok(())
    })?;
    to_value(&MetricImportCommitOutput {
        import: record,
        created: true,
    })
}

/// `metric_import.list`: every import in scope, with its row and observation counts.
fn list(store: &dyn MetricImportStore, input: Value) -> Result<Value, Fail> {
    let req: MetricImportListInput = parse_input(input, "metric_import.list")?;
    require_channel(store, &req.scope)?;
    let imports = store.list_imports(&req.scope)?;
    to_value(&MetricImportListOutput {
        scope: req.scope,
        imports,
    })
}
