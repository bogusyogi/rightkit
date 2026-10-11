#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream, readFileSync, realpathSync } from "node:fs";
import { lstat, readdir } from "node:fs/promises";
import path from "node:path";
import { HARDENING_RULES } from "./hardening-evidence.mjs";
import { QA_CONTROL_MARKER } from "./qa-control-scan.mjs";

const rawArgs = process.argv.slice(2);
if (rawArgs.length === 0 || rawArgs.includes("-h") || rawArgs.includes("--help")) {
  console.error("usage: node hardeningscan.mjs [--allow-evidence <json>] <artifact-path>...");
  process.exit(rawArgs.length === 0 ? 2 : 0);
}
const allowIndex = rawArgs.indexOf("--allow-evidence");
const allowEvidencePath = allowIndex >= 0 ? rawArgs[allowIndex + 1] : null;
if (allowIndex >= 0 && !allowEvidencePath) throw new Error("--allow-evidence requires a JSON path");
// Only strip the flag pair when the flag is present: with allowIndex = -1,
// filtering out index `allowIndex + 1` would silently drop the first
// artifact path (a lone artifact arg then failed as "no artifact path").
const args = allowIndex >= 0
  ? rawArgs.filter((_, index) => index !== allowIndex && index !== allowIndex + 1)
  : rawArgs;
if (!args.length) throw new Error("at least one artifact path is required");

const root = realpathSync(process.cwd());
const findings = [];
const evidence = allowEvidencePath ? JSON.parse(readFileSync(allowEvidencePath, "utf8")) : { allowances: [] };
if (allowEvidencePath && (evidence.schemaVersion !== 1 || evidence.kind !== "rightkit-hardening-evidence")) {
  throw new Error("invalid hardening evidence");
}
const allowances = evidence.allowances ?? [];
for (const allowance of allowances) {
  const sourceMatch = typeof allowance.sourceEvidence === "string" ? /^(.*):([1-9]\d*)$/.exec(allowance.sourceEvidence) : null;
  const knownRule = HARDENING_RULES[allowance.rule];
  if (!knownRule || allowance.exact !== knownRule.exact || !sourceMatch || !/^[a-f0-9]{64}$/.test(allowance.artifactSha256 ?? "") || !/^[a-f0-9]{64}$/.test(allowance.sourceSha256 ?? "") || allowance.sourceLine !== Number(sourceMatch[2]) || typeof allowance.rationale !== "string" || allowance.rationale.trim().length < 12) {
    throw new Error("invalid hardening allowance source binding");
  }
  const sourcePath = realpathSync(path.resolve(root, sourceMatch[1]));
  const sourceRelative = path.relative(root, sourcePath);
  if (sourceRelative === ".." || sourceRelative.startsWith("../") || sourceRelative.startsWith("..\\") || path.isAbsolute(sourceRelative)) {
    throw new Error("hardening allowance source escapes release root");
  }
  const sourceBytes = readFileSync(sourcePath);
  if (createHash("sha256").update(sourceBytes).digest("hex") !== allowance.sourceSha256) throw new Error("hardening allowance source digest mismatch");
  const line = sourceBytes.toString("utf8").split(/\r?\n/)[allowance.sourceLine - 1];
  if (typeof line !== "string" || !line.includes(allowance.exact)) throw new Error("hardening allowance source token mismatch");
}

const fileNameRules = [
  [/\.map$/i, "source map shipped"],
  [/\.pdb$/i, "Windows debug symbols shipped"],
  [/\.dSYM(?:$|[/\\])/i, "macOS debug symbols shipped"],
  [/\.mlpackage(?:$|[/\\])/i, "raw CoreML package shipped"],
  [/[/\\]docs[/\\]experiments(?:$|[/\\])/i, "experiment docs shipped"],
  [/[/\\]\.scratch(?:$|[/\\])/i, "scratch directory shipped"],
  [/[/\\]\.git(?:$|[/\\])/i, "git metadata shipped"],
  [/[/\\]node_modules(?:$|[/\\])/i, "node_modules shipped"],
  [/[/\\](?:src|tests?|benches|benchmarks)(?:$|[/\\])/i, "source/test tree shipped"],
  [/[/\\]tdt_w15_p6(?:$|[/\\])/i, "stale TDT p6 model shipped"],
];

const contentRules = [
  ["local-source-path", /\/Users\/adrdsouza\/claude/gi, "local source path leaked"],
  ["local-cargo-path", /\/Users\/adrdsouza\/\.cargo/gi, "local cargo path leaked"],
  ["local-windows-path", /C:\\Users\\adrdsouza\\(?:claude|\.cargo)/gi, "local Windows source path leaked"],
  ["experiment-path", /docs\/experiments/gi, "experiment path leaked"],
  ["cleanup-prompt-marker", /l3_cleanup_prompt/gi, "L3 cleanup prompt marker leaked"],
  ["system-prompt-marker", /SYSTEM_PROMPT/gi, "system prompt marker leaked"],
  ["secret-env-name", /OPENAI_API_KEY|ANTHROPIC_API_KEY|APPLE_PASSWORD|NOTARY_PROFILE/gi, "secret/env key name leaked"],
  ["gpl-flag", /--enable-gpl|--enable-nonfree/gi, "GPL/nonfree ffmpeg flag leaked"],
  ["stale-model", /tdt_w15_p6/gi, "stale TDT p6 string leaked"],
  // Not allowable through hardening evidence: a shipped artifact never contains the QA control server.
  ["qa-control-marker", new RegExp(QA_CONTROL_MARKER, "g"), "QA control server shipped (rightkit-control qa-control)"],
  ["experiment-model", /Canary-Qwen|Nemotron|dqlstm|bias-experiments/gi, "experiment/model-history marker leaked"],
];

const maxPatternLen = Math.max(...contentRules.map(([, rule]) => String(rule).length), 256);

function add(kind, file, message, detail = "") {
  findings.push({ kind, file: path.relative(root, file) || file, message, detail });
}

async function walk(target) {
  const st = await lstat(target);
  const normalized = target.split(path.sep).join("/");
  for (const [rule, message] of fileNameRules) if (rule.test(normalized)) add("path", target, message);
  if (st.isDirectory()) {
    const entries = await readdir(target);
    await Promise.all(entries.map((name) => walk(path.join(target, name))));
    return;
  }
  if (st.isFile()) await scanFile(target);
}

function scanFile(file) {
  return new Promise((resolveScan) => {
    const artifactSha256 = createHash("sha256").update(readFileSync(file)).digest("hex");
    const stream = createReadStream(file, { highWaterMark: 256 * 1024 });
    let tail = "";
    stream.on("data", (chunk) => {
      const text = tail + chunk.toString("latin1");
      for (const [ruleId, rule, message] of contentRules) {
        if (/ffmpeg-LICENSE\.txt$/i.test(file) && ruleId === "gpl-flag") continue;
        rule.lastIndex = 0;
        for (const match of text.matchAll(rule)) {
          const allowed = allowances.some((allowance) =>
            allowance.rule === ruleId &&
            allowance.artifactSha256 === artifactSha256 &&
            allowance.exact === match[0] &&
            typeof allowance.sourceEvidence === "string" &&
            typeof allowance.rationale === "string"
          );
          if (!allowed) add("content", file, message, rule.source);
        }
      }
      tail = text.slice(-maxPatternLen);
    });
    stream.on("error", (error) => {
      add("read", file, "could not scan file", error.message);
      resolveScan();
    });
    stream.on("end", resolveScan);
  });
}

for (const arg of args) {
  const target = path.resolve(arg);
  try {
    await walk(target);
  } catch (error) {
    add("target", target, "could not scan target", error.message);
  }
}

const unique = new Map();
for (const finding of findings) unique.set([finding.kind, finding.file, finding.message].join("\0"), finding);
const deduped = [...unique.values()].sort((left, right) =>
  (left.file + " " + left.message).localeCompare(right.file + " " + right.message),
);

if (deduped.length) {
  console.error("hardeningscan: " + deduped.length + " finding(s)");
  for (const finding of deduped) console.error("- [" + finding.kind + "] " + finding.file + ": " + finding.message);
  process.exit(1);
}

console.log("hardeningscan: clean");
