//! XLSX: read through calamine (typed cells, cached formula results), create through
//! rust_xlsxwriter, and edit in place by patching the sheet's `<sheetData>` byte ranges.
//!
//! An edit never loads the workbook into a model. Cells it does not set are copied as the exact
//! bytes they had; every other part is copied raw. The edit then makes three changes it reports:
//! the workbook is flagged to recalculate on open, the derived `calcChain.xml` is dropped (the
//! spreadsheet application rebuilds it), and rows it rewrote lose their `spans` hint.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};

use calamine::{Data, Reader, SheetType, SheetVisible, Xlsx};
use quick_xml::events::Event;

use crate::detect::Ooxml;
use crate::docx::EditOutcome;
use crate::error::{DocsError, Result};
use crate::model::{
    CellInput, CellRead, CellSet, CellValue, Loss, ReadOptions, SheetRead, SheetSpec, XlsxContent,
};
use crate::package::Package;
use crate::xmlutil::{
    attr, cell_name, dir_of, esc_attr, esc_text, local, parse_cell, parse_range, parse_rels,
    prefix_of, rels_name_for, resolve_target, Walk, MAX_COL, MAX_ROW,
};

// ---------------------------------------------------------------------------------------------
// Formula policy
// ---------------------------------------------------------------------------------------------

/// Refuse formulas that reach outside the workbook. The edit and create paths both call this.
pub(crate) fn check_formula(raw: &str) -> Result<String> {
    let f = raw.trim().strip_prefix('=').unwrap_or(raw.trim()).trim();
    if f.is_empty() {
        return Err(DocsError::InvalidOp("empty formula".into()));
    }
    if f.chars().count() > 8192 {
        return Err(DocsError::InvalidOp("formula over 8192 characters".into()));
    }
    // Scan outside string literals.
    let mut outside = String::new();
    let mut in_str = false;
    for c in f.chars() {
        if c == '"' {
            in_str = !in_str;
            continue;
        }
        if !in_str {
            outside.push(c);
        }
    }
    let lower = outside.to_ascii_lowercase();
    if outside.contains('|') {
        return Err(DocsError::UnsafeFormula(
            "DDE-style reference (a '|' in a reference)".into(),
        ));
    }
    for banned in ["webservice(", "filterxml(", "call(", "register(", "exec("] {
        if lower.contains(banned) {
            return Err(DocsError::UnsafeFormula(format!(
                "function {} reaches outside the workbook",
                banned.trim_end_matches('(').to_ascii_uppercase()
            )));
        }
    }
    let bytes = outside.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'[' {
            // `Table1[Col]` is a structured reference; `'[Book.xlsx]Sheet'!A1` and `[1]Sheet!A1`
            // are external workbook links.
            let prev = if i == 0 { b' ' } else { bytes[i - 1] };
            let digits = bytes[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            let external_index = digits > 0 && bytes.get(i + 1 + digits) == Some(&b']');
            if prev == b'\'' || external_index {
                return Err(DocsError::UnsafeFormula(
                    "external workbook reference".into(),
                ));
            }
        }
    }
    for ext in [".xls", ".xlsx", ".xlsm", ".xlsb"] {
        if lower.contains(ext) {
            return Err(DocsError::UnsafeFormula(
                "reference to another workbook file".into(),
            ));
        }
    }
    for scheme in ["http://", "https://", "file:", "ftp://", "\\\\"] {
        if lower.contains(scheme) {
            return Err(DocsError::UnsafeFormula(
                "network or file location in a reference".into(),
            ));
        }
    }
    Ok(f.to_string())
}

// ---------------------------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------------------------

pub(crate) struct XlsxRead {
    pub content: XlsxContent,
    pub truncated: bool,
    pub omitted: Vec<String>,
}

fn days_to_civil(z: i64) -> (i64, u32, u32) {
    // Howard Hinnant's civil_from_days; z = days since 1970-01-01.
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn serial_to_iso(serial: f64, date1904: bool) -> Option<String> {
    if !serial.is_finite() || serial < 0.0 || serial > 2_958_465.0 {
        return None;
    }
    let days = serial.floor();
    let secs = ((serial - days) * 86_400.0).round() as i64;
    let (days, secs) = if secs >= 86_400 {
        (days as i64 + 1, 0)
    } else {
        (days as i64, secs)
    };
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if !date1904 && days < 1 {
        // Time only.
        return Some(format!("{hh:02}:{mm:02}:{ss:02}"));
    }
    if !date1904 && days < 61 {
        return None; // 1900 leap-year quirk range
    }
    let epoch = if date1904 { -24_107 } else { -25_569 };
    let (y, m, d) = days_to_civil(days + epoch);
    if hh == 0 && mm == 0 && ss == 0 {
        Some(format!("{y:04}-{m:02}-{d:02}"))
    } else {
        Some(format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}"))
    }
}

fn to_value(d: &Data, date1904: bool) -> CellValue {
    match d {
        Data::Empty => CellValue::Empty,
        Data::String(s) => CellValue::Text(s.clone()),
        Data::Float(f) => CellValue::Number(*f),
        Data::Int(i) => CellValue::Number(*i as f64),
        Data::Bool(b) => CellValue::Bool(*b),
        Data::DateTime(dt) => {
            if dt.is_duration() {
                CellValue::Duration(dt.as_f64())
            } else {
                CellValue::DateTime {
                    serial: dt.as_f64(),
                    iso: serial_to_iso(dt.as_f64(), date1904),
                }
            }
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => CellValue::Text(s.clone()),
        Data::Error(e) => CellValue::Error(e.to_string()),
    }
}

fn count_part_elems(pkg: &Package<'_>, part: &str, name: &[u8]) -> Result<usize> {
    let Some(bytes) = pkg.read_opt(part)? else {
        return Ok(0);
    };
    let mut walk = Walk::new(&bytes, part);
    let mut n = 0;
    loop {
        let (ev, _, _) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == name => n += 1,
            _ => {}
        }
    }
    Ok(n)
}

fn a1_range(r1: u32, c1: u32, r2: u32, c2: u32) -> String {
    if (r1, c1) == (r2, c2) {
        cell_name(r1, c1)
    } else {
        format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
    }
}

pub(crate) fn read(o: &Ooxml<'_>, bytes: &[u8], opts: &ReadOptions) -> Result<XlsxRead> {
    let pkg = &o.pkg;
    let date1904 = {
        let wb = pkg.read(&o.main)?;
        let s = String::from_utf8_lossy(&wb);
        s.contains("date1904=\"1\"") || s.contains("date1904=\"true\"")
    };
    let selection =
        match &opts.range {
            Some(r) => Some(parse_range(r).ok_or_else(|| {
                DocsError::InvalidOp(format!("bad range {r:?} (use A1 or A1:C10)"))
            })?),
            None => None,
        };

    let parsed = catch_unwind(AssertUnwindSafe(|| -> Result<XlsxRead> {
        let mut wb: Xlsx<_> = calamine::open_workbook_from_rs(Cursor::new(bytes))
            .map_err(|e| DocsError::Malformed(format!("xlsx: {e}")))?;
        let meta: Vec<_> = wb.sheets_metadata().to_vec();
        let mut omitted = Vec::new();
        let worksheets: Vec<_> = meta
            .iter()
            .filter(|s| matches!(s.typ, SheetType::WorkSheet))
            .cloned()
            .collect();
        let other = meta.len() - worksheets.len();
        if other > 0 {
            omitted.push(format!(
                "non-worksheet sheets (chart, dialog, macro): {other}"
            ));
        }
        let chosen: Vec<_> = match &opts.sheets {
            None => worksheets,
            Some(names) => {
                let mut out = Vec::new();
                for n in names {
                    let found = worksheets
                        .iter()
                        .find(|s| &s.name == n)
                        .or_else(|| worksheets.iter().find(|s| s.name.eq_ignore_ascii_case(n)))
                        .ok_or_else(|| DocsError::NotFound {
                            kind: "sheet",
                            name: n.clone(),
                        })?;
                    out.push(found.clone());
                }
                out
            }
        };

        let mut remaining = opts.max_cells;
        let mut truncated = false;
        let mut sheets = Vec::new();
        for s in chosen {
            let range = wb
                .worksheet_range(&s.name)
                .map_err(|e| DocsError::Malformed(format!("sheet {}: {e}", s.name)))?;
            let formulas = wb
                .worksheet_formula(&s.name)
                .map_err(|e| DocsError::Malformed(format!("sheet {}: {e}", s.name)))?;
            let used_range = match (range.start(), range.end()) {
                (Some((r1, c1)), Some((r2, c2))) if !range.is_empty() => {
                    Some(a1_range(r1 + 1, c1 + 1, r2 + 1, c2 + 1))
                }
                _ => None,
            };
            let mut map: BTreeMap<(u32, u32), (CellValue, Option<String>)> = BTreeMap::new();
            if let Some((r0, c0)) = range.start() {
                for (r, c, d) in range.cells() {
                    let v = to_value(d, date1904);
                    if v != CellValue::Empty {
                        map.insert((r0 + r as u32 + 1, c0 + c as u32 + 1), (v, None));
                    }
                }
            }
            if let Some((r0, c0)) = formulas.start() {
                for (r, c, f) in formulas.cells() {
                    if f.is_empty() {
                        continue;
                    }
                    let key = (r0 + r as u32 + 1, c0 + c as u32 + 1);
                    let entry = map.entry(key).or_insert((CellValue::Empty, None));
                    // A formula written without a result carries an empty cached string.
                    if entry.0 == CellValue::Text(String::new()) {
                        entry.0 = CellValue::Empty;
                    }
                    entry.1 = Some(f.clone());
                }
            }
            let mut cells = Vec::new();
            let mut sheet_truncated = false;
            for ((row, col), (value, formula)) in map {
                if let Some((r1, c1, r2, c2)) = selection {
                    if row < r1 || row > r2 || col < c1 || col > c2 {
                        continue;
                    }
                }
                if remaining == 0 {
                    sheet_truncated = true;
                    truncated = true;
                    break;
                }
                remaining -= 1;
                cells.push(CellRead {
                    cell: cell_name(row, col),
                    row,
                    col,
                    value,
                    formula,
                });
            }
            sheets.push(SheetRead {
                name: s.name.clone(),
                state: match s.visible {
                    SheetVisible::Visible => "visible",
                    SheetVisible::Hidden => "hidden",
                    SheetVisible::VeryHidden => "veryHidden",
                }
                .to_string(),
                used_range,
                cells,
                truncated: sheet_truncated,
            });
        }
        Ok(XlsxRead {
            content: XlsxContent { sheets },
            truncated,
            omitted,
        })
    }));
    let mut result = match parsed {
        Ok(r) => r?,
        Err(_) => {
            return Err(DocsError::Malformed(
                "the workbook parser panicked on this file".into(),
            ))
        }
    };

    // What the cell grid does not carry.
    let count_prefix = |prefix: &str, suffix: &str| {
        pkg.names()
            .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
            .count()
    };
    let charts = count_prefix("xl/charts/chart", ".xml");
    if charts > 0 {
        result.omitted.push(format!("charts: {charts}"));
    }
    let mut comments = 0;
    for n in pkg
        .names()
        .filter(|n| n.starts_with("xl/comments") && n.ends_with(".xml"))
        .map(str::to_string)
        .collect::<Vec<_>>()
    {
        comments += count_part_elems(pkg, &n, b"comment")?;
    }
    if comments > 0 {
        result.omitted.push(format!("comments (notes): {comments}"));
    }
    let threaded = count_prefix("xl/threadedComments/", ".xml");
    if threaded > 0 {
        result
            .omitted
            .push(format!("threaded comment parts: {threaded}"));
    }
    let pivots = count_prefix("xl/pivotTables/", ".xml");
    if pivots > 0 {
        result.omitted.push(format!("pivot tables: {pivots}"));
    }
    let images = pkg.names().filter(|n| n.starts_with("xl/media/")).count();
    if images > 0 {
        result.omitted.push(format!("images: {images}"));
    }
    let external = count_prefix("xl/externalLinks/externalLink", ".xml");
    if external > 0 {
        result
            .omitted
            .push(format!("external workbook links not followed: {external}"));
    }
    if pkg.names().any(|n| n.starts_with("xl/tables/")) {
        result
            .omitted
            .push("table definitions (structure, filters)".into());
    }
    result
        .omitted
        .push("formulas: cached results are shown as stored; nothing is recalculated".into());
    if date1904 {
        result.omitted.push("1904 date system in use".into());
    }
    Ok(result)
}

fn value_text(v: &CellValue, formula: &Option<String>) -> String {
    match v {
        CellValue::Empty => formula
            .as_ref()
            .map(|f| format!("={f}"))
            .unwrap_or_default(),
        CellValue::Text(s) => s.clone(),
        CellValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        CellValue::DateTime { serial, iso } => iso.clone().unwrap_or_else(|| serial.to_string()),
        CellValue::Duration(d) => d.to_string(),
        CellValue::Error(e) => e.clone(),
    }
}

/// Plain text and markdown over the cells returned (the selection), one block per sheet.
pub(crate) fn render(content: &XlsxContent) -> (String, String) {
    let mut text = Vec::new();
    let mut md = Vec::new();
    for s in &content.sheets {
        text.push(format!("[{}]", s.name));
        md.push(format!("## {}", s.name));
        if s.cells.is_empty() {
            continue;
        }
        let min_c = s.cells.iter().map(|c| c.col).min().unwrap_or(1);
        let max_c = s.cells.iter().map(|c| c.col).max().unwrap_or(1);
        let mut rows: BTreeMap<u32, BTreeMap<u32, String>> = BTreeMap::new();
        for c in &s.cells {
            rows.entry(c.row)
                .or_default()
                .insert(c.col, value_text(&c.value, &c.formula));
        }
        let mut md_rows = Vec::new();
        for cols in rows.values() {
            let line: Vec<String> = (min_c..=max_c)
                .map(|c| cols.get(&c).cloned().unwrap_or_default())
                .collect();
            text.push(line.join("\t"));
            md_rows.push(format!(
                "| {} |",
                line.iter()
                    .map(|v| v.replace('|', "\\|").replace('\n', "<br>"))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        let width = (max_c - min_c + 1) as usize;
        if let Some(first) = md_rows.first().cloned() {
            md.push(first);
            md.push(format!("| {} |", vec!["---"; width].join(" | ")));
            md.extend(md_rows.into_iter().skip(1));
        }
    }
    (text.join("\n"), md.join("\n"))
}

// ---------------------------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------------------------

pub(crate) fn create(sheets: &[SheetSpec]) -> Result<Vec<u8>> {
    use rust_xlsxwriter::{Formula, Workbook};
    if sheets.is_empty() {
        return Err(DocsError::InvalidOp(
            "a workbook needs at least one sheet".into(),
        ));
    }
    let mut wb = Workbook::new();
    for spec in sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&spec.name)
            .map_err(|e| DocsError::InvalidOp(format!("sheet name {:?}: {e}", spec.name)))?;
        for (r, row) in spec.rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let (r, c) = (r as u32, c as u16);
                if r + 1 > MAX_ROW || (c as u32) + 1 > MAX_COL {
                    return Err(DocsError::InvalidOp(
                        "cell outside the worksheet grid".into(),
                    ));
                }
                let res = match v {
                    CellInput::Text(s) => {
                        ws.write_string(r, c, crate::xmlutil::clean(s)).map(|_| ())
                    }
                    CellInput::Number(n) => {
                        if !n.is_finite() {
                            return Err(DocsError::InvalidOp("non-finite number".into()));
                        }
                        ws.write_number(r, c, *n).map(|_| ())
                    }
                    CellInput::Bool(b) => ws.write_boolean(r, c, *b).map(|_| ()),
                    CellInput::Formula(f) => {
                        let f = check_formula(f)?;
                        ws.write_formula(r, c, Formula::new(&f).set_result(""))
                            .map(|_| ())
                    }
                    CellInput::Blank => Ok(()),
                };
                res.map_err(|e| {
                    DocsError::InvalidOp(format!("{}: {e}", cell_name(r + 1, c as u32 + 1)))
                })?;
            }
        }
    }
    let bytes = wb
        .save_to_buffer()
        .map_err(|e| DocsError::InvalidOp(format!("xlsx write: {e}")))?;
    strip_placeholder_results(&bytes)
}

/// rust_xlsxwriter stores `<v>0</v>` as the result of a formula it was given no result for. A
/// reader would then report a computed 0 that nobody computed, so those placeholders are removed:
/// a formula this crate writes has no stored result until a spreadsheet application calculates it.
fn strip_placeholder_results(bytes: &[u8]) -> Result<Vec<u8>> {
    let pkg = Package::open(bytes, &crate::limits::Limits::default())?;
    let mut replace = BTreeMap::new();
    for name in pkg
        .names()
        .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
    {
        let data = pkg.read(name)?;
        let text = String::from_utf8_lossy(&data);
        if text.contains("</f><v>0</v></c>") {
            replace.insert(
                name.to_string(),
                text.replace("</f><v>0</v></c>", "</f></c>").into_bytes(),
            );
        }
    }
    if replace.is_empty() {
        return Ok(bytes.to_vec());
    }
    pkg.rewrite(&replace, &BTreeSet::new(), &[])
}

// ---------------------------------------------------------------------------------------------
// Edit in place
// ---------------------------------------------------------------------------------------------

struct CellRec {
    col: u32,
    range: (usize, usize),
    populated: bool,
    style: Option<String>,
    /// The cell holds a shared-formula master or an array formula; overwriting it would break
    /// the cells that depend on it.
    locked_formula: bool,
}

struct RowRec {
    num: u32,
    range: (usize, usize),
    tag: (usize, usize),
    cells: Vec<CellRec>,
}

struct SheetScan {
    rows: Vec<RowRec>,
    /// Range of the whole `<sheetData>` element and, when it has a body, of that body.
    sheet_data: (usize, usize),
    body: Option<(usize, usize)>,
    prefix: String,
    dimension: Option<(usize, usize)>,
    has_table_parts: bool,
}

/// Range of the `sheetData` element, the range of its body when it has one, and its name prefix.
type SheetDataScan = ((usize, usize), Option<(usize, usize)>, String);

fn scan_sheet(xml: &[u8], part: &str) -> Result<SheetScan> {
    let mut walk = Walk::new(xml, part);
    let mut depth = 0usize;
    let mut sd_depth: Option<usize> = None;
    let mut sd_start = 0usize;
    let mut sd: Option<SheetDataScan> = None;
    let mut rows: Vec<RowRec> = Vec::new();
    let mut cur_row: Option<RowRec> = None;
    let mut cur_cell: Option<(usize, CellRec)> = None;
    let mut last_row_num = 0u32;
    let mut dimension = None;
    let mut has_table_parts = false;
    let mut body_start = 0usize;
    loop {
        let (ev, start, end) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                let name = e.name();
                let l = local(name.as_ref());
                if l == b"sheetData" && sd_depth.is_none() {
                    sd_depth = Some(depth);
                    sd_start = start;
                    body_start = end;
                    sd = Some(((start, end), None, prefix_of(name.as_ref())));
                } else if let Some(sdd) = sd_depth {
                    if l == b"row" && depth == sdd + 1 {
                        let num = attr(&e, "r")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(last_row_num + 1);
                        last_row_num = num;
                        cur_row = Some(RowRec {
                            num,
                            range: (start, end),
                            tag: (start, end),
                            cells: Vec::new(),
                        });
                    } else if l == b"c" && depth == sdd + 2 {
                        let col = cell_col(&e, cur_row.as_ref());
                        cur_cell = Some((
                            depth,
                            CellRec {
                                col,
                                range: (start, end),
                                populated: false,
                                style: attr(&e, "s"),
                                locked_formula: false,
                            },
                        ));
                    } else if let Some((cd, rec)) = cur_cell.as_mut() {
                        if depth > *cd {
                            inspect_cell_child(&e, l, rec);
                        }
                    }
                } else if l == b"tableParts" {
                    has_table_parts = true;
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                let l = local(name.as_ref());
                if l == b"dimension" && sd_depth.is_none() {
                    dimension = Some((start, end));
                } else if l == b"sheetData" && sd_depth.is_none() {
                    sd = Some(((start, end), None, prefix_of(name.as_ref())));
                } else if l == b"tableParts" {
                    has_table_parts = true;
                } else if let Some(sdd) = sd_depth {
                    if l == b"row" && depth == sdd {
                        let num = attr(&e, "r")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(last_row_num + 1);
                        last_row_num = num;
                        rows.push(RowRec {
                            num,
                            range: (start, end),
                            tag: (start, end),
                            cells: Vec::new(),
                        });
                    } else if l == b"c" && depth == sdd + 1 {
                        let col = cell_col(&e, cur_row.as_ref());
                        if let Some(r) = cur_row.as_mut() {
                            r.cells.push(CellRec {
                                col,
                                range: (start, end),
                                populated: false,
                                style: attr(&e, "s"),
                                locked_formula: false,
                            });
                        }
                    } else if let Some((cd, rec)) = cur_cell.as_mut() {
                        if depth >= *cd {
                            inspect_cell_child(&e, l, rec);
                        }
                    }
                }
            }
            Event::End(e) => {
                let name = e.name();
                let l = local(name.as_ref());
                if let Some(sdd) = sd_depth {
                    if l == b"c" && depth == sdd + 2 {
                        if let Some((_, mut rec)) = cur_cell.take() {
                            rec.range.1 = end;
                            if let Some(r) = cur_row.as_mut() {
                                r.cells.push(rec);
                            }
                        }
                    } else if l == b"row" && depth == sdd + 1 {
                        if let Some(mut r) = cur_row.take() {
                            r.range.1 = end;
                            rows.push(r);
                        }
                    } else if l == b"sheetData" && depth == sdd {
                        if let Some(s) = sd.as_mut() {
                            s.0 = (sd_start, end);
                            s.1 = Some((body_start, start));
                        }
                        sd_depth = None;
                    }
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let (sheet_data, body, prefix) =
        sd.ok_or_else(|| DocsError::Malformed(format!("{part}: no sheetData element")))?;
    let mut seen = BTreeSet::new();
    for r in &rows {
        if !seen.insert(r.num) {
            return Err(DocsError::Malformed(format!(
                "{part}: row {} appears twice",
                r.num
            )));
        }
    }
    rows.sort_by_key(|r| r.num);
    Ok(SheetScan {
        rows,
        sheet_data,
        body,
        prefix,
        dimension,
        has_table_parts,
    })
}

fn cell_col(e: &quick_xml::events::BytesStart<'_>, row: Option<&RowRec>) -> u32 {
    if let Some(r) = attr(e, "r") {
        if let Some((_, c)) = parse_cell(&r) {
            return c;
        }
    }
    row.and_then(|r| r.cells.last().map(|c| c.col + 1))
        .unwrap_or(1)
}

fn inspect_cell_child(e: &quick_xml::events::BytesStart<'_>, l: &[u8], rec: &mut CellRec) {
    match l {
        b"v" | b"is" => rec.populated = true,
        b"f" => {
            rec.populated = true;
            let t = attr(e, "t");
            if (t.as_deref() == Some("shared") && attr(e, "ref").is_some())
                || t.as_deref() == Some("array")
            {
                rec.locked_formula = true;
            }
        }
        _ => {}
    }
}

fn cell_xml(
    p: &str,
    row: u32,
    col: u32,
    v: &CellInput,
    style: Option<&str>,
) -> Result<Option<String>> {
    let r = cell_name(row, col);
    let s = style
        .map(|s| format!(" s=\"{}\"", esc_attr(s)))
        .unwrap_or_default();
    Ok(match v {
        CellInput::Text(t) => {
            if t.chars().count() > 32_767 {
                return Err(DocsError::InvalidOp(format!(
                    "{r}: text over 32767 characters"
                )));
            }
            Some(format!(
                "<{p}c r=\"{r}\"{s} t=\"inlineStr\"><{p}is><{p}t xml:space=\"preserve\">{}</{p}t></{p}is></{p}c>",
                esc_text(t)
            ))
        }
        CellInput::Number(n) => {
            if !n.is_finite() {
                return Err(DocsError::InvalidOp(format!("{r}: non-finite number")));
            }
            Some(format!("<{p}c r=\"{r}\"{s}><{p}v>{n}</{p}v></{p}c>"))
        }
        CellInput::Bool(b) => Some(format!(
            "<{p}c r=\"{r}\"{s} t=\"b\"><{p}v>{}</{p}v></{p}c>",
            if *b { 1 } else { 0 }
        )),
        CellInput::Formula(f) => {
            let f = check_formula(f)?;
            Some(format!(
                "<{p}c r=\"{r}\"{s}><{p}f>{}</{p}f></{p}c>",
                esc_text(&f)
            ))
        }
        CellInput::Blank => style.map(|_| format!("<{p}c r=\"{r}\"{s}/>")),
    })
}

fn strip_spans(tag: &str) -> String {
    if let Some(i) = tag.find(" spans=\"") {
        if let Some(j) = tag[i + 8..].find('"') {
            return format!("{}{}", &tag[..i], &tag[i + 8 + j + 1..]);
        }
    }
    tag.to_string()
}

struct SheetEdit {
    sets: Vec<(u32, u32, CellInput)>,
    append: Vec<Vec<CellInput>>,
}

/// Resolve a sheet name to its part path through `workbook.xml` and its relationships.
fn sheet_part(o: &Ooxml<'_>, name: &str) -> Result<String> {
    let wb = o.pkg.read(&o.main)?;
    let mut walk = Walk::new(&wb, &o.main);
    let mut found: Vec<(String, String)> = Vec::new();
    loop {
        let (ev, _, _) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == b"sheet" => {
                if let (Some(n), Some(id)) = (attr(&e, "name"), attr(&e, "id")) {
                    found.push((n, id));
                }
            }
            _ => {}
        }
    }
    let rid = found
        .iter()
        .find(|(n, _)| n == name)
        .or_else(|| found.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)))
        .map(|(_, id)| id.clone())
        .ok_or_else(|| DocsError::NotFound {
            kind: "sheet",
            name: name.to_string(),
        })?;
    let rels_part = rels_name_for(&o.main);
    let rels = parse_rels(&o.pkg.read(&rels_part)?, &rels_part)?;
    let rel = rels
        .iter()
        .find(|r| r.id == rid && !r.external)
        .ok_or_else(|| DocsError::Malformed(format!("sheet {name}: relationship {rid} missing")))?;
    if !rel.ty.ends_with("/worksheet") {
        return Err(DocsError::InvalidOp(format!(
            "sheet {name} is not a worksheet"
        )));
    }
    Ok(resolve_target(dir_of(&o.main), &rel.target))
}

/// Set `fullCalcOnLoad="1"` on the workbook's `calcPr`, adding one in schema order if absent.
fn force_recalc(xml: &[u8], part: &str) -> Result<Vec<u8>> {
    const AFTER: [&[u8]; 9] = [
        b"oleSize",
        b"customWorkbookViews",
        b"pivotCaches",
        b"smartTagPr",
        b"smartTagTypes",
        b"webPublishing",
        b"fileRecoveryPr",
        b"webPublishObjects",
        b"extLst",
    ];
    let mut walk = Walk::new(xml, part);
    let mut depth = 0usize;
    let mut insert_at: Option<usize> = None;
    let mut end_root: Option<usize> = None;
    let mut prefix = String::new();
    let mut calc: Option<(usize, usize)> = None;
    loop {
        let (ev, start, end) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                if depth == 1 {
                    prefix = prefix_of(e.name().as_ref());
                } else if depth == 2 {
                    let name = e.name();
                    let l = local(name.as_ref());
                    if l == b"calcPr" {
                        calc = Some((start, end));
                    } else if AFTER.contains(&l) && insert_at.is_none() {
                        insert_at = Some(start);
                    }
                }
            }
            Event::Empty(e) => {
                if depth == 1 {
                    let name = e.name();
                    let l = local(name.as_ref());
                    if l == b"calcPr" {
                        calc = Some((start, end));
                    } else if AFTER.contains(&l) && insert_at.is_none() {
                        insert_at = Some(start);
                    }
                }
            }
            Event::End(_) => {
                if depth == 1 {
                    end_root = Some(start);
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(xml.len() + 40);
    if let Some((s, e)) = calc {
        let tag = String::from_utf8_lossy(&xml[s..e]).into_owned();
        let new_tag = if let Some(i) = tag.find("fullCalcOnLoad=\"") {
            let rest = &tag[i + 16..];
            let close = rest.find('"').unwrap_or(0);
            format!("{}fullCalcOnLoad=\"1{}", &tag[..i], &rest[close..])
        } else {
            let cut = if tag.ends_with("/>") {
                tag.len() - 2
            } else {
                tag.len() - 1
            };
            format!(
                "{} fullCalcOnLoad=\"1\"{}",
                tag[..cut].trim_end(),
                &tag[cut..]
            )
        };
        out.extend_from_slice(&xml[..s]);
        out.extend_from_slice(new_tag.as_bytes());
        out.extend_from_slice(&xml[e..]);
    } else {
        let at = insert_at
            .or(end_root)
            .ok_or_else(|| DocsError::Malformed(format!("{part}: no root element")))?;
        out.extend_from_slice(&xml[..at]);
        out.extend_from_slice(format!("<{prefix}calcPr fullCalcOnLoad=\"1\"/>").as_bytes());
        out.extend_from_slice(&xml[at..]);
    }
    Ok(out)
}

/// Drop the Override or Relationship element that names `target` from a content-types part or a
/// relationships part. Returns the bytes unchanged when none matches.
fn drop_element_naming(
    xml: &[u8],
    part: &str,
    el: &[u8],
    key: &str,
    value_ends_with: &str,
) -> Result<Vec<u8>> {
    let mut walk = Walk::new(xml, part);
    let mut cut: Option<(usize, usize)> = None;
    loop {
        let (ev, start, end) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Empty(e)
                if local(e.name().as_ref()) == el
                    && attr(&e, key).is_some_and(|v| v.ends_with(value_ends_with)) =>
            {
                cut = Some((start, end));
            }
            _ => {}
        }
    }
    Ok(match cut {
        Some((s, e)) => [&xml[..s], &xml[e..]].concat(),
        None => xml.to_vec(),
    })
}

fn edit_sheet(o: &Ooxml<'_>, sheet: &str, plan: SheetEdit) -> Result<EditOutcome> {
    let pkg = &o.pkg;
    let part = sheet_part(o, sheet)?;
    let xml = pkg.read(&part)?;
    let scan = scan_sheet(&xml, &part)?;
    let p = scan.prefix.clone();

    // Row number -> (existing record, edits by column).
    let mut edits: BTreeMap<u32, BTreeMap<u32, Option<String>>> = BTreeMap::new();
    let existing: BTreeMap<u32, &RowRec> = scan.rows.iter().map(|r| (r.num, r)).collect();
    let mut cells_set = 0usize;

    for (row, col, v) in &plan.sets {
        let style = existing
            .get(row)
            .and_then(|r| r.cells.iter().find(|c| c.col == *col))
            .map(|c| (c.style.clone(), c.locked_formula));
        if let Some((_, true)) = style {
            return Err(DocsError::InvalidOp(format!(
                "{} holds a shared or array formula; overwriting it would break the cells that depend on it",
                cell_name(*row, *col)
            )));
        }
        let style = style.and_then(|(s, _)| s);
        let xml = cell_xml(&p, *row, *col, v, style.as_deref())?;
        edits.entry(*row).or_default().insert(*col, xml);
        cells_set += 1;
    }

    let mut rows_appended = 0usize;
    if !plan.append.is_empty() {
        let last = scan
            .rows
            .iter()
            .filter(|r| r.cells.iter().any(|c| c.populated) || edits.contains_key(&r.num))
            .map(|r| r.num)
            .max()
            .unwrap_or(0)
            .max(edits.keys().copied().max().unwrap_or(0));
        for (next, row) in (last + 1..).zip(&plan.append) {
            if next > MAX_ROW {
                return Err(DocsError::InvalidOp(
                    "rows past the end of the worksheet".into(),
                ));
            }
            for (i, v) in row.iter().enumerate() {
                let col = i as u32 + 1;
                if col > MAX_COL {
                    return Err(DocsError::InvalidOp(
                        "columns past the end of the worksheet".into(),
                    ));
                }
                let xml = cell_xml(&p, next, col, v, None)?;
                edits.entry(next).or_default().insert(col, xml);
            }
            rows_appended += 1;
        }
    }

    // Rebuild the row list.
    let mut all_rows: BTreeSet<u32> = existing.keys().copied().collect();
    all_rows.extend(edits.keys().copied());
    let mut body_bytes: Vec<u8> = Vec::new();
    let (mut min_r, mut min_c, mut max_r, mut max_c) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut note = |r: u32, c: u32| {
        min_r = min_r.min(r);
        max_r = max_r.max(r);
        min_c = min_c.min(c);
        max_c = max_c.max(c);
    };
    for num in all_rows {
        let rec = existing.get(&num);
        match (rec, edits.get(&num)) {
            (Some(r), None) => {
                body_bytes.extend_from_slice(&xml[r.range.0..r.range.1]);
                for c in &r.cells {
                    note(num, c.col);
                }
            }
            (rec, Some(ed)) => {
                // (column -> exact bytes), existing cells first, edits on top.
                let mut cols: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
                let tag = match rec {
                    Some(r) => {
                        for c in &r.cells {
                            cols.insert(c.col, xml[c.range.0..c.range.1].to_vec());
                        }
                        let t = String::from_utf8_lossy(&xml[r.tag.0..r.tag.1]).into_owned();
                        let t = strip_spans(&t);
                        if t.ends_with("/>") {
                            format!("{}>", t[..t.len() - 2].trim_end())
                        } else {
                            t
                        }
                    }
                    None => format!("<{p}row r=\"{num}\">"),
                };
                for (col, new) in ed {
                    match new {
                        Some(x) => {
                            cols.insert(*col, x.clone().into_bytes());
                        }
                        None => {
                            cols.remove(col);
                        }
                    }
                }
                body_bytes.extend_from_slice(tag.as_bytes());
                for (col, bytes) in cols {
                    note(num, col);
                    body_bytes.extend_from_slice(&bytes);
                }
                body_bytes.extend_from_slice(format!("</{p}row>").as_bytes());
            }
            (None, None) => {}
        }
    }
    let sd_name = format!("{p}sheetData");
    let mut out = Vec::with_capacity(xml.len() + body_bytes.len());
    // Dimension and sheetData are replaced; the rest is the original bytes.
    let mut patches: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    match scan.body {
        Some((bs, be)) => patches.push((bs, be, body_bytes)),
        None => {
            let (s, e) = scan.sheet_data;
            let mut el = format!("<{sd_name}>").into_bytes();
            el.extend_from_slice(&body_bytes);
            el.extend_from_slice(format!("</{sd_name}>").as_bytes());
            patches.push((s, e, el));
        }
    }
    if let Some((s, e)) = scan.dimension {
        let dim = if max_r == 0 {
            "A1".to_string()
        } else {
            a1_range(min_r, min_c, max_r, max_c)
        };
        patches.push((s, e, format!("<{p}dimension ref=\"{dim}\"/>").into_bytes()));
    }
    patches.sort_by_key(|x| x.0);
    let mut at = 0;
    for (s, e, bytes) in patches {
        out.extend_from_slice(&xml[at..s]);
        out.extend_from_slice(&bytes);
        at = e;
    }
    out.extend_from_slice(&xml[at..]);

    let mut replace: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut remove: BTreeSet<String> = BTreeSet::new();
    let mut losses = Vec::new();
    let mut warnings = Vec::new();
    replace.insert(part.clone(), out);

    // Recalculate on open, and drop the derived calcChain so it cannot disagree with the cells.
    let wb_xml = pkg.read(&o.main)?;
    replace.insert(o.main.clone(), force_recalc(&wb_xml, &o.main)?);
    warnings.push(
        "the workbook is flagged to recalculate on open; cached results of existing formulas stay as stored until a spreadsheet application opens it"
            .into(),
    );
    let rels_part = rels_name_for(&o.main);
    if let Some(rels_bytes) = pkg.read_opt(&rels_part)? {
        let rels = parse_rels(&rels_bytes, &rels_part)?;
        if let Some(chain) = rels
            .iter()
            .find(|r| r.ty.ends_with("/calcChain") && !r.external)
        {
            let chain_part = resolve_target(dir_of(&o.main), &chain.target);
            let new_rels = drop_element_naming(
                &rels_bytes,
                &rels_part,
                b"Relationship",
                "Target",
                chain.target.as_str(),
            )?;
            replace.insert(rels_part.clone(), new_rels);
            if let Some(ct) = pkg.read_opt("[Content_Types].xml")? {
                let new_ct = drop_element_naming(
                    &ct,
                    "[Content_Types].xml",
                    b"Override",
                    "PartName",
                    "calcChain.xml",
                )?;
                replace.insert("[Content_Types].xml".into(), new_ct);
            }
            remove.insert(chain_part.clone());
            losses.push(Loss {
                part: chain_part,
                kind: "derived_part_dropped".into(),
                detail: "the calculation chain is rebuilt by the spreadsheet application on open"
                    .into(),
            });
        }
    }
    if rows_appended > 0 && scan.has_table_parts {
        warnings.push(
            "the sheet has table objects; appended rows are not added to a table's range".into(),
        );
    }
    if pkg.names().any(|n| n.starts_with("_xmlsignatures/")) {
        losses.push(Loss {
            part: "_xmlsignatures/".into(),
            kind: "signature_invalidated".into(),
            detail: "the package has a digital signature; editing invalidates it".into(),
        });
    }
    let touched: Vec<String> = replace
        .keys()
        .cloned()
        .chain(remove.iter().cloned())
        .collect();
    let bytes = pkg.rewrite(&replace, &remove, &[])?;
    Ok(EditOutcome {
        bytes,
        touched,
        count: cells_set + rows_appended,
        warnings,
        losses,
    })
}

pub(crate) fn set_cells(
    o: &Ooxml<'_>,
    sheet: &str,
    cells: &[CellSet],
) -> Result<(EditOutcome, usize)> {
    if cells.is_empty() {
        return Err(DocsError::InvalidOp("no cells to set".into()));
    }
    let mut sets = Vec::new();
    for c in cells {
        let (row, col) = parse_cell(&c.cell)
            .ok_or_else(|| DocsError::InvalidOp(format!("bad cell address {:?}", c.cell)))?;
        sets.push((row, col, c.value.clone()));
    }
    let n = sets.len();
    Ok((
        edit_sheet(
            o,
            sheet,
            SheetEdit {
                sets,
                append: Vec::new(),
            },
        )?,
        n,
    ))
}

pub(crate) fn append_rows(
    o: &Ooxml<'_>,
    sheet: &str,
    rows: &[Vec<CellInput>],
) -> Result<(EditOutcome, usize)> {
    if rows.is_empty() {
        return Err(DocsError::InvalidOp("no rows to append".into()));
    }
    let n = rows.len();
    Ok((
        edit_sheet(
            o,
            sheet,
            SheetEdit {
                sets: Vec::new(),
                append: rows.to_vec(),
            },
        )?,
        n,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_serials_become_iso_dates_in_both_date_systems() {
        assert_eq!(
            serial_to_iso(45_000.0, false).as_deref(),
            Some("2023-03-15")
        );
        assert_eq!(serial_to_iso(61.0, false).as_deref(), Some("1900-03-01"));
        assert_eq!(
            serial_to_iso(60.0, false),
            None,
            "the 1900 leap-year quirk is not a real date"
        );
        assert_eq!(serial_to_iso(0.5, false).as_deref(), Some("12:00:00"));
        assert_eq!(
            serial_to_iso(45_000.25, false).as_deref(),
            Some("2023-03-15T06:00:00")
        );
        // 1904 system: serial 0 is 1904-01-01.
        assert_eq!(serial_to_iso(0.0, true).as_deref(), Some("1904-01-01"));
        assert_eq!(serial_to_iso(f64::NAN, false), None);
        assert_eq!(serial_to_iso(-1.0, false), None);
        assert_eq!(serial_to_iso(3e9, false), None);
    }

    #[test]
    fn formula_policy_table() {
        for ok in [
            "SUM(A1:A3)",
            "=A1+B1",
            "=IF(A1>1,\"http://text-only\",0)",
            "=Sheet2!A1",
            "='My Sheet'!A1",
            "=Table1[Sales]",
            "=Table1[[#Headers],[Sales]]",
            "=INDEX(A:A,MATCH(\"x\",B:B,0))",
        ] {
            assert!(check_formula(ok).is_ok(), "{ok}");
        }
        for bad in [
            "=WEBSERVICE(\"http://x\")",
            "=webservice(A1)",
            "=FILTERXML(A1,\"//a\")",
            "=CALL(\"x\")",
            "=REGISTER(\"x\")",
            "=EXEC(\"calc\")",
            "=cmd|'/c calc'!A0",
            "='[Book.xlsx]S'!A1",
            "=[1]Sheet1!A1",
            "=SUM('C:\\x\\[a.xls]S'!A1)",
            "=A1&\\\\host\\share",
            "=",
        ] {
            assert!(check_formula(bad).is_err(), "{bad}");
        }
        assert_eq!(check_formula("= SUM(A1)").unwrap(), "SUM(A1)");
    }
}
