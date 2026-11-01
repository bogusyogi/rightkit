import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import { validateOutput } from "./validate_output.mjs";

const base = { input_text: "Please send invoice SX-7109 to Amina tomorrow morning.", output_text: "Please send invoice SX-7109 to Amina tomorrow morning.", structured_spans: { protected: [{ text: "SX-7109" }, { text: "Amina" }] }, bounded_context: {} };
test("accepts a complete conservative repair", () => assert.deepEqual(validateOutput(base), { ok: true, errors: [] }));
test("rejects wrappers, forbidden dashes, and protected-span loss", () => {
  const result = validateOutput({ ...base, output_text: "Output: Please send invoice SX-7110 to Amina - tomorrow - morning - now - immediately - again - again - again - again - again - again.".replaceAll(" - ", " — ") });
  assert.equal(result.ok, false);
  assert(result.errors.some((error) => error.startsWith("wrapper")));
  assert(result.errors.some((error) => error.startsWith("punctuation")));
  assert(result.errors.some((error) => error.startsWith("protected")));
});
test("rejects copied reference context", () => {
  const result = validateOutput({ ...base, output_text: `${base.output_text} Closed afternoon carrier slot at west depot.`, bounded_context: { document_selection_excerpt: "Closed afternoon carrier slot at west depot." } });
  assert(result.errors.some((error) => error.startsWith("invention")));
});
test("dynamic import under recovered runner node -e emits exactly one JSON object", () => {
  const validator = pathToFileURL(fileURLToPath(new URL("./validate_output.mjs", import.meta.url))).href;
  const program = "const m=await import(process.argv[1]);let s='';process.stdin.setEncoding('utf8');for await(const c of process.stdin)s+=c;console.log(JSON.stringify(m.validateOutput(JSON.parse(s))))";
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", program, validator], { input: JSON.stringify(base), encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const lines = result.stdout.trim().split(/\r?\n/u);
  assert.equal(lines.length, 1);
  assert.deepEqual(JSON.parse(lines[0]), { ok: true, errors: [] });
});
