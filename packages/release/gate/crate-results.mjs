// Per-crate results of ONE together Cargo call, read from Cargo's own JSON message stream (design section 8,
// docs/design/2026-11-10-gate-fresh-design.md). Pure: text in, verdicts out. No cache, no grouping, no projection onto other keys:
// a crate's result here is how it compiled in the call that named it, nothing more.
//
// Cargo's stream (`--message-format=json`), one JSON object per stdout line:
//   compiler-message   { package_id, target, message: { level, rendered } }   no target triple in the record
//   compiler-artifact  { package_id, target: { kind, name }, filenames: [...], fresh }
//   build-script-executed, build-finished { success }
// Facts this file relies on and nothing else:
//   - an artifact's file path holds `<target-dir>/<triple>/<profile>/...` when the call named `--target <triple>`;
//   - a build-script failure and a link failure are plain stderr text: "failed to run custom build command for `name vX`",
//     "could not compile `name` (...)";
//   - `build-finished` is the last record of a call that reached the build; a stream without it was cut short or never started.

/** The crate name inside a Cargo package id, for every spelling Cargo has used. */
export function packageNameOf(id) {
  const text = String(id ?? "");
  // Cargo >= 1.77: `path+file:///abs/crates/foo#0.1.0`, `path+file:///abs/dir#name@0.1.0`, `registry+https://...#serde@1.0.200`.
  const hash = text.lastIndexOf("#");
  if (/^[a-z]+\+/.test(text) && hash >= 0) {
    const tail = text.slice(hash + 1);
    const at = tail.lastIndexOf("@");
    if (at > 0) return tail.slice(0, at);
    const base = text.slice(0, hash).replace(/[?].*$/, "").replace(/\/+$/, "");
    return base.slice(base.lastIndexOf("/") + 1);
  }
  // Older: `name 0.1.0 (path+file:///...)`.
  const space = text.indexOf(" ");
  return space > 0 ? text.slice(0, space) : text;
}

/** The target triple an artifact was built for: the first path segment that is one of `targets`; `host` when the path names none. */
export function tripleOfArtifact(filenames, targets, host) {
  for (const file of Array.isArray(filenames) ? filenames : []) {
    const parts = String(file).split(/[\\/]+/);
    for (const t of targets) if (parts.includes(t)) return t;
  }
  return host;
}

const STDERR_FAILURES = [
  /failed to run custom build command for `([^`\s]+)(?: v[^`]*)?`/g,
  /could not compile `([^`\s]+)`/g,
  /could not document `([^`\s]+)`/g,
];

/**
 * Reads one call's output.
 * `stdout`: the call's stdout (Cargo's JSON lines, other lines ignored). `stderr`: its stderr.
 * Returns { finished, success, records, errors: { crate: { count, rendered[] } }, artifacts: { crate: { triple: n } }, named: Set<crate> }.
 */
export function readCargoStream({ stdout = "", stderr = "", targets = [], host = null } = {}) {
  const out = { finished: false, success: null, records: 0, malformed: 0, errors: {}, artifacts: {}, named: new Set() };
  const fail = (name, rendered) => {
    const e = (out.errors[name] ??= { count: 0, rendered: [] });
    e.count += 1;
    if (rendered && e.rendered.length < 4) e.rendered.push(String(rendered).trimEnd().slice(0, 700));
  };
  for (const raw of String(stdout).split(/\r?\n/)) {
    const line = raw.trim();
    if (!line.startsWith("{")) continue;
    let v;
    try { v = JSON.parse(line); } catch { out.malformed += 1; continue; }
    if (!v || typeof v.reason !== "string") continue;
    out.records += 1;
    if (v.reason === "build-finished") { out.finished = true; out.success = v.success === true; continue; }
    if (v.reason === "compiler-message") {
      if (v.message?.level === "error") fail(packageNameOf(v.package_id), v.message.rendered ?? v.message.message);
      continue;
    }
    if (v.reason === "compiler-artifact") {
      const kinds = Array.isArray(v.target?.kind) ? v.target.kind : [];
      if (kinds.includes("custom-build")) continue;
      const name = packageNameOf(v.package_id);
      const triple = tripleOfArtifact(v.filenames, targets, host);
      const per = (out.artifacts[name] ??= {});
      per[triple] = (per[triple] ?? 0) + 1;
    }
  }
  for (const pattern of STDERR_FAILURES) {
    for (const m of String(stderr).matchAll(pattern)) { out.named.add(m[1]); if (!out.errors[m[1]]) fail(m[1], m[0]); }
  }
  return out;
}

/**
 * The result of every crate the call named.
 * `crates`: [{ name, targets: [triple...] }] (the triples this call asked of that crate; one entry, the host, for a call with no cross target).
 * `exit`: the call's exit code (null when it never ran). `refusal`: a one-line cause when the call did not reach Cargo (broker refusal, no result line).
 * Each result: { crate, result: "passed" | "failed" | "not-built" | "unknown", detail }.
 *   passed     no error for the crate and an artifact for every triple asked of it
 *   failed     an error of the crate's own (compiler, build script, link)
 *   not-built  no error of its own and an artifact missing: something it depends on failed, or Cargo stopped before it
 *   unknown    the call gave no usable stream (refused, cut short, no JSON): nothing is known, nothing passes
 */
export function crateResults({ crates, stdout, stderr, exit = null, refusal = null, host = null }) {
  const targets = [...new Set(crates.flatMap((c) => c.targets))];
  if (refusal) return crates.map((c) => ({ crate: c.name, result: "unknown", detail: refusal }));
  const s = readCargoStream({ stdout, stderr, targets, host });
  if (!s.finished) {
    const why = s.records ? "Cargo's output ended without a build-finished record (cut short)" : "the call produced no Cargo JSON";
    // A crate Cargo named as failed before the stream ended still failed; the rest are unknown.
    return crates.map((c) => (s.errors[c.name] ? { crate: c.name, result: "failed", detail: s.errors[c.name].rendered.join(" | ") } : { crate: c.name, result: "unknown", detail: why }));
  }
  const failedNames = Object.keys(s.errors);
  return crates.map((c) => {
    const err = s.errors[c.name];
    if (err) return { crate: c.name, result: "failed", detail: `${err.count} error(s): ${err.rendered.join(" | ")}` };
    const missing = c.targets.filter((t) => !(s.artifacts[c.name]?.[t] > 0));
    if (missing.length === 0) {
      // Every artifact is there. A non-zero exit with this crate complete is someone else's failure.
      return { crate: c.name, result: "passed", detail: "" };
    }
    const because = failedNames.length ? `failed in this call: ${failedNames.slice(0, 6).join(", ")}${failedNames.length > 6 ? ", ..." : ""}` : s.success === false || (exit !== null && exit !== 0) ? "the call failed with no error attributed to a crate (see its output)" : "Cargo reported success but no artifact for it";
    return { crate: c.name, result: "not-built", detail: `no artifact for ${missing.join(", ")}; ${because}` };
  });
}

/** One crate's result over every call that named it: passed only when it passed them all; the first non-pass decides the word. */
export function combineResults(perCall) {
  const order = ["failed", "not-built", "unknown"];
  for (const word of order) {
    const hit = perCall.find((r) => r.result === word);
    if (hit) return { result: word, detail: hit.detail };
  }
  return perCall.length ? { result: "passed", detail: "" } : { result: "unknown", detail: "no call named this crate" };
}
