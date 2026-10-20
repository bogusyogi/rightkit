// Structured test results for the publish gate. The gate consumes what the test runners REPORT, not
// whether a command exited 0:
//   Rust libtest   `test result: ok. N passed; F failed; I ignored; ...` once per test binary
//   node:test      `# tests|pass|fail|cancelled|skipped|todo N` (tap) or `ℹ ...` (spec) summary lines
//   vitest         `Tests  F failed | N passed | S skipped (T)`
//   skip markers   a line holding the word SKIP, with or without a prefix (`SKIP x`, `[browser] SKIP x`)
// Output is buffered by LINE, so a marker or summary split across two stream chunks is still one line.
// A unit passes only when EVERY test command executed a positive count (the total is a summary, never a pool), no ignored or skipped test the request has not
// allowed, and no red step (the caller checks exit codes).
import { StringDecoder } from "node:string_decoder";

const ANSI = /\u001b\[[0-9;?]*[ -/]*[@-~]/g;
export const stripAnsi = (text) => String(text).replace(ANSI, "");

const LIBTEST = /\btest result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored/;
const NODE_SUMMARY = /^\s*(?:#|ℹ)\s+(tests|pass|fail|cancelled|skipped|todo)\s+(\d+)\s*$/;
const VITEST_TESTS = /^\s*Tests\s+(.+?)\s+\(\d+\)\s*$/;
const SKIP_MARKER = /(?:^|[\s\]:])SKIP(?=[\s:]|$)/;

/** What a test command is: used to decide which steps count toward a unit's executed total. */
export const TEST_COMMAND = /(?:\bcargo test\b|\brun test\b|\bnode --test\b)/;

export const emptyCounts = () => ({ executed: 0, failed: 0, ignored: 0, skipped: 0, binaries: 0 });

export class ResultScanner {
  constructor() {
    this.decoder = new StringDecoder("utf8");
    this.tail = "";
    this.counts = emptyCounts();
    this.node = { pass: 0, fail: 0, cancelled: 0, skipped: 0, todo: 0, seen: false };
  }

  push(chunk) {
    const lines = (this.tail + (typeof chunk === "string" ? chunk : this.decoder.write(chunk))).split(/\r\n|\n|\r/);
    this.tail = lines.pop() ?? "";
    for (const line of lines) this.line(line);
  }

  /** Flush the last unterminated line and return the counts. */
  finish() {
    this.tail += this.decoder.end();
    if (this.tail) this.line(this.tail);
    this.tail = "";
    const { counts, node } = this;
    return {
      executed: counts.executed + node.pass,
      failed: counts.failed + node.fail + node.cancelled,
      ignored: counts.ignored,
      skipped: counts.skipped + node.skipped + node.todo,
      binaries: counts.binaries,
    };
  }

  line(raw) {
    const line = stripAnsi(raw);
    let m = LIBTEST.exec(line);
    if (m) {
      this.counts.binaries += 1;
      this.counts.executed += Number(m[2]);
      this.counts.failed += Number(m[3]);
      this.counts.ignored += Number(m[4]);
      return;
    }
    m = NODE_SUMMARY.exec(line);
    if (m) {
      const key = m[1] === "tests" ? null : m[1];
      if (key) { this.node[key] += Number(m[2]); this.node.seen = true; }
      return;
    }
    m = VITEST_TESTS.exec(line);
    if (m) {
      for (const [, n, kind] of m[1].matchAll(/(\d+)\s+(passed|failed|skipped|todo)/g)) {
        if (kind === "passed") this.node.pass += Number(n);
        else if (kind === "failed") this.node.fail += Number(n);
        else this.node.skipped += Number(n);
      }
      return;
    }
    if (SKIP_MARKER.test(line)) this.counts.skipped += 1;
  }
}

/** Sum the counts of several scanners (stdout and stderr). */
export const mergeCounts = (...all) => all.reduce((a, b) => ({ executed: a.executed + b.executed, failed: a.failed + b.failed, ignored: a.ignored + b.ignored, skipped: a.skipped + b.skipped, binaries: a.binaries + b.binaries }), emptyCounts());

/** Convenience for tests and tools: scan whole chunks in order. */
export function scanChunks(chunks) {
  const scanner = new ResultScanner();
  for (const chunk of chunks) scanner.push(chunk);
  return scanner.finish();
}

export const UNIT_STATUSES = ["pass", "allowed-skips"];

/**
 * Judge one requested unit from its steps. `steps` are gate result rows ({ command, isTest, executed, failed,
 * ignored, skipped }). Returns { status: "pass" | "allowed-skips" | "fail", executed, ignored, skipped,
 * rows, problems }.
 *
 * Every test command is a required row and must itself have executed at least one test: a feature-set or
 * serial rerun that ran nothing fails, naming that exact row, whatever another row reported, and a row whose
 * tests were all skipped or ignored is never credited. `requiredCommands`, when given, is how many test
 * commands the gate was meant to run for the unit; fewer rows than that fails (a mandatory command that
 * never ran is not qualified). The totals (executed, ignored, skipped) are a summary only. An ignored or
 * skipped test fails unless the request names the unit (allowIgnored for `#[ignore]`/runner skips counted as
 * ignored, allowMissingAssets for SKIP markers and runner-skipped tests); naming it never hides the counts.
 */
export function judgeUnit({ name, steps, allowIgnored = [], allowMissingAssets = [], requiredCommands = undefined }) {
  const tests = steps.filter((s) => s.isTest);
  const sum = (key) => tests.reduce((total, s) => total + (s[key] ?? 0), 0);
  const executed = sum("executed"); const ignored = sum("ignored"); const skipped = sum("skipped");
  const problems = [];
  const rows = tests.map((s, i) => ({ command: s.command ?? `test command ${i + 1}`, executed: s.executed ?? 0, ignored: s.ignored ?? 0, skipped: s.skipped ?? 0, qualified: (s.executed ?? 0) > 0 }));
  if (tests.length === 0) problems.push("no test step ran");
  else for (const row of rows) {
    if (!row.qualified) problems.push(`required test command \`${row.command}\` executed no test (${row.skipped} skipped, ${row.ignored} ignored; no runner summary reported a positive passed count)`);
  }
  if (requiredCommands !== undefined && tests.length < requiredCommands) problems.push(`${tests.length} of ${requiredCommands} required test commands ran; a mandatory command that did not run is not a pass`);
  let allowed = false;
  if (ignored > 0) {
    if (allowIgnored.includes(name)) allowed = true;
    else problems.push(`${ignored} ignored test(s); an ignored test is not a pass (name the unit in allow_ignored to accept it)`);
  }
  if (skipped > 0) {
    if (allowMissingAssets.includes(name)) allowed = true;
    else problems.push(`${skipped} skipped test(s) or SKIP marker(s); a skipped test is not a pass (name the unit in allow_missing_assets to accept missing assets)`);
  }
  return { status: problems.length ? "fail" : allowed ? "allowed-skips" : "pass", executed, ignored, skipped, rows, problems };
}
