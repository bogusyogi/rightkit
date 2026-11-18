import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { writeFileAtomic } from "./atomic-write.mjs";
import { acquireFileLock } from "./ios-lock.mjs";
import { explainKeyChange } from "./deterministic-scans.mjs";

const hash = (value) => createHash("sha256").update(value).digest("hex");
const digest = /^[0-9a-f]{64}$/;

// Separate from the mutable scan cache. Removing a scan result must leave its
// prior outcome available, including a cached failure or a previously seen key.
export function openClassAHistory(options) {
  return openHistory(options, "A");
}

export function openClassBHistory(options) {
  return openHistory(options, "B");
}

function openHistory({ historyRoot, storeDir }, stage) {
  const label = "Class " + stage;
  const store = path.resolve(storeDir);
  const relative = path.relative(store, path.resolve(historyRoot));
  if (relative === "" || (!path.isAbsolute(relative) && relative !== ".." && !relative.startsWith(".." + path.sep))) throw new Error(label + " cache history must be outside " + store);
  const namespace = hash(process.platform === "win32" ? store.toLowerCase() : store);
  const file = path.join(historyRoot, namespace + ".json");
  const save = (document) => {
    const body = JSON.stringify(document);
    writeFileAtomic(file, JSON.stringify({ sha256: hash(body), body }), { mode: 0o600 });
  };
  const read = () => {
    const envelope = JSON.parse(fs.readFileSync(file, "utf8"));
    if (typeof envelope.body !== "string" || hash(envelope.body) !== envelope.sha256) throw new Error("invalid " + label + " cache history checksum: " + file);
    const body = JSON.parse(envelope.body);
    if (body.namespace !== namespace || ![1, 2].includes(body.schema)) throw new Error("invalid " + label + " cache history: " + file);
    if (body.schema === 1 && !Array.isArray(body.rows)) throw new Error("invalid Class A cache history: " + file);
    if (body.schema === 2 && (!body.stages || !Array.isArray(body.stages.A) || !Array.isArray(body.stages.B))) throw new Error("invalid " + label + " cache history: " + file);
    return body;
  };
  const rowsFor = (document) => {
    if (stage === "B" && document.schema === 1) throw new Error("Class B cache history missing at " + file + " for legacy store " + store + ". Restore verified independent Class B history or select a new store for a declared cold run.");
    return new Map(document.schema === 1 ? document.rows : document.stages[stage]);
  };
  let baseline;
  try { baseline = rowsFor(read()); } catch (error) {
    if (error.code !== "ENOENT") throw error;
    const lock = acquireFileLock(file + ".lock", label + " cache history");
    try {
      if (fs.existsSync(file)) baseline = rowsFor(read());
      else {
        fs.mkdirSync(path.dirname(store), { recursive: true });
        // Exclusive creation establishes a first-use store. An already existing
        // cache without independent evidence must never silently forecast all misses.
        try { fs.mkdirSync(store); } catch (cause) {
          if (cause.code !== "EEXIST") throw cause;
          throw new Error(label + " cache history missing at " + file + " for existing store " + store + ". Restore its verified history or select a new store for a declared cold run.");
        }
        baseline = new Map();
        // Both classes acquire their independent first-use evidence in one
        // atomic write, before either class may populate the physical store.
        save({ schema: 2, namespace, stages: { A: [], B: [] } });
      }
    } finally { lock.release(); }
  }

  function forecast(fingerprints) {
    return fingerprints.map(({ id, key, failureKey, inputs, toolVersion, environment, invocationId, problem, uncacheableReason }) => {
      const prior = baseline.get(id);
      if (problem) throw new Error(id + ": " + problem);
      if (!key && uncacheableReason) return { id, expected: "uncacheable", reason: uncacheableReason };
      if (!digest.test(key)) throw new Error(id + ": invalid result key");
      const pass = prior?.passes.includes(key);
      const fail = failureKey && prior?.failures.includes(failureKey);
      if (pass && fail) throw new Error(id + ": conflicting prior pass & failure evidence");
      if (pass || fail) return { id, key: pass ? key : failureKey, expected: "hit", status: pass ? "pass" : "fail", reason: "prior verified result for this key" };
      if (!prior) return { id, key, expected: "run", reason: "first observation of row in this store" };
      const changes = explainKeyChange(prior.last, { inputs, toolVersion, inherited: environment });
      if (stage === "B" && prior.last.invocationId !== invocationId) changes.invocation = { from: prior.last.invocationId ?? null, to: invocationId ?? null };
      if (!changes.inputsChanged && !changes.tool && !changes.inheritedEnv && !changes.invocation) throw new Error(id + ": result key changed without an identified change to its inputs, tool, environment or invocation. Cause unknown. Previous key " + prior.last.key + ", current key " + key);
      return { id, key, priorKey: prior.last.key, expected: "run", changes, reason: "no prior result for this input, tool & environment identity" };
    });
  }

  function record({ fingerprints, retained, mismatches }) {
    if (mismatches.length) throw new Error("refused " + label + " stage cannot advance cache history");
    const current = new Map(fingerprints.map((row) => [row.id, row]));
    for (const { id, key, status } of retained) {
      const row = current.get(id);
      if (!row || !["pass", "fail"].includes(status) || !digest.test(key) || key !== (status === "pass" ? row.key : row.failureKey)) throw new Error(id + ": stored evidence does not match frozen identity");
    }
    const changes = retained.filter(({ id, key, status }) => {
      const previous = baseline.get(id);
      return previous?.last.key !== key || !(status === "pass" ? previous.passes : previous.failures).includes(key);
    });
    if (!changes.length) return;
    const lock = acquireFileLock(file + ".lock", label + " cache history");
    try {
      const document = read();
      const merged = rowsFor(document);
      for (const { id, key, status } of changes) {
        const row = current.get(id);
        const entry = merged.get(id) ?? { passes: [], failures: [] };
        const keys = status === "pass" ? entry.passes : entry.failures;
        if (!keys.includes(key)) keys.push(key);
        entry.last = { key, inputs: row.inputs, toolVersion: row.toolVersion, inherited: row.environment, ...(stage === "B" ? { invocationId: row.invocationId } : {}) };
        merged.set(id, entry);
      }
      if (document.schema === 1) document.rows = [...merged];
      else document.stages[stage] = [...merged];
      save(document);
    } finally { lock.release(); }
  }
  return { file, forecast, record };
}
