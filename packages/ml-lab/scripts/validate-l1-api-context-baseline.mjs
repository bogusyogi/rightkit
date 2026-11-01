#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const forbidden = [
  /authority/i,
  /expected output/i,
  /required checks/i,
  /source_order/i,
  /clip_id/i,
  /script_ref/i,
  /Downloads[\\/]+scripts\.md/i,
  /Downloads[\\/]+bakeoff\.md/i,
];

function scanFile(file) {
  const text = fs.readFileSync(file, "utf8");
  return forbidden.filter((pattern) => pattern.test(text)).map((pattern) => `${file}: ${pattern}`);
}

function scanDir(dir) {
  const hits = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) hits.push(...scanDir(file));
    else if (/\.(json|jsonl|txt|md)$/i.test(entry.name)) hits.push(...scanFile(file));
  }
  return hits;
}

function selfTest() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hr-l1-context-"));
  fs.writeFileSync(path.join(dir, "good.json"), JSON.stringify({ vocabulary: ["Groq", "Cerebras"] }));
  fs.writeFileSync(path.join(dir, "bad.json"), JSON.stringify({ source_order: 29, note: "expected output says Groq" }));
  const hits = scanDir(dir);
  fs.rmSync(dir, { recursive: true, force: true });
  if (hits.length < 2) throw new Error("context baseline self-test failed");
  console.log("l1 context baseline self-test ok");
}

if (process.argv.includes("--self-test")) {
  selfTest();
  process.exit(0);
}

const target = process.argv[2];
if (!target) {
  console.error("usage: node scripts/validate-l1-api-context-baseline.mjs <payload-dir>");
  process.exit(2);
}

const hits = scanDir(path.resolve(target));
if (hits.length) {
  console.error(`context baseline failed: ${hits.length} forbidden hit(s)`);
  for (const hit of hits.slice(0, 50)) console.error(`- ${hit}`);
  process.exit(1);
}
console.log(`context baseline ok: ${target}`);
