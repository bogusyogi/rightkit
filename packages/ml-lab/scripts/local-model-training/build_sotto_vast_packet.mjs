#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const APP = path.resolve(ROOT, "..", "..");
const DATASET = path.resolve(APP, "bakeoff", "l1-sotto-tuning-round1-20260727");
const DEFAULT_OUTPUT = path.resolve(APP, "bakeoff", "sotto-vast-prep-20260727");
const SOURCE_MANIFEST = path.join(DATASET, "manifest.json");
const MODEL_SOURCE = "juanquivilla/sotto-cleanup-lfm25-350m";
const MODEL_REVISION = "6df6f019170b8b55333c047b901886a51750a965";

const sha256 = (file) => `sha256:${crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex")}`;
const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
const copy = (source, target) => { fs.mkdirSync(path.dirname(target), { recursive: true }); fs.copyFileSync(source, target); };
const countRows = (file) => fs.readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).length;
const required = [
  "train.py", "recovered_train_runner.pyc", "prepare_source_checkpoint.py", "requirements.vast-cu126.pins.txt",
  "model-recipes.json", "environment.lock.json", "convert_hf_to_q8.py", "evaluate_gguf.py", "validate_output.mjs",
  "paid_run_watchdog.py", "vast_shutdown_hook.py",
];

const runScript = `#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "\${BASH_SOURCE[0]}")" && pwd)"
PYTHON="\${RIGHTKIT_ML_PYTHON:-/workspace/heardright-runtime-cpython311/bin/python3}"
AUTH="\${1:-\$ROOT/authorization/full.json}"
[[ -x "\${PYTHON}" ]] || { echo "missing CPython 3.11: \${PYTHON}" >&2; exit 64; }
[[ -f "\${AUTH}" ]] || { echo "missing authorization: \${AUTH}" >&2; exit 64; }
python3 - "\${ROOT}/environment.lock.json" "\${AUTH}" <<'PY'
import json, sys
lock = json.load(open(sys.argv[1], encoding="utf-8"))
auth = json.load(open(sys.argv[2], encoding="utf-8"))
if lock.get("status") != "resolved_pass_live":
    raise SystemExit("environment lock is not resolved_pass_live")
if auth.get("approved") is not True:
    raise SystemExit("authorization approved flag is false")
if auth.get("hourly_rate_usd", 0) <= 0 or auth.get("max_usd", 0) <= 0:
    raise SystemExit("authorization must bind positive hourly rate and max USD")
PY
exec "\${PYTHON}" "\${ROOT}/train.py" --family sotto --action train --output-dir "\${OUTPUT_DIR:-/workspace/heardright-sotto-full}" --model-source ${MODEL_SOURCE} --model-revision ${MODEL_REVISION} --live --training-input "\${ROOT}/data/train.jsonl" --mode full --epochs 3 --batch-size 2 --gradient-accumulation 8
`;

function build({ outputDir = DEFAULT_OUTPUT, replace = false } = {}) {
  outputDir = path.resolve(outputDir);
  if (fs.existsSync(outputDir)) {
    const entries = fs.readdirSync(outputDir);
    if (entries.length > 0) {
      if (!replace) throw new Error(`output exists; pass replace:true for exact packet path: ${outputDir}`);
      if (!fs.existsSync(path.join(outputDir, "packet-manifest.json"))) throw new Error("refusing to replace unrecognized output");
      fs.rmSync(outputDir, { recursive: true, force: true });
    }
  }
  for (const file of [SOURCE_MANIFEST, path.join(DATASET, "train.jsonl"), path.join(DATASET, "heldout-synthetic.jsonl")]) {
    if (!fs.existsSync(file)) throw new Error(`missing generated dataset artifact: ${file}`);
  }
  const sourceManifest = JSON.parse(fs.readFileSync(SOURCE_MANIFEST, "utf8"));
  fs.mkdirSync(outputDir, { recursive: true });
  copy(path.join(DATASET, "train.jsonl"), path.join(outputDir, "data", "train.jsonl"));
  copy(path.join(DATASET, "heldout-synthetic.jsonl"), path.join(outputDir, "data", "heldout-synthetic.jsonl"));
  for (const file of required) copy(path.join(ROOT, file), path.join(outputDir, "runtime", file));
  fs.mkdirSync(path.join(outputDir, "authorization"), { recursive: true });
  fs.writeFileSync(path.join(outputDir, "runtime", "run_sotto_vast.sh"), runScript, "utf8");
  fs.writeFileSync(path.join(outputDir, "authorization", "full.template.json"), json({ schema_version: "heardright.sotto.vast_authorization.v1", approved: false, scope: "sotto_full_parameter_training", offer_id: "REPLACE_WITH_EXACT_VAST_OFFER_ID", hourly_rate_usd: 0, max_usd: 20, max_hours: 2, environment_lock_sha256: sha256(path.join(ROOT, "environment.lock.json")), note: "Preparation only. Resolve immutable container/package lock & reviewed offer before copying to full.json." }), "utf8");
  fs.writeFileSync(path.join(outputDir, "RUN.md"), `# Sotto Vast preparation packet\n\nStatus: prepared, not authorized. Dataset is synthetic plus pinned public Sotto replay rows; no private transcript data is included.\n\n1. Use immutable image from \`runtime/requirements.vast-cu126.pins.txt\`.\n2. Install exact pinned packages & resolve \`runtime/environment.lock.json\`; replace its status with \`resolved_pass_live\` only after capturing image, Python, package versions, and hashes.\n3. Copy \`authorization/full.template.json\` to \`full.json\`, bind exact offer/rate, keep \`approved=false\` until local packet review.\n4. Download exact model source: \`python runtime/prepare_source_checkpoint.py --model-source ${MODEL_SOURCE} --model-revision ${MODEL_REVISION} --output-dir /workspace/sotto-source\`.\n5. After environment and authorization gates pass, run \`bash runtime/run_sotto_vast.sh authorization/full.json\`.\n\nThe current packet intentionally refuses launch because environment lock is unresolved.\n`, "utf8");
  const files = [];
  const walk = (current) => { for (const entry of fs.readdirSync(current, { withFileTypes: true })) { const full = path.join(current, entry.name); if (entry.isDirectory()) walk(full); else if (entry.name !== "packet-manifest.json") files.push({ path: path.relative(outputDir, full).replaceAll("\\", "/"), bytes: fs.statSync(full).size, sha256: sha256(full) }); } };
  walk(outputDir);
  const manifest = {
    schema_version: "heardright.sotto.vast_packet.v1",
    status: "prepared_not_authorized",
    authorization: { approved: false, max_usd: 20, max_hours: 2 },
    dataset: { train_rows: countRows(path.join(outputDir, "data", "train.jsonl")), heldout_rows: countRows(path.join(outputDir, "data", "heldout-synthetic.jsonl")), manifest_sha256: sha256(SOURCE_MANIFEST), source_revision: sourceManifest.source.revision, source_file_sha256: sourceManifest.source.file_sha256, training_gate: sourceManifest.training_gate },
    model: { source_id: MODEL_SOURCE, revision: MODEL_REVISION, recipe: "runtime/model-recipes.json", training_mode: "full_parameter_only", precision: "bf16", epochs: 3, effective_batch: 16 },
    environment: { status: JSON.parse(fs.readFileSync(path.join(ROOT, "environment.lock.json"), "utf8")).status, lock_sha256: sha256(path.join(outputDir, "runtime", "environment.lock.json")) },
    files: files.sort((a, b) => a.path.localeCompare(b.path)),
  };
  fs.writeFileSync(path.join(outputDir, "packet-manifest.json"), json(manifest), "utf8");
  return { status: manifest.status, outputDir, manifest: path.join(outputDir, "packet-manifest.json"), files: files.length };
}

export { build };

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  const outputIndex = process.argv.indexOf("--output-dir");
  const outputDir = outputIndex >= 0 ? process.argv[outputIndex + 1] : DEFAULT_OUTPUT;
  try { console.log(JSON.stringify(build({ outputDir, replace: process.argv.includes("--replace") }))); }
  catch (error) { console.error(error.stack ?? error.message); process.exitCode = 1; }
}
