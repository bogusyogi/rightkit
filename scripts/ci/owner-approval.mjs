#!/usr/bin/env node
// One-off owner-approved publication outside the gate and broker (Adrian, 2026-10-31, rightkit#1322): "You will publish those
// outside the gate on my explicit approval. Once validated and discussed with validate". The broker-only publish.yml route stays
// as it is; this path only uploads the exact crates and versions a committed approval file names, from a commit whose
// only change since the validated source commit is the approval itself, and only after that source commit passed the
// native CI matrix (ci.yml dispatched by hand, macOS and Windows jobs all green, none skipped).
import fs from "node:fs";
import { execFileSync } from "node:child_process";

const fail = (message) => { console.error(`::error::${message}`); process.exit(1); };
const need = (ok, message) => { if (!ok) fail(message); };
const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
const api = async (route) => {
  const res = await fetch(`https://api.github.com/repos/${process.env.GITHUB_REPOSITORY}/${route}`, { headers: { authorization: `Bearer ${process.env.GH_TOKEN}`, accept: "application/vnd.github+json" } });
  need(res.ok, `GitHub API ${route}: ${res.status}`);
  return res.json();
};

const file = process.env.APPROVAL ?? "";
need(/^ci\/owner-approvals\/[A-Za-z0-9._-]+\.json$/.test(file) && fs.existsSync(file), `no approval file ${file}`);
need(process.env.GITHUB_REF === "refs/heads/main", "owner publication runs from main only");
const approval = JSON.parse(fs.readFileSync(file, "utf8"));
need(approval.schema === 1 && approval.approvedBy === "Adrian" && /^https:\/\/github\.com\//.test(approval.issue ?? "") && typeof approval.quote === "string" && approval.quote.length > 0, "approval needs schema 1, approvedBy Adrian, the issue URL and his words");
need(/^[0-9a-f]{40}$/.test(approval.sourceSha ?? "") && /^[1-9][0-9]*$/.test(String(approval.ciRunId ?? "")), "approval needs the validated source commit and its CI run id");
need(Array.isArray(approval.units) && approval.units.length > 0 && approval.units.every((u) => u.registry === "crates" && /^rightkit-[a-z0-9-]+$/.test(u.name) && /^\d+\.\d+\.\d+$/.test(u.version)), "approval units must be rightkit crates with exact versions");

// The commit being published differs from the validated source only by approval files and this path's own two files.
const head = process.env.GITHUB_SHA;
git("merge-base", "--is-ancestor", approval.sourceSha, head);
const changed = git("diff", "--name-only", approval.sourceSha, head).split("\n").filter(Boolean);
need(changed.every((p) => p.startsWith("ci/owner-approvals/") || p === ".github/workflows/publish-owner.yml" || p === "scripts/ci/owner-approval.mjs"), `commits after the validated source change more than the approval: ${changed.join(", ")}`);

// The source commit passed the full native matrix.
const run = await api(`actions/runs/${approval.ciRunId}`);
need(run.path === ".github/workflows/ci.yml" && run.event === "workflow_dispatch" && run.head_sha === approval.sourceSha && run.conclusion === "success", `CI run ${approval.ciRunId} is not a green hand-dispatched ci.yml run of ${approval.sourceSha}`);
const jobs = (await api(`actions/runs/${approval.ciRunId}/jobs?per_page=100`)).jobs;
const native = jobs.filter((j) => /^(macos|windows) /.test(j.name));
need(native.length > 0 && native.every((j) => j.conclusion === "success"), `native CI jobs not all green: ${native.map((j) => `${j.name}=${j.conclusion}`).join(", ")}`);

for (const unit of approval.units) {
  const toml = fs.readFileSync(`crates/${unit.name}/Cargo.toml`, "utf8");
  const version = /^version\s*=\s*"([^"]+)"/m.exec(toml)?.[1];
  need(version === unit.version, `${unit.name} is ${version} in source, approval says ${unit.version}`);
  const res = await fetch(`https://crates.io/api/v1/crates/${unit.name}/${unit.version}`, { headers: { "user-agent": "rightkit-owner-approval" } });
  need(res.status === 404, `${unit.name}@${unit.version} is already on crates.io (or the registry answered ${res.status})`);
}
fs.appendFileSync(process.env.GITHUB_OUTPUT, `crates=${approval.units.map((u) => u.name).join(" ")}\n`);
console.log(`approved: ${approval.units.map((u) => `${u.name}@${u.version}`).join(", ")} from ${approval.sourceSha} (CI ${approval.ciRunId})`);
