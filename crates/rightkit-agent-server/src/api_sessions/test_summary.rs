//! "Tested: 44 passed" on a turn's result.
//!
//! The engine keeps no structured test result. What it has is the command the agent ran and
//! the text that came back. This reads the closing summary line that the common test runners
//! print and reports what it says. It reports nothing when no such line is found: a result card
//! must never claim a test run it cannot point to.

use serde::Serialize;

/// What a test run's own summary line said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestSummary {
    /// The runner, as recognised from the command: `pytest`, `cargo test`, `jest`, `vitest`,
    /// `go test`, `unittest`, `npm test` and so on.
    pub runner: String,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    /// `passed` when nothing failed, otherwise `failed`.
    pub outcome: &'static str,
    /// One plain phrase: "44 passed" or "3 of 44 failed".
    pub words: String,
}

/// The runner a command line starts, if it is a test command.
pub(crate) fn test_runner(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    if has("pytest") || has("py.test") {
        Some("pytest")
    } else if has("unittest") {
        Some("unittest")
    } else if has("cargo test") || has("cargo nextest") {
        Some("cargo test")
    } else if has("vitest") {
        Some("vitest")
    } else if has("jest") {
        Some("jest")
    } else if has("go test") {
        Some("go test")
    } else if has("npm test")
        || has("npm run test")
        || has("pnpm test")
        || has("pnpm run test")
        || has("yarn test")
        || has("bun test")
    {
        Some("npm test")
    } else if has("rspec") {
        Some("rspec")
    } else if has("dotnet test") {
        Some("dotnet test")
    } else if has("mvn test") || has("gradle test") || has("gradlew test") {
        Some("junit")
    } else {
        None
    }
}

/// Read a test run's summary from the text it printed. `None` when the command is not a test
/// command or no summary line is found.
pub(crate) fn parse_test_summary(command: &str, output: &str) -> Option<TestSummary> {
    let runner = test_runner(command)?;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut found = false;

    for line in output.lines().rev().take(400) {
        let lower = strip_escapes(line).to_ascii_lowercase();
        // cargo: "test result: ok. 12 passed; 0 failed; 1 ignored; ..." (one per test binary:
        // add them up).
        if lower.contains("test result:") {
            passed += number_before(&lower, " passed").unwrap_or(0);
            failed += number_before(&lower, " failed").unwrap_or(0);
            skipped += number_before(&lower, " ignored").unwrap_or(0);
            found = true;
            continue;
        }
        if found && runner == "cargo test" {
            continue;
        }
        // unittest: "Ran 20 tests in 0.01s" then "OK" or "FAILED (failures=2, errors=1)".
        if runner == "unittest" || lower.trim_start().starts_with("ran ") {
            if let Some(total) = number_after(&lower, "ran ").filter(|_| lower.contains(" test")) {
                let bad = number_after_key(output, "failures=").unwrap_or(0)
                    + number_after_key(output, "errors=").unwrap_or(0);
                failed = bad.min(total);
                skipped = number_after_key(output, "skipped=").unwrap_or(0);
                passed = total.saturating_sub(failed).saturating_sub(skipped);
                found = true;
                break;
            }
        }
        // pytest, jest, vitest, rspec, dotnet: a line that counts passed and/or failed.
        let counts_passed = number_before(&lower, " passed");
        let counts_failed = number_before(&lower, " failed")
            .or_else(|| number_before(&lower, " failures"))
            .or_else(|| number_before(&lower, " failure"));
        if counts_passed.is_some() || counts_failed.is_some() {
            passed = counts_passed.unwrap_or(0);
            failed = counts_failed.unwrap_or(0)
                + number_before(&lower, " errors").unwrap_or(0)
                + number_before(&lower, " error")
                    .filter(|_| !lower.contains(" errors"))
                    .unwrap_or(0);
            skipped = number_before(&lower, " skipped")
                .or_else(|| number_before(&lower, " pending"))
                .unwrap_or(0);
            found = true;
            break;
        }
    }

    // go test prints one "ok" or "FAIL" line per package and no counts.
    if !found && runner == "go test" {
        for line in output.lines() {
            let line = line.trim_start();
            if line.starts_with("ok ") || line.starts_with("ok\t") {
                passed += 1;
                found = true;
            } else if line.starts_with("FAIL\t")
                || line.starts_with("FAIL ")
                || line.starts_with("--- FAIL")
            {
                failed += 1;
                found = true;
            }
        }
    }

    if !found {
        return None;
    }
    let outcome = if failed == 0 { "passed" } else { "failed" };
    let words = if failed == 0 {
        format!("{passed} passed")
    } else {
        format!("{failed} of {} failed", passed + failed)
    };
    Some(TestSummary {
        runner: runner.to_string(),
        passed,
        failed,
        skipped,
        outcome,
        words,
    })
}

/// The whole number that ends just before `suffix` ("12 passed" -> 12). Occurrences of the
/// suffix with no number in front (cargo's "test result: FAILED.") are passed over.
fn number_before(text: &str, suffix: &str) -> Option<u32> {
    let mut from = 0;
    while let Some(found) = text[from..].find(suffix) {
        let end = from + found;
        let digits: String = text[..end]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(number) = digits.parse() {
            return Some(number);
        }
        from = end + suffix.len();
    }
    None
}

/// The whole number that starts just after `prefix` ("ran 20 tests" -> 20).
fn number_after(text: &str, prefix: &str) -> Option<u32> {
    let start = text.find(prefix)? + prefix.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn number_after_key(text: &str, key: &str) -> Option<u32> {
    number_after(&text.to_ascii_lowercase(), key)
}

/// Drop terminal colour sequences, which test runners put around the counts.
fn strip_escapes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(command: &str, output: &str) -> (u32, u32, u32, String) {
        let s = parse_test_summary(command, output).expect("a summary");
        (s.passed, s.failed, s.skipped, s.words)
    }

    #[test]
    fn pytest_lines() {
        assert_eq!(
            summary("python -m pytest -q", "....\n44 passed in 0.31s\n"),
            (44, 0, 0, "44 passed".into())
        );
        assert_eq!(
            summary("pytest", "=== 3 failed, 38 passed, 2 skipped in 1.20s ==="),
            (38, 3, 2, "3 of 41 failed".into())
        );
        assert_eq!(
            summary(
                "pytest",
                "\u{1b}[32m\u{1b}[1m12 passed\u{1b}[0m\u{1b}[32m in 0.05s\u{1b}[0m"
            ),
            (12, 0, 0, "12 passed".into())
        );
    }

    #[test]
    fn cargo_adds_up_every_test_binary() {
        let out = "test result: ok. 10 passed; 0 failed; 1 ignored; 0 measured\n\
                   running 3 tests\n\
                   test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured\n";
        assert_eq!(
            summary("cargo test -p x", out),
            (12, 1, 1, "1 of 13 failed".into())
        );
    }

    #[test]
    fn unittest_counts_failures_and_errors() {
        assert_eq!(
            summary(
                "python -m unittest discover -s tests",
                "Ran 20 tests in 0.010s\n\nOK\n"
            ),
            (20, 0, 0, "20 passed".into())
        );
        assert_eq!(
            summary(
                "python -m unittest",
                "Ran 20 tests in 0.010s\n\nFAILED (failures=2, errors=1)\n"
            ),
            (17, 3, 0, "3 of 20 failed".into())
        );
    }

    #[test]
    fn jest_and_vitest_lines() {
        assert_eq!(
            summary("npx jest", "Tests:       1 failed, 41 passed, 42 total\n"),
            (41, 1, 0, "1 of 42 failed".into())
        );
        assert_eq!(
            summary("pnpm exec vitest run", " Tests  2 passed (2)\n"),
            (2, 0, 0, "2 passed".into())
        );
    }

    #[test]
    fn go_counts_packages() {
        assert_eq!(
            summary(
                "go test ./...",
                "ok  \texample.com/a\t0.01s\nFAIL\texample.com/b\t0.02s\n"
            ),
            (1, 1, 0, "1 of 2 failed".into())
        );
    }

    #[test]
    fn nothing_is_claimed_without_a_test_command_or_a_summary() {
        assert!(parse_test_summary("ls -la", "44 passed").is_none());
        assert!(parse_test_summary("pytest", "collected 0 items\n").is_none());
        assert!(test_runner("python build.py").is_none());
    }
}
