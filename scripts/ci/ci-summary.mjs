#!/usr/bin/env node
// Final aggregate consumes exact plan counts plus reports from every planned shard.
import fs from "node:fs";
import path from "node:path";
import { evidenceVerdict, summaryVerdict } from "./shards.mjs";

try {
  const needs = JSON.parse(process.env.NEEDS ?? "");
  const outputs = needs.plan?.outputs ?? {};
  const counts = {};
  for (const host of ["windows", "macos"]) {
    for (const [family, key] of [["shards", "shard"], ["closure", "closure"]]) {
      const value = outputs[`${host}_${key}_count`];
      counts[`${family}-${host}`] = /^\d+$/.test(value ?? "") ? Number(value) : NaN;
    }
  }
  const required = ["private-repo-guard", "plan", "hygiene", "ownership"];
  // Compile baselines prove host work only; ownership still blocks overall CI below.
  const compileRequired = ["private-repo-guard", "plan", "hygiene"];
  const aggregate = summaryVerdict(needs, { required, counts });
  const expected = JSON.parse(outputs.evidence ?? "null");
  if (!Array.isArray(expected) || expected.length !== Object.values(counts).reduce((sum, count) => sum + count, 0)) throw new Error("evidence plan does not match planned matrix counts");
  for (const host of ["windows", "macos"]) {
    if (expected.filter((job) => job.host === host && job.steps === "test").length !== counts[`shards-${host}`] || expected.filter((job) => job.host === host && job.steps === "closure").length !== counts[`closure-${host}`]) throw new Error(`${host}: evidence plan does not match shard counts`);
  }
  const reports = {};
  if (Array.isArray(expected)) for (const job of expected) {
    if (!/^qa-evidence-[a-z0-9-]+$/.test(job.artifact ?? "")) throw new Error("invalid evidence artifact name");
    try { reports[job.artifact] = JSON.parse(fs.readFileSync(path.join(process.env.EVIDENCE_DIR ?? "ci-evidence", job.artifact, "summary.json"), "utf8")); } catch { /* verdict names missing reports */ }
  }
  const evidence = evidenceVerdict(expected, reports, process.env.GITHUB_SHA);
  for (const host of ["windows", "macos"]) {
    const keys = [...compileRequired, `shards-${host}`, `closure-${host}`];
    const hostNeeds = Object.fromEntries(keys.map((key) => [key, needs[key]]));
    const hostCounts = Object.fromEntries(Object.entries(counts).filter(([key]) => key.endsWith(`-${host}`)));
    const jobs = summaryVerdict(hostNeeds, { required: compileRequired, counts: hostCounts });
    const proof = evidenceVerdict(expected.filter((job) => job.host === host), reports, process.env.GITHUB_SHA);
    if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, `${host}_green=${jobs.ok && proof.ok}\n`);
  }
  for (const [name, job] of Object.entries(needs)) console.log(`${name}: ${job?.result}`);
  if (!aggregate.ok || !evidence.ok) throw new Error([...aggregate.failures, ...evidence.failures].join(", "));
  console.log(`ci-summary: every required job passed; ${expected.length} shard reports match this plan`);
} catch (error) { console.error(`ci-summary: ${error.message}`); process.exit(1); }
