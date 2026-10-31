// The declared compile inputs a RustRight broker captured for a compile (RIGHTKIT_COMPILE_INPUTS): the gate judges exactly
// those bytes, where the broker copied them, so what is checked is what the compile reads. The tree's tracked files are
// judged from the commit as always; the declared inputs are the ignored paths (a built `dist`, generated assets) the
// commit does not hold, listed in the captured tree's `.rightkit/compile-inputs.json`.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { SECRET_PATTERNS } from "./publish-cargo.mjs";
import { COMPILE_INPUTS_CANONICAL, COMPILE_INPUTS_LEGACY, SnapshotRequestError, readDeclaredInputs } from "./snapshot-request.mjs";

/** The canonical declaration (GB-49); the legacy `.rightkit/compile-inputs.json` is read only when it is absent (snapshot-request.mjs readDeclaredInputs, the one reader). */
export const COMPILE_INPUTS_FILE = COMPILE_INPUTS_CANONICAL;
export { COMPILE_INPUTS_LEGACY };

/** Files of one declared input in the broker's order (children by name), refusing links; [{ name, abs }]. */
function inputFiles(root, input) {
  const out = [];
  const walk = (abs, name) => {
    const st = fs.lstatSync(abs);
    if (st.isSymbolicLink()) throw new Error(`declared compile input holds a symbolic link at ${name}`);
    if (st.isDirectory()) {
      for (const child of fs.readdirSync(abs).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) walk(path.join(abs, child), `${name}/${child}`);
    } else if (st.isFile()) out.push({ name, abs });
  };
  walk(path.join(root, input), input);
  return out;
}

/**
 * Judges the captured declared inputs under `root`: each declared path must exist without links; every `.js/.mjs/.cjs`
 * file parses (`node --check`), every `.json` file parses, and no file carries a secret. Returns { inputs: [{ path, sha256,
 * files }], findings: [string] }; `sha256` is computed as the broker records it (name, NUL, bytes, NUL per file, in order),
 * so the receipt and the gate name the same bytes.
 */
export function judgeCompileInputs(root, { spawn = spawnSync, execPath = process.execPath } = {}) {
  const findings = [];
  // One reader for every consumer: canonical first, legacy only when the canonical file is absent, shared link, size, path and overlap checks.
  let declared;
  try { declared = readDeclaredInputs(root); } catch (e) { if (e instanceof SnapshotRequestError) return { inputs: [], findings: [e.message] }; throw e; }
  if (declared.length === 0) return { inputs: [], findings };
  const inputs = [];
  for (const input of declared) {
    let files;
    try { files = inputFiles(root, input.split(/[\\/]/).join("/")); } catch (e) { findings.push(`${input}: ${e.code === "ENOENT" ? "missing from the captured tree" : e.message}`); continue; }
    const hash = crypto.createHash("sha256");
    for (const f of files) {
      const bytes = fs.readFileSync(f.abs);
      hash.update(f.name); hash.update(Buffer.from([0])); hash.update(bytes); hash.update(Buffer.from([0]));
      const text = bytes.toString("utf8");
      if (SECRET_PATTERNS.some((pattern) => pattern.test(text))) findings.push(`${f.name}: holds a secret (token or private key pattern)`);
      if (/\.(m|c)?js$/.test(f.name)) {
        const r = spawn(execPath, ["--check", f.abs], { encoding: "utf8" });
        if (r.status !== 0) findings.push(`${f.name}: does not parse (${String(r.stderr ?? "").split("\n").find((l) => /Error/.test(l))?.trim() ?? `node --check exit ${r.status}`})`);
      }
      if (/\.json$/.test(f.name)) {
        try { JSON.parse(text); } catch (e) { findings.push(`${f.name}: invalid JSON (${e.message})`); }
      }
    }
    inputs.push({ path: input, sha256: hash.digest("hex"), files: files.length });
  }
  return { inputs, findings };
}
