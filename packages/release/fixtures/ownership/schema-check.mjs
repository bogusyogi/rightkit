// Minimal JSON Schema subset validator (no external dependency) for the ownership fixtures.
// Supports: $ref (local and sibling-file), type, enum, const, properties, required, additionalProperties:false,
// items, minItems, minLength, minimum, maximum, pattern, oneOf, anyOf, allOf, if/then. Unknown keywords are ignored.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const dir = path.dirname(fileURLToPath(import.meta.url));
const docs = new Map();
const load = (name) => { if (!docs.has(name)) docs.set(name, JSON.parse(readFileSync(path.join(dir, name), "utf8"))); return docs.get(name); };
const kind = (v) => v === null ? "null" : Array.isArray(v) ? "array" : Number.isInteger(v) ? "integer" : typeof v;

function resolve(ref, current) {
  const [file, pointer = ""] = ref.split("#");
  const doc = file ? load(file) : load(current);
  let node = doc;
  for (const part of pointer.split("/").filter(Boolean)) node = node[part];
  return { node, doc: file || current };
}

export function validate(schema, value, current = "report.schema.json", at = "$") {
  const errors = [];
  const fail = (message) => errors.push(`${at}: ${message}`);
  if (schema.$ref) {
    const { node, doc } = resolve(schema.$ref, current);
    errors.push(...validate(node, value, doc, at));
  }
  if (schema.type !== undefined) {
    const types = [].concat(schema.type), actual = kind(value);
    if (!types.some((t) => t === actual || (t === "number" && actual === "integer"))) { fail(`expected ${types.join("|")}, got ${actual}`); return errors; }
  }
  if (schema.enum && !schema.enum.some((x) => JSON.stringify(x) === JSON.stringify(value))) fail(`not in enum ${JSON.stringify(schema.enum)}`);
  if ("const" in schema && JSON.stringify(schema.const) !== JSON.stringify(value)) fail(`expected const ${JSON.stringify(schema.const)}`);
  if (typeof value === "string") {
    if (schema.minLength !== undefined && value.length < schema.minLength) fail("string too short");
    if (schema.pattern && !new RegExp(schema.pattern).test(value)) fail(`does not match ${schema.pattern}`);
  }
  if (typeof value === "number") {
    if (schema.minimum !== undefined && value < schema.minimum) fail("below minimum");
    if (schema.maximum !== undefined && value > schema.maximum) fail("above maximum");
  }
  if (Array.isArray(value)) {
    if (schema.minItems !== undefined && value.length < schema.minItems) fail("too few items");
    if (schema.items) value.forEach((item, i) => errors.push(...validate(schema.items, item, current, `${at}[${i}]`)));
  }
  if (value && typeof value === "object" && !Array.isArray(value)) {
    for (const key of schema.required ?? []) if (!Object.hasOwn(value, key)) fail(`missing required ${key}`);
    for (const [key, sub] of Object.entries(schema.properties ?? {})) if (Object.hasOwn(value, key)) errors.push(...validate(sub, value[key], current, `${at}.${key}`));
    if (schema.additionalProperties === false) for (const key of Object.keys(value)) if (!Object.hasOwn(schema.properties ?? {}, key)) fail(`unknown property ${key}`);
  }
  const branches = (list) => list.map((sub) => validate(sub, value, current, at));
  if (schema.oneOf) { const ok = branches(schema.oneOf).filter((e) => !e.length).length; if (ok !== 1) fail(`matched ${ok} of ${schema.oneOf.length} oneOf branches`); }
  if (schema.anyOf && !branches(schema.anyOf).some((e) => !e.length)) fail("matched no anyOf branch");
  if (schema.allOf) for (const e of branches(schema.allOf)) errors.push(...e);
  if (schema.if && !validate(schema.if, value, current, at).length && schema.then) errors.push(...validate(schema.then, value, current, at));
  return errors;
}

/** Validate against a named definition, e.g. validateDef("report.schema.json", "workerOutput", value). */
export function validateDef(file, def, value) { return validate({ $ref: `${file}#/$defs/${def}` }, value, file); }
export function validateTop(file, value) { const doc = load(file); return validate(doc, value, file); }
