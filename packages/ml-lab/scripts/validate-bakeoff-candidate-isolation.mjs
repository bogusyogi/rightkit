#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const forbiddenKeys = new Set([
  "authority",
  "checks",
  "clip_id",
  "deterministic_output",
  "deterministic_pass",
  "expected",
  "expected_output",
  "ground_truth",
  "required_checks",
  "script_ref",
  "source",
  "source_order",
  "wispr",
]);

const forbiddenText = [
  /Downloads[\\/]+scripts\.md/i,
  /Downloads[\\/]+bakeoff\.md/i,
  /ground truth/i,
  /\bauthority\b/i,
  /\banswer key\b/i,
  /\bexpected output\b/i,
  /local_scoring_authority/i,
  /mapped_clips_with_wispr/i,
  /locked_asr_32/i,
  /deterministic_(?:output|pass|l1)/i,
  /l1_phase0_5_deterministic/i,
];

const allowedExtensions = new Set([
  ".json",
  ".jsonl",
  ".txt",
  ".md",
  ".mjs",
  ".js",
  ".py",
  ".rs",
]);

function usage() {
  console.error(
    "usage: node scripts/validate-bakeoff-candidate-isolation.mjs <candidate-visible-file-or-dir>...",
  );
  console.error("       node scripts/validate-bakeoff-candidate-isolation.mjs --self-test");
}

function listFiles(target) {
  const stat = fs.statSync(target);
  if (stat.isDirectory()) {
    return fs
      .readdirSync(target, { withFileTypes: true })
      .flatMap((entry) => listFiles(path.join(target, entry.name)));
  }
  if (!allowedExtensions.has(path.extname(target).toLowerCase())) return [];
  return [target];
}

function addKeyViolations(value, where, violations) {
  if (Array.isArray(value)) {
    for (const [index, item] of value.entries()) {
      addKeyViolations(item, `${where}[${index}]`, violations);
    }
    return;
  }
  if (!value || typeof value !== "object") return;
  for (const [key, child] of Object.entries(value)) {
    const childWhere = where ? `${where}.${key}` : key;
    if (forbiddenKeys.has(key.toLowerCase())) {
      violations.push({ where: childWhere, reason: `forbidden key '${key}'` });
    }
    addKeyViolations(child, childWhere, violations);
  }
}

function parseJsonl(text) {
  const rows = [];
  for (const [index, line] of text.split(/\r?\n/).entries()) {
    if (!line.trim()) continue;
    rows.push({ where: `$line${index + 1}`, value: JSON.parse(line) });
  }
  return rows;
}

function validateFile(file) {
  const text = fs.readFileSync(file, "utf8");
  const violations = [];

  for (const pattern of forbiddenText) {
    if (pattern.test(text)) {
      violations.push({ where: "$text", reason: `forbidden text ${pattern}` });
    }
  }

  const ext = path.extname(file).toLowerCase();
  try {
    if (ext === ".json") {
      addKeyViolations(JSON.parse(text), "$", violations);
    } else if (ext === ".jsonl") {
      for (const row of parseJsonl(text)) addKeyViolations(row.value, row.where, violations);
    }
  } catch (error) {
    violations.push({ where: "$parse", reason: `invalid JSON: ${error.message}` });
  }

  return violations.map((violation) => ({ file, ...violation }));
}

function validatePaths(paths) {
  const targetResults = paths.map((target) => {
    const resolved = path.resolve(target);
    const files = listFiles(resolved);
    const stat = fs.statSync(resolved);
    const unsupportedDirectFile = stat.isFile() && files.length === 0;
    return { resolved, files, unsupportedDirectFile };
  });
  const files = [...new Set(targetResults.flatMap((result) => result.files))];
  const targetViolations = targetResults.flatMap((result) => {
    if (result.unsupportedDirectFile) {
      return [{
        file: result.resolved,
        where: "$target",
        reason: `unsupported extension '${path.extname(result.resolved) || "<none>"}'`,
      }];
    }
    if (result.files.length === 0) {
      return [{
        file: result.resolved,
        where: "$target",
        reason: "target contains zero inspectable files",
      }];
    }
    return [];
  });
  const violations = [...targetViolations, ...files.flatMap(validateFile)];
  return { files, violations };
}

function selfTest() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hr-bakeoff-isolation-"));
  const good = path.join(dir, "good.json");
  const bad = path.join(dir, "bad.json");
  const nested = path.join(dir, "nested");
  const badPython = path.join(dir, "bad.py");
  const goodRust = path.join(dir, "good.rs");
  const badRust = path.join(dir, "bad.rs");
  const unsupported = path.join(dir, "ignored.bin");
  fs.mkdirSync(nested);
  const nestedBad = path.join(nested, "payload.json");
  fs.writeFileSync(
    good,
    JSON.stringify({
      row_id: "opaque_001",
      model: "llama-3.1-8b-instant",
      messages: [{ role: "user", content: "Transcript: grok should route quota" }],
    }),
  );
  fs.writeFileSync(
    bad,
    JSON.stringify({
      row_id: "l1_29_bad",
      source_order: 29,
      checks: ["Groq"],
      messages: [{ role: "user", content: "ground truth says Groq" }],
    }),
  );
  fs.writeFileSync(nestedBad, JSON.stringify({ clip_id: 7 }));
  fs.writeFileSync(badPython, 'SCORER = "local_scoring_authority"\n');
  fs.writeFileSync(goodRust, 'const MODEL: &str = "qwen/qwen3.6-27b";\n');
  fs.writeFileSync(badRust, 'const LEAK: &str = "ground truth";\n');
  fs.writeFileSync(unsupported, "not inspectable");
  const goodResult = validatePaths([good]);
  const badResult = validatePaths([bad]);
  const nestedResult = validatePaths([nested]);
  const pythonResult = validatePaths([badPython]);
  const goodRustResult = validatePaths([goodRust]);
  const badRustResult = validatePaths([badRust]);
  const unsupportedResult = validatePaths([unsupported]);
  fs.rmSync(dir, { recursive: true, force: true });
  if (
    goodResult.violations.length !== 0 ||
    badResult.violations.length < 3 ||
    nestedResult.violations.length !== 1 ||
    pythonResult.files.length !== 1 ||
    pythonResult.violations.length !== 1 ||
    goodRustResult.files.length !== 1 ||
    goodRustResult.violations.length !== 0 ||
    badRustResult.files.length !== 1 ||
    badRustResult.violations.length !== 1 ||
    unsupportedResult.files.length !== 0 ||
    unsupportedResult.violations.length !== 1
  ) {
    throw new Error("self-test failed");
  }
  console.log("candidate isolation self-test ok");
}

if (process.argv.includes("--self-test")) {
  selfTest();
  process.exit(0);
}

const targets = process.argv.slice(2).filter((arg) => arg !== "--");
if (targets.length === 0) {
  usage();
  process.exit(2);
}

const { files, violations } = validatePaths(targets);
if (violations.length) {
  console.error(
    `candidate isolation failed: ${violations.length} violation(s) in ${files.length} file(s)`,
  );
  for (const violation of violations.slice(0, 50)) {
    console.error(
      `- ${path.relative(process.cwd(), violation.file)} ${violation.where}: ${violation.reason}`,
    );
  }
  if (violations.length > 50) console.error(`- ... ${violations.length - 50} more`);
  process.exit(1);
}

console.log(`candidate isolation ok: ${files.length} file(s)`);
