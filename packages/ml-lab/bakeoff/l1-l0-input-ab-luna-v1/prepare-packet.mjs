import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const sourcePath = resolve(here, "../l1-v13_2-backtrack-v1/candidate-corpus.jsonl");
const outputPath = resolve(here, "source-corpus.jsonl");

const source = await readFile(sourcePath, "utf8");
const rows = source
  .split(/\r?\n/u)
  .filter(Boolean)
  .map((line) => JSON.parse(line))
  .sort((left, right) => left.row_id.localeCompare(right.row_id));
const output = `${rows.map((row) => JSON.stringify({
  row_id: row.row_id,
  input_text: row.input_text,
  capabilities: row.capabilities ?? [],
})).join("\n")}\n`;

await writeFile(outputPath, output, "utf8");
console.log(JSON.stringify({
  row_count: rows.length,
  source_corpus_sha256: createHash("sha256").update(output).digest("hex"),
}));
