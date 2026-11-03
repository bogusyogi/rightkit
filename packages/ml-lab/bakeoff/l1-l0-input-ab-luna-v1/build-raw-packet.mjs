import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const manifestPath = resolve(here, "candidate-manifest.json");
const packetPath = resolve(repo, "docs/results/2026-08-24-l1-l0-input-ab-luna-raw-packet.md");
const resultsManifestPath = resolve(here, "raw-results-manifest.json");
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const bytes = (name) => readFile(resolve(here, name));
const jsonl = async (name) => (await bytes(name)).toString("utf8").trim().split(/\r?\n/u).map(JSON.parse);

const manifestBytes = await readFile(manifestPath);
const manifest = JSON.parse(manifestBytes);
const source = await jsonl("source-corpus.jsonl");
const l0 = await jsonl("with-l0-corpus.jsonl");
const lanes = [
  ["prompt1_no_l0", "luna-prompt1-no-l0-raw.jsonl"],
  ["prompt1_with_l0", "luna-prompt1-with-l0-raw.jsonl"],
  ["prompt2_no_l0", "luna-prompt2-no-l0-raw.jsonl"],
  ["prompt2_with_l0", "luna-prompt2-with-l0-raw.jsonl"],
];
const laneRows = new Map();
const outputFiles = {};

if (source.length !== 52 || l0.length !== 52) throw new Error("expected 52 rows in each input corpus");
for (let index = 0; index < source.length; index += 1) {
  if (source[index].row_id !== l0[index].row_id) throw new Error(`L0 row mismatch at ${index}`);
  if (source[index].input_text !== l0[index].source_text) throw new Error(`L0 source mismatch for ${source[index].row_id}`);
}

for (const [lane, file] of lanes) {
  const fileBytes = await bytes(file);
  const rows = fileBytes.toString("utf8").trim().split(/\r?\n/u).map(JSON.parse);
  if (rows.length !== source.length) throw new Error(`${lane}: expected ${source.length} rows, got ${rows.length}`);
  const seen = new Set();
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index];
    if (row.row_id !== source[index].row_id) throw new Error(`${lane}: row order mismatch at ${index}`);
    if (row.lane !== lane) throw new Error(`${lane}: wrong lane metadata for ${row.row_id}`);
    if (row.model_identity !== "gpt-5.6-luna" || row.model_tier !== "high") throw new Error(`${lane}: wrong model metadata`);
    if (typeof row.raw_output !== "string" || !row.raw_output.trim()) throw new Error(`${lane}: empty output for ${row.row_id}`);
    if (seen.has(row.row_id)) throw new Error(`${lane}: duplicate ${row.row_id}`);
    seen.add(row.row_id);
  }
  laneRows.set(lane, rows);
  outputFiles[lane] = { file, row_count: rows.length, sha256: sha256(fileBytes) };
}

const block = (value) => {
  const longest = Math.max(0, ...[...value.matchAll(/`+/gu)].map((match) => match[0].length));
  const fence = "`".repeat(Math.max(3, longest + 1));
  return `${fence}text\n${value}\n${fence}`;
};

const lines = [
  "# Luna 2×2 raw packet: prompt structure × L0 input",
  "",
  "Raw outputs only. No scoring, repair, fallback, adjudication, or winner.",
  "",
  "## Four result lanes",
  "",
  "| Lane | Prompt | Input |",
  "|---|---|---|",
  "| `prompt1_no_l0` | Prompt 1: full cleanup | Original frozen source; no L0 |",
  "| `prompt1_with_l0` | Prompt 1: full cleanup | Production deterministic L0 output |",
  "| `prompt2_no_l0` | Prompt 2: already-cleaned | Original frozen source; no L0 |",
  "| `prompt2_with_l0` | Prompt 2: already-cleaned | Production deterministic L0 output |",
  "",
  "Both prompts ran on both input conditions. Each lane uses `gpt-5.6-luna` at `high` tier across all 52 rows.",
  "",
  "## Frozen artifacts",
  "",
  `- Candidate manifest SHA-256: \`${sha256(manifestBytes)}\``,
  `- Original source SHA-256: \`${manifest.corpus.source_sha256}\``,
  `- Production L0 corpus SHA-256: \`${manifest.corpus.with_l0_sha256}\``,
  ...lanes.map(([lane]) => `- ${lane} SHA-256: \`${outputFiles[lane].sha256}\``),
  "",
  "## Prompt 1: full cleanup",
  "",
  block(manifest.prompts.prompt_1_full_cleanup.system_prompt),
  "",
  "## Prompt 2: already-cleaned",
  "",
  block(manifest.prompts.prompt_2_already_cleaned.system_prompt),
  "",
  "## Raw rows",
  "",
];

for (let index = 0; index < source.length; index += 1) {
  const row = source[index];
  lines.push(`### ${String(index + 1).padStart(2, "0")} — \`${row.row_id}\``, "");
  lines.push(`Capabilities: ${row.capabilities.length ? row.capabilities.map((value) => `\`${value}\``).join(", ") : "none"}`, "");
  lines.push("Original source — NO_L0 input", "", block(row.input_text), "");
  lines.push("Production deterministic L0 — WITH_L0 input", "", block(l0[index].input_text), "");
  for (const [lane] of lanes) {
    lines.push(lane, "", block(laneRows.get(lane)[index].raw_output), "");
  }
}

const packet = `${lines.join("\n")}\n`;
await writeFile(packetPath, packet, "utf8");
const resultsManifest = {
  schema: "heardright.l1_l0_input_ab_luna_raw_results.v1",
  candidate_manifest_file: "candidate-manifest.json",
  candidate_manifest_sha256: sha256(manifestBytes),
  row_count_per_lane: source.length,
  model: manifest.model,
  lanes: outputFiles,
  packet_file: "../../../docs/results/2026-08-24-l1-l0-input-ab-luna-raw-packet.md",
  packet_sha256: sha256(Buffer.from(packet)),
  scoring: "none",
};
await writeFile(resultsManifestPath, `${JSON.stringify(resultsManifest, null, 2)}\n`, "utf8");
console.log(JSON.stringify({ packet: packetPath, packet_sha256: resultsManifest.packet_sha256, lanes: outputFiles }));
