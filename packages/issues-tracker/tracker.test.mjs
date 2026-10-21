import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { main } from "./cli/right-issues.mjs";
import { buildModel, embedJson, fromRestIssues, groupOf, noteOf, normaliseGroups, renderHtml, safeUrl, statusOf } from "./tracker.mjs";

const NOW = new Date("2026-10-21T12:00:00Z");
const issue = (n, extra = {}) => ({
  number: n, title: `Issue ${n}`, state: "OPEN", labels: [], updatedAt: "2026-10-20T12:00:00Z",
  closedAt: null, url: `https://github.com/o/r/issues/${n}`, body: "", ...extra,
});

test("status comes from labels in priority order, closed is done", () => {
  assert.equal(statusOf(issue(1)), "open");
  assert.equal(statusOf(issue(1, { labels: [{ name: "tracking" }, { name: "needs-decision" }] })), "decision");
  assert.equal(statusOf(issue(1, { labels: [{ name: "needs-live-run" }, { name: "blocked" }] })), "live");
  assert.equal(statusOf(issue(1, { labels: ["built-needs-check"] })), "fixed");
  assert.equal(statusOf(issue(1, { labels: [{ name: "blocked-upstream" }] })), "blocked");
  assert.equal(statusOf(issue(1, { state: "CLOSED", labels: [{ name: "needs-decision" }] })), "done");
});

test("group: explicit mapping, then group label, then title rule, else Other", () => {
  const g = normaliseGroups({ groups: ["Hub"], issues: { 5: "Hub" }, rules: [["Voice", ["\\bvoice\\b"]]] });
  assert.equal(groupOf(issue(5, { labels: [{ name: "area:Notch" }] }), g), "Hub");
  assert.equal(groupOf(issue(6, { labels: [{ name: "Component:Notch" }] }), g), "Notch");
  assert.equal(groupOf(issue(7, { title: "Voice input drops" }), g), "Voice");
  assert.equal(groupOf(issue(8, { labels: [{ name: "area:" }] }), g), "Other");
  assert.equal(groupOf(issue(9), normaliseGroups(undefined)), "Other");
});

test("model keeps every closed issue and flags the recent ones", () => {
  const model = buildModel({
    repo: "o/r", now: NOW,
    issues: [
      issue(1, { labels: [{ name: "needs-decision" }], body: "Which option?\n\nMore detail." }),
      issue(2, { labels: [{ name: "needs-live-run" }] }),
      issue(3, { labels: [{ name: "built-needs-check" }] }),
      issue(4, { labels: [{ name: "blocked" }] }),
      issue(5, { state: "CLOSED", closedAt: "2026-10-19T00:00:00Z" }),
      issue(6, { state: "CLOSED", closedAt: "2026-09-01T00:00:00Z" }),
    ],
  });
  assert.deepEqual(model.issues.map((i) => i.n), [1, 2, 3, 4, 5, 6]);
  assert.deepEqual(model.issues.map((i) => i.recent), [true, true, true, true, true, false]);
  assert.deepEqual(model.stats, { open: 4, needs: 2, decisions: 1, live: 1, built: 1, blocked: 1, closedWeek: 1, closedTotal: 2 });
  assert.equal(model.issues[0].note, "Which option?");
  assert.equal(model.issues[2].note, "");
  assert.deepEqual(model.groups, ["Other"]);
});

test("declared groups keep their order; undeclared ones follow sorted with Other last", () => {
  const model = buildModel({
    repo: "o/r", now: NOW, groupsFile: { groups: ["Zeta"] },
    issues: [issue(1, { labels: [{ name: "area:Beta" }] }), issue(2), issue(3, { labels: [{ name: "area:Alpha" }] })],
  });
  assert.deepEqual(model.groups, ["Zeta", "Alpha", "Beta", "Other"]);
});

test("note skips headings and comments and is bounded", () => {
  assert.equal(noteOf("<!-- template -->\n\n## Heading\n\nThe   real\nquestion."), "The real question.");
  assert.equal(noteOf("x".repeat(400)).length, 280);
  assert.equal(noteOf(null), "");
});

test("embedded data cannot close the script element", () => {
  const out = embedJson({ t: "</script><script>alert(1)</script> " });
  assert.ok(!out.includes("</script>"));
  assert.ok(!out.includes(" "));
  assert.deepEqual(JSON.parse(out), { t: "</script><script>alert(1)</script> " });
});

test("page is self-contained and escapes the repository name", () => {
  const model = buildModel({ repo: "o/r<b>", now: NOW, issues: [issue(1, { title: "</script>" })] });
  const html = renderHtml(model, { who: "Adrian" });
  assert.match(html, /<title>r&lt;b&gt; issues<\/title>/);
  assert.equal((html.match(/<\/script>/g) || []).length, 1);
  assert.ok(!/(src|href)=["']https?:/i.test(html.replace(/https:\/\/github\.com\/o\/r\/issues\/1/g, "")));
  assert.match(html, /prefers-color-scheme:dark/);
});

test("cli writes issues.html from an issues file and a groups file", () => {
  const dir = mkdtempSync(join(tmpdir(), "right-issues-"));
  mkdirSync(join(dir, ".rightkit"));
  writeFileSync(join(dir, ".rightkit", "issue-groups.json"), JSON.stringify({ groups: ["Hub"], issues: { 1: "Hub" } }));
  writeFileSync(join(dir, "issues.json"), JSON.stringify([issue(1, { labels: [{ name: "needs-decision" }] })]));
  const cli = join(dirname(fileURLToPath(import.meta.url)), "cli", "right-issues.mjs");
  const stdout = execFileSync(process.execPath, [cli, "--root", dir, "--repo", "o/r", "--issues", "issues.json"], { encoding: "utf8" });
  assert.match(stdout, /"decisions":1/);
  const html = readFileSync(join(dir, "issues.html"), "utf8");
  assert.match(html, /"group":"Hub"/);
  assert.throws(() => execFileSync(process.execPath, [cli, "--root", dir, "--repo", "not a repo", "--issues", "issues.json"], { stdio: "pipe" }));
});

const cliPath = join(dirname(fileURLToPath(import.meta.url)), "cli", "right-issues.mjs");
const runCli = (args) => {
  try {
    return { code: 0, out: execFileSync(process.execPath, [cliPath, ...args], { encoding: "utf8", stdio: "pipe" }) };
  } catch (error) {
    return { code: error.status, err: String(error.stderr) };
  }
};

test("links outside http(s) never reach an href", () => {
  const model = buildModel({ repo: "o/r", now: NOW, issues: [issue(1, { url: "javascript:alert(1)" }), issue(2, { url: "https://github.com/o/r/issues/2" })] });
  assert.deepEqual(model.issues.map((i) => i.url), ["#", "https://github.com/o/r/issues/2"]);
  assert.equal(safeUrl("data:text/html,x"), "#");
});

test("status labels match case-insensitively", () => {
  assert.equal(statusOf(issue(1, { labels: [{ name: "Needs-Decision" }] })), "decision");
});

test("closed issues age from updatedAt when closedAt is missing, and closedDays 0 marks none recent", () => {
  const stale = issue(1, { state: "CLOSED", closedAt: null, updatedAt: "2026-01-01T00:00:00Z" });
  const fresh = issue(2, { state: "CLOSED", closedAt: null, updatedAt: "2026-10-20T00:00:00Z" });
  const all = buildModel({ repo: "o/r", now: NOW, issues: [stale, fresh] }).issues;
  assert.deepEqual(all.map((i) => [i.n, i.recent]), [[1, false], [2, true]]);
  const none = buildModel({ repo: "o/r", now: NOW, closedDays: 0, issues: [fresh] }).issues;
  assert.deepEqual(none.map((i) => [i.n, i.recent]), [[2, false]]);
});

test("a thousand issues build and render", () => {
  const issues = Array.from({ length: 1500 }, (_, i) => issue(i + 1, { labels: [{ name: `area:G${i % 40}` }] }));
  const model = buildModel({ repo: "o/r", now: NOW, issues });
  assert.equal(model.issues.length, 1500);
  assert.equal(model.groups.length, 40);
  assert.ok(renderHtml(model).length > 1000);
});

test("bad group rules and non-array issues fail with a clear message", () => {
  assert.throws(() => normaliseGroups({ rules: [["Voice", ["("]]] }), /invalid pattern/);
  assert.throws(() => normaliseGroups({ rules: ["Voice"] }), /must be \["Group"/);
  assert.throws(() => buildModel({ repo: "o/r", issues: { a: 1 } }), /JSON array/);
  const g = normaliseGroups({ issues: {} });
  assert.equal(groupOf(issue(1, { number: "constructor" }), g), "Other");
});

test("cli argument edge cases", () => {
  assert.match(runCli(["--bogus", "1"]).err, /unknown option/);
  assert.match(runCli(["--out", "--repo", "o/r"]).err, /needs a value/);
  assert.match(runCli(["--limit", "5"]).err, /unknown option/);
  assert.match(runCli(["--max", "0"]).err, /at least 1/);
  assert.match(runCli(["--max", "1e3"]).err, /whole number/);
  assert.match(runCli(["--closed-days", "-1"]).err, /whole number/);
  assert.match(runCli(["stray"]).err, /unexpected argument/);
  assert.equal(runCli(["--version"]).out.trim(), "0.1.0");
});

test("cli reads BOM json, reports bad json and missing gh clearly, creates the output folder", () => {
  const dir = mkdtempSync(join(tmpdir(), "right-issues-"));
  writeFileSync(join(dir, "issues.json"), `﻿${JSON.stringify([issue(1)])}`);
  const ok = runCli(["--root", dir, "--repo", "o/r", "--issues", "issues.json", "--out", "sub/dir/issues.html"]);
  assert.equal(ok.code, 0);
  assert.ok(readFileSync(join(dir, "sub", "dir", "issues.html"), "utf8").includes("<!doctype html>"));
  writeFileSync(join(dir, "bad.json"), "{nope");
  assert.match(runCli(["--root", dir, "--repo", "o/r", "--issues", "bad.json"]).err, /not valid JSON/);
  assert.match(runCli(["--root", dir, "--repo", "o/r", "--issues", "missing.json"]).err, /cannot read issues file/);
  const noGh = (() => {
    try {
      execFileSync(process.execPath, [cliPath, "--root", dir, "--repo", "o/r"], { stdio: "pipe", env: { ...process.env, PATH: "", Path: "" } });
    } catch (error) {
      return String(error.stderr);
    }
  })();
  assert.match(noGh, /gh\) is not installed|gh issue list failed/);
});

test("group labels resolve by prefix priority then sorted name, whatever the order", () => {
  const g = normaliseGroups(undefined);
  const labels = ["area:Hub", "component:Voice", "feature:Search", "group:Core", "component:Alpha"];
  const expected = groupOf(issue(1, { labels }), g);
  assert.equal(expected, "Core");
  for (const order of [labels, [...labels].reverse(), [labels[2], labels[4], labels[0], labels[3], labels[1]]]) {
    assert.equal(groupOf(issue(1, { labels: order.map((name) => ({ name })) }), g), expected);
  }
  assert.equal(groupOf(issue(1, { labels: ["area:Hub", "component:Voice"] }), g), "Voice");
  assert.equal(groupOf(issue(1, { labels: ["component:Voice", "area:Hub"] }), g), "Voice");
  assert.equal(groupOf(issue(1, { labels: ["component:b", "component:A"] }), g), "A");
  assert.equal(groupOf(issue(1, { labels: ["component:A", "component:b"] }), g), "A");
});

test("group names are trimmed and de-duplicated case-insensitively, first declared spelling wins", () => {
  const g = normaliseGroups({ groups: ["  Hub ", "hub", "HUB", "Voice", "", "voice"] });
  assert.deepEqual(g.groups, ["Hub", "Voice"]);
  const model = buildModel({
    repo: "o/r", now: NOW, groupsFile: { groups: [" Hub", "hub"], issues: { 3: "HUB " } },
    issues: [issue(1, { labels: ["area:hub"] }), issue(2, { labels: ["area:  HUB  "] }), issue(3), issue(4, { labels: ["area:zed"] }), issue(5, { labels: ["area:Zed"] })],
  });
  assert.deepEqual(model.groups, ["Hub", "Zed"]);
  assert.deepEqual(model.issues.map((i) => i.group), ["Hub", "Hub", "Hub", "Zed", "Zed"]);
  const flipped = buildModel({ repo: "o/r", now: NOW, issues: [issue(5, { labels: ["area:Zed"] }), issue(4, { labels: ["area:zed"] })] });
  assert.deepEqual(flipped.groups, ["Zed"]);
});

test("REST issues map to the model's shape and pull requests are dropped", () => {
  const rest = [
    { number: 1, title: "A", state: "open", labels: [{ name: "blocked" }], updated_at: "2026-10-20T00:00:00Z", closed_at: null, html_url: "https://github.com/o/r/issues/1", url: "https://api.github.com/repos/o/r/issues/1", body: null },
    { number: 2, title: "PR", state: "open", labels: [], pull_request: {}, html_url: "https://github.com/o/r/pull/2" },
    { number: 3, title: "C", state: "closed", labels: [], updated_at: "2026-01-01T00:00:00Z", closed_at: "2026-01-01T00:00:00Z", html_url: "https://github.com/o/r/issues/3" },
  ];
  const mapped = fromRestIssues(rest);
  assert.deepEqual(mapped.map((i) => [i.number, i.state, i.url]), [[1, "OPEN", "https://github.com/o/r/issues/1"], [3, "CLOSED", "https://github.com/o/r/issues/3"]]);
  assert.throws(() => fromRestIssues([rest[0], rest[0]]), /appears twice/);
});

const sandbox = (existing) => {
  const dir = mkdtempSync(join(tmpdir(), "right-issues-"));
  if (existing !== undefined) writeFileSync(join(dir, "issues.html"), existing);
  return dir;
};
const quiet = (fn) => {
  const log = console.log;
  console.log = () => {};
  try { return fn(); } finally { console.log = log; }
};
const old = (n, closed) => issue(n, closed ? { state: "CLOSED", closedAt: "2020-01-01T00:00:00Z", updatedAt: "2020-01-01T00:00:00Z" } : {});

test("main uses an injected fetcher, never gh, and keeps old closed issues in the page", () => {
  const dir = sandbox();
  const calls = [];
  quiet(() => main(["--root", dir, "--repo", "o/r"], {
    now: NOW,
    fetchIssues: (args) => (calls.push(args), { issues: [old(1), old(2, true)], total: 2 }),
    resolveRepo: () => assert.fail("repo was given"),
  }));
  assert.equal(calls.length, 1);
  const html = readFileSync(join(dir, "issues.html"), "utf8");
  assert.match(html, /"n":2,/);
  assert.match(html, /"recent":false/);
  assert.match(html, /\["done","Done"\]/);
});

test("incomplete or failed retrieval refuses to overwrite an existing issues.html", () => {
  const dir = sandbox("KEEP");
  const attempts = [
    { fetchIssues: () => ({ issues: [old(1)], total: 5 }), message: /incomplete: fetched 1 of 5/ },
    { fetchIssues: () => { throw new Error("gh api failed: HTTP 502"); }, message: /HTTP 502/ },
    { fetchIssues: () => ({ issues: undefined }), message: /no list/ },
    { fetchIssues: () => ({ issues: [old(1), old(2)], total: 2 }), message: /more than --max 1/, extra: ["--max", "1"] },
  ];
  for (const { fetchIssues, message, extra = [] } of attempts) {
    assert.throws(() => quiet(() => main(["--root", dir, "--repo", "o/r", ...extra], { fetchIssues })), message);
    assert.equal(readFileSync(join(dir, "issues.html"), "utf8"), "KEEP");
  }
});

test("local arguments are validated before any fetch", () => {
  const dir = sandbox();
  const boom = { fetchIssues: () => assert.fail("fetched"), resolveRepo: () => assert.fail("resolved") };
  assert.throws(() => main(["--root", dir, "--closed-days", "x"], boom), /closed-days/);
  assert.throws(() => main(["--root", dir, "--max", "0"], boom), /--max/);
  assert.throws(() => main(["--root", join(dir, "nope")], boom), /not a directory/);
  assert.throws(() => main(["--root", dir, "--repo", "bad repo"], boom), /owner\/name/);
  assert.throws(() => main(["--root", dir, "--groups", "missing.json"], boom), /groups file not found/);
  writeFileSync(join(dir, "g.json"), JSON.stringify({ rules: [["V", ["("]]] }));
  assert.throws(() => main(["--root", dir, "--groups", "g.json"], boom), /invalid pattern/);
  writeFileSync(join(dir, "bad.json"), "{nope");
  assert.throws(() => main(["--root", dir, "--issues", "bad.json"], boom), /not valid JSON/);
  assert.ok(!existsSync(join(dir, "issues.html")));
});
