//! Static source lint for app repositories (ADR-0001 item 9; `journeys check`, dispatch W2-H).
//!
//! Pure: the input is repository file paths with their contents, plus the caller's options. The
//! output is a [`Report`]. Nothing here reads the filesystem, calls `gh`, or compiles or runs code.
//! The caller supplies the files, the scenario declarations, the bridge entries, today's date and
//! the frozen baseline, and decides what the report means for the gate.
//!
//! Rules (identifiers in [`rule`]):
//!
//! 1. **Test tags.** Each `#[test]` / `#[tokio::test]` function needs a `// rk-test: pure`,
//!    `// rk-test: contract` or `// rk-test: pin #<n>` comment directly above its attribute stack.
//!    Other `#[...]` attributes and `///` doc lines may sit between the tag and the test; a blank
//!    line or any other line breaks the link. An untagged test is reported, but it is not a failure
//!    by itself (rule 2 decides). A malformed tag fails. A `pure` test whose body spawns processes
//!    or touches temp directories fails as misclassified. `pin #<n>` numbers are returned in
//!    [`Report::pins`]; checking that each issue exists is the caller's job (`gh`), out of scope here.
//! 2. **Baseline.** [`check_baseline`] fails only when the untagged count rises above the frozen
//!    number. A fall is never a failure.
//! 3. **Aggregate suite loops.** A test whose body calls `run_suite(` or loops over scenario files
//!    fails (the PostRight shape, `crates/e2e/tests/scenarios.rs:2554`).
//! 4. **Undeclared resource tokens.** A scenario whose step kinds use a machine-wide resource
//!    (`clipboard`, `focus`, `audio`, `gui`, `credentials`) must declare the matching token
//!    (`clipboard`, `focus`, `audio`, `gui-session`, `credentials`). An unknown declared token fails.
//! 5. **No binary launches from tests (Sage C8).** A test that launches a process fails unless a
//!    [`BridgeEntry`] for its file (and for its test, when the entry names one) has a removal date
//!    that is not yet past. An expired or malformed entry does not exempt.
//!
//! Heuristics, stated so the gate is read honestly:
//! * Source is first reduced by an internal code view that blanks comments and the contents of
//!   string and char literals, so tokens inside strings and comments never match. Brace matching
//!   runs on that view.
//! * Rules 1 (`pure` misclassification) and 5 look for fixed tokens in a test's own body. They do
//!   not follow helper functions, macros or `use` aliases, so a launch hidden behind a helper is missed.
//! * Rule 3 reads the header of each `for` loop. A loop whose iterable mentions `scenario` (and not
//!   `step`), or enumerates files with `read_dir`, `glob(` or `walkdir`, counts.
//! * Rule 4 maps a step kind by its first segment (`gui_click` maps to `gui`). Kinds outside the
//!   table are not checked.

/// One source file: a repository-relative path and its full text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

/// A scenario's declared resource tokens and its step kinds in step order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioTokens {
    /// Where the scenario is declared, for the report.
    pub path: String,
    pub id: String,
    pub declared: Vec<String>,
    pub steps: Vec<String>,
}

/// A file (and optionally one test) that may still launch binaries until `removal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeEntry {
    pub path: String,
    pub test: Option<String>,
    /// Last day of the exemption, `YYYY-MM-DD`. The exemption holds while today is not after it.
    pub removal: String,
}

/// Inputs besides the source files.
#[derive(Debug, Clone, Default)]
pub struct LintOptions {
    /// Today's date, `YYYY-MM-DD`. An empty or malformed date refuses every bridge exemption.
    pub today: String,
    /// The frozen untagged count. `None` disables the baseline comparison.
    pub baseline_untagged: Option<usize>,
    pub bridge: Vec<BridgeEntry>,
    pub scenarios: Vec<ScenarioTokens>,
}

/// Whether a finding fails the lint or is only listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Fail,
    Report,
}

/// One problem or note. `line` is 1-based, or 0 when the finding has no source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub path: String,
    pub line: usize,
    pub subject: Option<String>,
    pub message: String,
}

/// A `// rk-test: pin #<n>` tag, returned for the caller's `gh` check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRef {
    pub path: String,
    pub line: usize,
    pub test: String,
    pub issue: u64,
}

/// The result of one lint run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Every `#[test]` function found in the `.rs` files.
    pub tests: usize,
    /// Tests with a valid `pure`, `contract` or `pin` tag.
    pub tagged: usize,
    /// Tests without a valid tag. This is the number compared with the frozen baseline.
    pub untagged: usize,
    pub pins: Vec<PinRef>,
}

impl Report {
    /// True when any finding has [`Severity::Fail`].
    pub fn failed(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Fail)
    }
}

/// Finding identifiers.
pub mod rule {
    /// A test has no valid `rk-test:` tag directly above it. Reported; counted against the baseline.
    pub const UNTAGGED_TEST: &str = "untagged_test";
    /// An `rk-test:` tag is present but is not `pure`, `contract` or `pin #<n>`.
    pub const INVALID_TAG: &str = "invalid_tag";
    /// A `pure` test spawns processes or touches temp directories.
    pub const PURE_MISCLASSIFIED: &str = "pure_misclassified";
    /// The untagged count rose above the frozen baseline.
    pub const UNTAGGED_ROSE: &str = "untagged_rose";
    /// A test calls `run_suite(` or loops over scenario files.
    pub const AGGREGATE_SUITE: &str = "aggregate_suite";
    /// A scenario uses a machine-wide resource without declaring its token.
    pub const UNDECLARED_TOKEN: &str = "undeclared_token";
    /// A scenario declares a token that is not one of the known tokens.
    pub const UNKNOWN_TOKEN: &str = "unknown_token";
    /// A test launches a process and no live bridge entry covers it.
    pub const LAUNCHES_BINARY: &str = "launches_binary";
    /// A test launches a process under a live bridge entry. Informational.
    pub const BRIDGE_EXEMPT: &str = "bridge_exempt";
    /// The options are unusable. For example, `today` is not a date.
    pub const BAD_INPUT: &str = "bad_input";
}

/// Tokens that mark a process launch in a test body.
const PROCESS_TOKENS: &[&str] = &["Command::new", "process::Command", "OwnedCommand"];
/// Tokens that mark a temp-directory use in a test body.
const TEMP_TOKENS: &[&str] = &["temp_dir", "tempfile", "TempDir", "tempdir"];
/// Calls that enumerate files, matched against a lowercased loop iterable.
const ENUM_TOKENS: &[&str] = &["read_dir", "glob(", "walkdir"];
/// The resource tokens a scenario may declare.
const KNOWN_TOKENS: &[&str] = &["focus", "audio", "gui-session", "clipboard", "credentials"];

/// Runs every rule over `files` and the options. Only `.rs` files are read.
pub fn lint_sources(files: &[SourceFile], opts: &LintOptions) -> Report {
    let mut report = Report::default();
    if !is_iso_date(&opts.today) {
        report.findings.push(finding(
            rule::BAD_INPUT,
            Severity::Fail,
            "(options)",
            0,
            None,
            format!(
                "today `{}` is not a YYYY-MM-DD date; bridge exemptions are refused",
                opts.today
            ),
        ));
    }
    for file in files {
        let path = normalize(&file.path);
        if !path.ends_with(".rs") {
            continue;
        }
        for t in scan_tests(&file.text) {
            report.tests += 1;
            let name = t.name.as_str();
            match &t.tag {
                Some(Tag::Pure) | Some(Tag::Contract) => report.tagged += 1,
                Some(Tag::Pin(issue)) => {
                    report.tagged += 1;
                    report.pins.push(PinRef {
                        path: path.clone(),
                        line: t.line,
                        test: t.name.clone(),
                        issue: *issue,
                    });
                }
                Some(Tag::Invalid(raw)) => {
                    report.untagged += 1;
                    report.findings.push(finding(
                        rule::INVALID_TAG,
                        Severity::Fail,
                        &path,
                        t.line,
                        Some(name),
                        format!("`rk-test:` tag `{raw}` is not `pure`, `contract` or `pin #<n>`"),
                    ));
                }
                None => {
                    report.untagged += 1;
                    report.findings.push(finding(
                        rule::UNTAGGED_TEST,
                        Severity::Report,
                        &path,
                        t.line,
                        Some(name),
                        format!("test `{name}` has no `// rk-test: pure | contract | pin #<n>` comment directly above it"),
                    ));
                }
            }
            let process = hits(&t.body_code, PROCESS_TOKENS);
            let temp = hits(&t.body_code, TEMP_TOKENS);
            if matches!(t.tag, Some(Tag::Pure)) {
                let misclassified: Vec<&'static str> =
                    process.iter().chain(temp.iter()).copied().collect();
                if !misclassified.is_empty() {
                    report.findings.push(finding(
                        rule::PURE_MISCLASSIFIED,
                        Severity::Fail,
                        &path,
                        t.line,
                        Some(name),
                        format!(
                            "tagged `pure` but uses {}; reclassify as `contract` or remove the use (heuristic, see module docs)",
                            misclassified.join(", ")
                        ),
                    ));
                }
            }
            if !process.is_empty() {
                report
                    .findings
                    .push(launch_finding(&path, t.line, name, &process, opts));
            }
            if let Some(what) = aggregate_reason(&t.body_code) {
                report.findings.push(finding(
                    rule::AGGREGATE_SUITE,
                    Severity::Fail,
                    &path,
                    t.line,
                    Some(name),
                    format!("test {what}; one `#[test]` must not drive a suite (ADR-0001 item 3): give each scenario its own journey"),
                ));
            }
        }
    }
    for s in &opts.scenarios {
        report.findings.extend(check_scenario(s));
    }
    if let Some(frozen) = opts.baseline_untagged {
        report
            .findings
            .extend(check_baseline(report.untagged, frozen));
    }
    report
}

/// Rule 2. A finding only when the untagged count is above the frozen number.
pub fn check_baseline(untagged: usize, frozen: usize) -> Option<Finding> {
    (untagged > frozen).then(|| {
        finding(
            rule::UNTAGGED_ROSE,
            Severity::Fail,
            "(baseline)",
            0,
            None,
            format!(
                "untagged tests rose to {untagged} from the frozen baseline {frozen}; tag the new tests (`// rk-test: pure | contract | pin #<n>`)"
            ),
        )
    })
}

/// Rule 4 for one scenario: unknown declared tokens, and step kinds whose resource is not declared.
/// Each undeclared resource is reported once per scenario.
pub fn check_scenario(s: &ScenarioTokens) -> Vec<Finding> {
    let mut out = Vec::new();
    for d in &s.declared {
        if !KNOWN_TOKENS.contains(&d.as_str()) {
            out.push(finding(
                rule::UNKNOWN_TOKEN,
                Severity::Fail,
                &s.path,
                0,
                Some(s.id.as_str()),
                format!(
                    "declares unknown resource token `{d}`; known tokens: {}",
                    KNOWN_TOKENS.join(", ")
                ),
            ));
        }
    }
    let mut reported: Vec<&'static str> = Vec::new();
    for (idx, kind) in s.steps.iter().enumerate() {
        let Some(token) = token_for_step(kind) else {
            continue;
        };
        let declared = s.declared.iter().any(|d| d.as_str() == token);
        if declared || reported.contains(&token) {
            continue;
        }
        reported.push(token);
        out.push(finding(
            rule::UNDECLARED_TOKEN,
            Severity::Fail,
            &s.path,
            0,
            Some(s.id.as_str()),
            format!(
                "step {} (`{kind}`) uses the `{token}` resource, which the scenario does not declare",
                idx + 1
            ),
        ));
    }
    out
}

/// True for `YYYY-MM-DD` with a month of 01 to 12 and a day of 01 to 31.
pub fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    let shape = b.iter().enumerate().all(|(i, c)| {
        if i == 4 || i == 7 {
            *c == b'-'
        } else {
            c.is_ascii_digit()
        }
    });
    let month = s.get(5..7).and_then(|m| m.parse::<u32>().ok());
    let day = s.get(8..10).and_then(|d| d.parse::<u32>().ok());
    shape && matches!((month, day), (Some(1..=12), Some(1..=31)))
}

// ---------------------------------------------------------------------------------------------
// Tags and test discovery
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tag {
    Pure,
    Contract,
    Pin(u64),
    Invalid(String),
}

struct TestFn {
    name: String,
    /// 1-based line of the `#[test]` attribute.
    line: usize,
    tag: Option<Tag>,
    /// The body with comments and literal contents blanked, from `{` to the matching `}`.
    body_code: String,
}

fn scan_tests(text: &str) -> Vec<TestFn> {
    let code = code_view(text);
    let starts = line_starts(&code);
    let raw: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for li in 0..starts.len() {
        let (start, end) = line_bounds(&starts, code.len(), li);
        if !is_test_attr(code[start..end].trim_start()) {
            continue;
        }
        let Some((name, name_end)) = find_fn(&code, start) else {
            continue;
        };
        let Some((open, close)) = body_range(code.as_bytes(), name_end) else {
            continue;
        };
        out.push(TestFn {
            name,
            line: li + 1,
            tag: tag_above(&raw, li),
            body_code: code[open..close].to_string(),
        });
    }
    out
}

fn is_test_attr(line: &str) -> bool {
    line.starts_with("#[test]")
        || line.starts_with("#[tokio::test]")
        || line.starts_with("#[tokio::test(")
}

/// The tag comment directly above line `li`, skipping other attributes and `///` doc lines.
/// `None` when the nearest other line is not a tag comment (or there is no such line).
fn tag_above(raw: &[&str], li: usize) -> Option<Tag> {
    let mut k = li;
    while k > 0 {
        k -= 1;
        let t = raw.get(k).map_or("", |l| l.trim());
        if t.starts_with("#[") || t.starts_with("///") {
            continue;
        }
        return parse_tag_line(t);
    }
    None
}

fn parse_tag_line(t: &str) -> Option<Tag> {
    let rest = t.strip_prefix("//")?;
    if rest.starts_with('/') {
        return None;
    }
    let rest = rest.trim_start().strip_prefix("rk-test:")?;
    Some(parse_tag(rest))
}

fn parse_tag(rest: &str) -> Tag {
    let rest = rest.trim();
    match rest {
        "pure" => Tag::Pure,
        "contract" => Tag::Contract,
        _ => {
            let issue = rest
                .strip_prefix("pin")
                .map(|r| r.trim_start())
                .and_then(|r| r.strip_prefix('#'))
                .filter(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()))
                .and_then(|d| d.parse::<u64>().ok())
                .filter(|n| *n > 0);
            match issue {
                Some(n) => Tag::Pin(n),
                None => Tag::Invalid(rest.to_string()),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rules 1, 3 and 5 on one test body
// ---------------------------------------------------------------------------------------------

/// The aggregate-loop reason for a body, if any (rule 3).
fn aggregate_reason(body_code: &str) -> Option<&'static str> {
    if body_code.contains("run_suite(") {
        Some("calls `run_suite(`")
    } else if iterates_scenario_files(body_code) {
        Some("loops over scenario files")
    } else {
        None
    }
}

/// True when a `for` loop in the body iterates scenario files (rule 3).
fn iterates_scenario_files(code: &str) -> bool {
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(rel) = code[from..].find("for") {
        let at = from + rel;
        from = at + 3;
        let word_start = at == 0 || !is_ident(b[at - 1]);
        let word_end = b.get(at + 3).is_some_and(|c| c.is_ascii_whitespace());
        if !(word_start && word_end) {
            continue;
        }
        // The loop header runs to the first `{` or `;`. `impl X for Y {` has no ` in ` and is skipped.
        let header_end = b[at..]
            .iter()
            .position(|&c| c == b'{' || c == b';')
            .map_or(b.len(), |p| at + p);
        let header = &code[at + 3..header_end];
        let Some(in_at) = header.find(" in ") else {
            continue;
        };
        let expr = header[in_at + 4..].to_ascii_lowercase();
        if expr.contains("step") {
            continue;
        }
        if expr.contains("scenario") || ENUM_TOKENS.iter().any(|t| expr.contains(*t)) {
            return true;
        }
    }
    false
}

fn hits(body: &str, tokens: &'static [&'static str]) -> Vec<&'static str> {
    tokens
        .iter()
        .copied()
        .filter(|t| body.contains(*t))
        .collect()
}

/// Rule 5 for one launching test. Declared, unexpired bridge entries exempt it; the exemption is
/// reported as informational.
fn launch_finding(
    path: &str,
    line: usize,
    name: &str,
    spawns: &[&'static str],
    opts: &LintOptions,
) -> Finding {
    let what = spawns.join(", ");
    let entry = opts
        .bridge
        .iter()
        .find(|e| normalize(&e.path) == path && e.test.as_deref().is_none_or(|t| t == name));
    match entry {
        None => finding(
            rule::LAUNCHES_BINARY,
            Severity::Fail,
            path,
            line,
            Some(name),
            format!(
                "test launches a process ({what}); tests must not launch binaries (ADR-0001 item 9). Only an unexpired bridge entry exempts it"
            ),
        ),
        Some(e) if !is_iso_date(&e.removal) => finding(
            rule::LAUNCHES_BINARY,
            Severity::Fail,
            path,
            line,
            Some(name),
            format!("bridge entry for `{path}` has a malformed removal date `{}`", e.removal),
        ),
        Some(e) if !is_iso_date(&opts.today) || e.removal.as_str() < opts.today.as_str() => finding(
            rule::LAUNCHES_BINARY,
            Severity::Fail,
            path,
            line,
            Some(name),
            format!("bridge entry for `{path}` expired on {} (today `{}`)", e.removal, opts.today),
        ),
        Some(e) => finding(
            rule::BRIDGE_EXEMPT,
            Severity::Report,
            path,
            line,
            Some(name),
            format!("launches a process ({what}); exempt by bridge entry until {}", e.removal),
        ),
    }
}

/// Maps a step kind to the resource token it needs, by its first segment.
fn token_for_step(kind: &str) -> Option<&'static str> {
    let first = kind
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match first.as_str() {
        "clipboard" => Some("clipboard"),
        "focus" => Some("focus"),
        "audio" => Some("audio"),
        "gui" => Some("gui-session"),
        "credentials" | "credential" => Some("credentials"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Source reduction and scanning helpers
// ---------------------------------------------------------------------------------------------

fn finding(
    id: &'static str,
    severity: Severity,
    path: &str,
    line: usize,
    subject: Option<&str>,
    message: String,
) -> Finding {
    Finding {
        rule: id,
        severity,
        path: path.to_string(),
        line,
        subject: subject.map(str::to_string),
        message,
    }
}

fn normalize(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        text.bytes()
            .enumerate()
            .filter(|(_, c)| *c == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// Byte range of line `li` in the text, without its trailing newline.
fn line_bounds(starts: &[usize], len: usize, li: usize) -> (usize, usize) {
    let start = starts[li];
    let end = match starts.get(li + 1) {
        Some(next) => *next - 1,
        None => len,
    };
    (start, end)
}

/// The first `fn <name>` at or after `from`, with the byte offset just past the name.
fn find_fn(code: &str, from: usize) -> Option<(String, usize)> {
    let b = code.as_bytes();
    let mut i = from;
    while i + 2 < b.len() {
        if b[i] == b'f'
            && b[i + 1] == b'n'
            && (i == 0 || !is_ident(b[i - 1]))
            && b[i + 2].is_ascii_whitespace()
        {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            while j < b.len() && is_ident(b[j]) {
                j += 1;
            }
            if j > start {
                return Some((code[start..j].to_string(), j));
            }
        }
        i += 1;
    }
    None
}

/// The body braces `{ ... }` after `from`, as a half-open range. `None` when a `;` comes first.
fn body_range(b: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < b.len() && b[i] != b'{' && b[i] != b';' {
        i += 1;
    }
    if i >= b.len() || b[i] == b';' {
        return None;
    }
    let open = i;
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn blank(out: &mut [u8], k: usize) {
    if out[k] != b'\n' {
        out[k] = b' ';
    }
}

/// Same length as `src`. Comments and the contents of string and char literals become spaces
/// (newlines are kept), so token searches cannot match inside them. Delimiters stay.
fn code_view(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < n {
        let c = b[i];
        let next = if i + 1 < n { b[i + 1] } else { 0 };
        if c == b'/' && next == b'/' {
            while i < n && b[i] != b'\n' {
                blank(&mut out, i);
                i += 1;
            }
            continue;
        }
        if c == b'/' && next == b'*' {
            i = blank_block_comment(&mut out, b, i);
            continue;
        }
        if c == b'r' && raw_prefix_ok(b, i) {
            if let Some((quote, hashes)) = raw_quote(b, i) {
                i = blank_raw(&mut out, b, quote, hashes);
                continue;
            }
        }
        if c == b'"' {
            i = blank_str(&mut out, b, i);
            continue;
        }
        if c == b'\'' {
            if let Some(end) = char_literal_end(b, i) {
                for k in i + 1..end {
                    blank(&mut out, k);
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Blanks a nested block comment that starts at `start` (which holds `/*`). Returns the index after it.
fn blank_block_comment(out: &mut [u8], b: &[u8], start: usize) -> usize {
    let n = b.len();
    let mut depth = 0usize;
    let mut i = start;
    while i < n {
        if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            depth += 1;
            blank(out, i);
            blank(out, i + 1);
            i += 2;
        } else if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' {
            depth -= 1;
            blank(out, i);
            blank(out, i + 1);
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            blank(out, i);
            i += 1;
        }
    }
    n
}

/// Blanks an ordinary string whose opening quote is at `open`. Returns the index after it.
fn blank_str(out: &mut [u8], b: &[u8], open: usize) -> usize {
    let n = b.len();
    let mut k = open + 1;
    while k < n && b[k] != b'"' {
        if b[k] == b'\\' && k + 1 < n {
            blank(out, k);
            k += 1;
        }
        blank(out, k);
        k += 1;
    }
    k + 1
}

/// `r` may start a raw string when it does not continue an identifier (`br` counts).
fn raw_prefix_ok(b: &[u8], i: usize) -> bool {
    if i == 0 || !is_ident(b[i - 1]) {
        return true;
    }
    i >= 2 && b[i - 1] == b'b' && !is_ident(b[i - 2])
}

/// For `r` at `i`, the opening quote index and the number of `#` marks, when a raw string opens here.
fn raw_quote(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut j = i + 1;
    let mut hashes = 0usize;
    while j < b.len() && b[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    (j < b.len() && b[j] == b'"').then_some((j, hashes))
}

/// Blanks a raw string whose opening quote is at `quote`. Returns the index after it.
fn blank_raw(out: &mut [u8], b: &[u8], quote: usize, hashes: usize) -> usize {
    let n = b.len();
    let body = quote + 1;
    let mut k = body;
    while k < n {
        if b[k] == b'"' && (1..=hashes).all(|h| k + h < n && b[k + h] == b'#') {
            for m in body..k {
                blank(out, m);
            }
            return k + 1 + hashes;
        }
        k += 1;
    }
    for m in body..n {
        blank(out, m);
    }
    n
}

/// For a `'` at `i`, the index of the closing quote when a char literal starts here. `None` for a
/// lifetime such as `'a`.
fn char_literal_end(b: &[u8], i: usize) -> Option<usize> {
    let n = b.len();
    if i + 1 >= n {
        return None;
    }
    if b[i + 1] == b'\\' {
        let mut k = i + 3;
        while k < n && k < i + 12 {
            if b[k] == b'\'' {
                return Some(k);
            }
            k += 1;
        }
        return None;
    }
    if b[i + 1] == b'\'' {
        return None;
    }
    let end = i + 1 + utf8_len(b[i + 1]);
    (end < n && b[end] == b'\'').then_some(end)
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
