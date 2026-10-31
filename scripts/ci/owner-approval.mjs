#!/usr/bin/env node
// One-off owner-approved publication outside the gate and broker (Adrian, 2026-10-31, rightkit#1322): "You will publish those
// outside the gate on my explicit approval. Once validated and discussed with validate"; then "Heardright HAS to go live
// asap ... Any blockers it has must be [peer] validated ... and published. Immediately. And outside the gate means outside
// the broker. Not outside the class a". publish.yml (broker admission) stays as it is. This path is scoped to the #1322
// units below; an approval file can only narrow it. Authorization is the owner's push of the approval file to main and the
// write access a workflow_dispatch needs; the file's text is a record, not a credential.
import fs from "node:fs";
import { execFileSync } from "node:child_process";

const ISSUE = "https://github.com/bogusyogi/rightkit/issues/1322";
const ALLOWED = { "rightkit-license": "0.2.6", "rightkit-models": "0.5.2" };
const CLASS_A_JOBS = ["private-repo-guard", "plan", "hygiene", "ownership"];

const fail = (message) => {
  console.error(`::error::${message}`);
  process.exit(1);
};
const need = (ok, message) => {
  if (!ok) fail(message);
};
const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
const headers = { authorization: `Bearer ${process.env.GH_TOKEN}`, accept: "application/vnd.github+json", "user-agent": "rightkit-owner-approval" };
const api = async (route, raw = false) => {
  const res = await fetch(`https://api.github.com/repos/${process.env.GITHUB_REPOSITORY}/${route}`, { headers });
  need(res.ok, `GitHub API ${route}: ${res.status}`);
  return raw ? res.text() : res.json();
};

const file = process.env.APPROVAL ?? "";
need(/^ci\/owner-approvals\/[A-Za-z0-9._-]+\.json$/.test(file) && fs.existsSync(file), `no approval file ${file}`);
need(process.env.GITHUB_REF === "refs/heads/main", "owner publication runs from main only");
const approval = JSON.parse(fs.readFileSync(file, "utf8"));
need(approval.schema === 2 && approval.issue === ISSUE, `approval must be schema 2 for ${ISSUE}`);
need(/^[0-9a-f]{40}$/.test(approval.sourceSha ?? "") && /^[1-9][0-9]*$/.test(String(approval.ciRunId ?? "")) && /^[1-9][0-9]*$/.test(String(approval.ciRunAttempt ?? "")), "approval needs the validated source commit, its CI run id and attempt");
need(Array.isArray(approval.units) && approval.units.length > 0, "approval names no units");
for (const u of approval.units) need(ALLOWED[u.name] === u.version, `${u.name}@${u.version} is outside the #1322 scope (${JSON.stringify(ALLOWED)})`);
need(new Set(approval.units.map((u) => u.name)).size === approval.units.length, "duplicate unit");

// The source commit already holds this checker and its workflow, so the CI run's Class A covered the publishing code too.
// Only approval files may change after it.
const head = process.env.GITHUB_SHA;
git("merge-base", "--is-ancestor", approval.sourceSha, head);
const changed = git("diff", "--name-only", approval.sourceSha, head).split("\n").filter(Boolean);
need(changed.every((p) => p.startsWith("ci/owner-approvals/") && p.endsWith(".json")), `commits after the validated source change more than approval files: ${changed.join(", ")}`);

// The exact CI attempt: hand-dispatched ci.yml of the source commit, green, Class A jobs green.
const run = await api(`actions/runs/${approval.ciRunId}/attempts/${approval.ciRunAttempt}`);
need(run.path === ".github/workflows/ci.yml" && run.event === "workflow_dispatch" && run.head_sha === approval.sourceSha && run.conclusion === "success", `CI run ${approval.ciRunId} attempt ${approval.ciRunAttempt} is not a green hand-dispatched ci.yml run of ${approval.sourceSha}`);
const jobs = [];
for (let page = 1; ; page++) {
  const batch = (await api(`actions/runs/${approval.ciRunId}/attempts/${approval.ciRunAttempt}/jobs?per_page=100&page=${page}`)).jobs;
  jobs.push(...batch);
  if (batch.length < 100) break;
}
const byName = new Map(jobs.map((j) => [j.name, j]));
for (const name of CLASS_A_JOBS) need(byName.get(name)?.conclusion === "success", `Class A job ${name} is not green`);

// Coverage: every approved crate ran in a macOS shard AND a Windows shard, all green, proven from the plan job's own shard list.
const planLog = execFileSync("gh", ["run", "view", "--repo", process.env.GITHUB_REPOSITORY, "--log", "--job", String(byName.get("plan").id)], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, env: { ...process.env, GH_TOKEN: process.env.GH_TOKEN } }).replace(/\\"/g, '"');
// Each host has its own shard list (the plan's macos_shards and windows_shards outputs); ids may repeat across hosts.
const hostShards = (os) => {
  const list = new RegExp(`"${os}_shards": "(\\{.*?\\]\\})"`).exec(planLog)?.[1];
  need(list, `the plan log has no ${os}_shards output`);
  return [...list.matchAll(/\{"id":"([^"]+)","kind":"([^"]+)","tokens":"([^"]*)"/g)].map((m) => ({ id: m[1], kind: m[2], tokens: m[3].split(/\s+/) }));
};
const shardsByHost = { macos: hostShards("macos"), windows: hostShards("windows") };
for (const u of approval.units) {
  for (const os of ["macos", "windows"]) {
    // A test shard names its crates as `rust:<name>` (scripts/ci/shards.mjs); a closure shard's `rust-check:<name>` only compiles.
    const covering = new Set(shardsByHost[os].filter((s) => s.kind === "rust" && s.tokens.includes(`rust:${u.name}`)).map((s) => s.id));
    need(covering.size > 0, `the plan scheduled no ${os} test shard for ${u.name}`);
    const ran = jobs.filter((j) => j.name.startsWith(`${os} `) && covering.has(j.name.slice(os.length + 1)));
    need(ran.length > 0 && ran.every((j) => j.conclusion === "success"), `${u.name}: ${os} shards ${[...covering].join(", ")} not all green (${ran.map((j) => `${j.name}=${j.conclusion}`).join(", ") || "none ran"})`);
  }
}

for (const u of approval.units) {
  const version = /^version\s*=\s*"([^"]+)"/m.exec(fs.readFileSync(`crates/${u.name}/Cargo.toml`, "utf8"))?.[1];
  need(version === u.version, `${u.name} is ${version} in source, approval says ${u.version}`);
  const res = await fetch(`https://crates.io/api/v1/crates/${u.name}/${u.version}`, { headers: { "user-agent": "rightkit-owner-approval" } });
  need(res.status === 404, `${u.name}@${u.version} is already on crates.io (or the registry answered ${res.status})`);
}
fs.appendFileSync(process.env.GITHUB_OUTPUT, `crates=${approval.units.map((u) => u.name).join(" ")}\n`);
console.log(`approved: ${approval.units.map((u) => `${u.name}@${u.version}`).join(", ")} from ${approval.sourceSha} (CI ${approval.ciRunId}/${approval.ciRunAttempt})`);
