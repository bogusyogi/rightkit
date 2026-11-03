//! Eval-hack advisory detector for the victory gate.
//!
//! Inspects the run's diff for patterns that suggest scoring exploitation
//! rather than genuine goal completion (arXiv:2605.15221: stronger models
//! produce MORE eval-hacks). Deterministic, config-gated, advisory-only —
//! findings are attached to the gate result for the jury/signoff to weigh.

use crate::conductor::WorkerResult;

/// Advisory finding from the eval-hack detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalHackFinding {
    /// `test_rewrite` | `hardcoded_output` | `config_mutation`
    pub category: String,
    /// Human-readable description of the concern.
    pub message: String,
    /// File path involved, if known.
    pub file: String,
}

/// Run all eval-hack checks over a unified diff and worker results.
/// Returns an empty vec when the diff is clean.
pub fn detect(diff: &str, workers: &[WorkerResult]) -> Vec<EvalHackFinding> {
    let mut findings = Vec::new();
    findings.extend(check_test_rewrite(diff, workers));
    findings.extend(check_hardcoded_outputs(diff));
    findings.extend(check_config_mutation(diff));
    findings
}

// ---------------------------------------------------------------------------
// Check 1: test files modified in the same run whose tests now pass.
// ---------------------------------------------------------------------------

fn check_test_rewrite(diff: &str, workers: &[WorkerResult]) -> Vec<EvalHackFinding> {
    let test_files: Vec<String> = modified_files(diff)
        .into_iter()
        .filter(|f| is_test_file(f))
        .collect();
    if test_files.is_empty() {
        return vec![];
    }
    // If any worker reports success (interpreted as the run passing), flag it.
    let any_passed = workers.iter().any(|w| w.success);
    if !any_passed {
        return vec![];
    }
    test_files
        .into_iter()
        .map(|f| EvalHackFinding {
            category: "test_rewrite".into(),
            message: format!(
                "test file '{f}' was modified in this run and verification passed — \
                 possible test rewrite-to-pass"
            ),
            file: f,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Check 2: hardcoded expected outputs in new test assertions.
//
// Heuristic: look for new `+` lines that contain an assert-like pattern with
// a literal value, where that same literal also appears in the implementation
// diff (non-test file additions).
// ---------------------------------------------------------------------------

fn check_hardcoded_outputs(diff: &str) -> Vec<EvalHackFinding> {
    let files = parse_file_sections(diff);
    let mut findings = Vec::new();

    // Collect literal strings/numbers from new implementation lines (non-test).
    let impl_literals: Vec<String> = files
        .iter()
        .filter(|(f, _)| !is_test_file(f))
        .flat_map(|(_, lines)| {
            lines
                .iter()
                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                .flat_map(|l| extract_literals(l))
                .collect::<Vec<_>>()
        })
        .collect();
    if impl_literals.is_empty() {
        return findings;
    }

    // Check new lines in test files for assert-like patterns with matching literals.
    for (file, lines) in &files {
        if !is_test_file(file) {
            continue;
        }
        for line in lines {
            if !line.starts_with('+') || line.starts_with("+++") {
                continue;
            }
            if !looks_like_assertion(line) {
                continue;
            }
            let test_literals = extract_literals(line);
            for lit in &test_literals {
                if impl_literals.contains(lit) && lit.len() > 1 {
                    findings.push(EvalHackFinding {
                        category: "hardcoded_output".into(),
                        message: format!(
                            "new assertion in '{file}' contains literal {lit} which also \
                             appears in the implementation diff — possible hardcoded expected output"
                        ),
                        file: file.clone(),
                    });
                    break; // one finding per line
                }
            }
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// Check 3: verification-script / check-config mutation.
// ---------------------------------------------------------------------------

fn check_config_mutation(diff: &str) -> Vec<EvalHackFinding> {
    modified_files(diff)
        .into_iter()
        .filter(|f| is_verification_config(f))
        .map(|f| EvalHackFinding {
            category: "config_mutation".into(),
            message: format!(
                "verification/check config '{f}' was modified in this run — \
                 possible scoring exploitation"
            ),
            file: f,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Diff parsing helpers.
// ---------------------------------------------------------------------------

/// Extract the list of modified file paths from a unified diff.
fn modified_files(diff: &str) -> Vec<String> {
    diff.lines()
        .filter_map(|l| l.strip_prefix("+++ b/").map(|s| s.to_string()))
        .collect()
}

/// Parse a diff into per-file sections: (file_path, lines).
fn parse_file_sections(diff: &str) -> Vec<(String, Vec<String>)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            sections.push((path.to_string(), Vec::new()));
        } else if let Some((_, ref mut lines)) = sections.last_mut() {
            lines.push(line.to_string());
        }
    }
    sections
}

// ---------------------------------------------------------------------------
// Test-file heuristic (mirrors rightkit-agent-policy `is_test_file`).
// ---------------------------------------------------------------------------

fn is_test_file(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    if p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains("/spec/")
        || p.starts_with("tests/")
        || p.starts_with("test/")
        || p.starts_with("spec/")
    {
        return true;
    }
    let name = p.rsplit('/').next().unwrap_or(&p);
    name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.ends_with("_test.ts")
        || name.ends_with("_test.js")
        || name.ends_with("_spec.rb")
        || name.ends_with("_spec.ts")
        || name.ends_with("_spec.js")
        || name.starts_with("test_")
}

// ---------------------------------------------------------------------------
// Verification-config heuristic.
// ---------------------------------------------------------------------------

fn is_verification_config(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    p.contains(".github/workflows/")
        || p.contains(".gitlab-ci")
        || p.contains("jest.config")
        || p.contains("vitest.config")
        || p.contains("pytest.ini")
        || p.contains("pyproject.toml")
        || p.contains("tox.ini")
        || p.contains(".mocharc")
        || p.contains("karma.conf")
        || p.contains("codecov")
        || p.contains(".coveragerc")
        || p.contains("coverage.config")
        || p.ends_with("Makefile")
        || p.ends_with("makefile")
        || p.contains("verify.sh")
        || p.contains("check.sh")
        || p.contains("ci.sh")
        || p.contains("deno.json")
        || p.contains("biome.json")
        || p.contains("eslint.config")
        || p.contains(".eslintrc")
        || p.contains("rustfmt.toml")
        || p.contains("clippy.toml")
        || p.contains(".cargo/config")
}

// ---------------------------------------------------------------------------
// Assertion / literal heuristics.
// ---------------------------------------------------------------------------

/// Quick check whether a line looks like a test assertion.
fn looks_like_assertion(line: &str) -> bool {
    let l = line.trim_start_matches('+').trim();
    l.contains("assert_eq!")
        || l.contains("assert_ne!")
        || l.contains("assert(")
        || l.contains("expect(")
        || l.contains(".toBe(")
        || l.contains(".toEqual(")
        || l.contains(".toStrictEqual(")
        || l.contains("assert_eq(")
        || l.contains("assertEqual")
        || l.contains("assertEquals")
}

/// Extract quoted strings and bare numeric literals from a line.
fn extract_literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            // Quoted string
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    s.push(chars[i + 1]);
                    i += 2;
                } else {
                    s.push(chars[i]);
                    i += 1;
                }
            }
            i += 1; // closing quote
            if !s.is_empty() {
                out.push(s);
            }
        } else if chars[i].is_ascii_digit()
            || (chars[i] == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let num: String = chars[start..i].iter().collect();
            out.push(num);
        } else {
            i += 1;
        }
    }
    out
}
