// The INCOMPLETE closure on the real consumeNpm path: the source probe's report is closed against what the sealed-tarball import
// check actually imported. npm install and the import-check child are stubbed (they create the installed package and answer
// RK_CONSUME rows); everything in between — declaredEntries, expectedIncomplete, closeIncomplete, the per-unit problems — is real.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { consumeNpm } from "./consume.mjs";

const NAME = "@rk/closed"; const VERSION = "1.0.0";
const sealedPkg = { name: NAME, version: VERSION, type: "module", exports: { ".": "./dist/index.js", "./x": "./dist/x.js", "./src": "./src/s.mjs" } };
const row = (key, target) => ({ package: NAME, key, target });
const ROWS = [row(".", "./dist/index.js"), row("./x", "./dist/x.js")];

function drive({ incomplete, notImported = [] }) {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), "rk-closure-"));
  const packDir = path.join(scratchRoot, "pack"); fs.mkdirSync(packDir); fs.writeFileSync(path.join(packDir, "closed.tgz"), "not a real tarball");
  const spawn = (command, args, options = {}) => {
    if (command === "npm" && args.includes("install")) {
      const dir = path.join(options.cwd, "node_modules", ...NAME.split("/"));
      fs.mkdirSync(path.join(dir, "dist"), { recursive: true }); fs.mkdirSync(path.join(dir, "src"), { recursive: true });
      fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify(sealedPkg));
      for (const f of ["dist/index.js", "dist/x.js", "src/s.mjs"]) fs.writeFileSync(path.join(dir, f), "export {};\n");
      return { status: 0, stdout: "", stderr: "" };
    }
    if (command === process.execPath) {
      const plan = JSON.parse(fs.readFileSync(args.at(-1), "utf8"));
      const rows = plan.items.map((item) => ({ spec: item.spec, ok: true, imported: item.import && !notImported.includes(item.spec), resolved: "file:///x" }));
      return { status: 0, stdout: `RK_CONSUME ${JSON.stringify(rows)}\n`, stderr: "" };
    }
    throw new Error(`unexpected child: ${command} ${args.join(" ")}`);
  };
  try {
    const results = consumeNpm({ units: [{ name: NAME, version: VERSION, file: "closed.tgz", sha256: "0".repeat(64), size: 1 }], packDir, scratchRoot, npm: { command: "npm", prefix: [], shell: false }, spawn, incomplete: incomplete === null ? null : { schema: "rightkit.import-effects/1", incomplete, covered: [] }, npmConsumer: () => { throw new Error("not used"); } });
    const r = results.get(NAME);
    // The recipe inventory has no reviewed example for this fixture package; that real finding is orthogonal to the closure.
    return { ...r, problems: r.problems.filter((p) => !/missing reviewed PK example/.test(p)) };
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
}

test("consumeNpm closes every INCOMPLETE row the sealed tarball imported and records the count", () => {
  const r = drive({ incomplete: ROWS });
  assert.deepEqual(r.problems, []);
  assert.equal(r.report.incompleteClosed, 2);
  assert.deepEqual(r.report.imported.sort(), [NAME, `${NAME}/src`, `${NAME}/x`].sort());
});

test("consumeNpm refuses a unit whose INCOMPLETE export the tarball import check did not import", () => {
  const r = drive({ incomplete: ROWS, notImported: [`${NAME}/x`] });
  assert.equal(r.problems.length, 1);
  assert.match(r.problems[0], /INCOMPLETE not closed: @rk\/closed\/x \(\.\/dist\/x\.js\)/);
  assert.equal(r.report.incompleteClosed, 1);
});

test("consumeNpm refuses a forged COMPLETE report: the sealed package exports build output the report never accounted for", () => {
  const r = drive({ incomplete: [] });
  assert.equal(r.problems.length, 2);
  assert.ok(r.problems.every((p) => /accounts for neither an INCOMPLETE nor a covered/.test(p)), JSON.stringify(r.problems));
});

test("consumeNpm refuses duplicate, mis-targeted and undeclared rows (exact set)", () => {
  assert.match(drive({ incomplete: [...ROWS, ROWS[0]] }).problems[0], /lists \. twice/);
  assert.match(drive({ incomplete: [row(".", "./dist/other.js"), ROWS[1]] }).problems[0], /binds \. to \.\/dist\/other\.js, the sealed package to \.\/dist\/index\.js/);
  assert.match(drive({ incomplete: [...ROWS, row("./ghost", "./dist/ghost.js")] }).problems[0], /names \.\/ghost .* which the sealed package does not export from build output/);
});

test("consumeNpm without a report performs no closure (publish.mjs verify is what requires the report)", () => {
  const r = drive({ incomplete: null });
  assert.deepEqual(r.problems, []);
  assert.equal(r.report.incompleteClosed, undefined);
});
