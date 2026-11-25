import assert from "node:assert/strict";
import test from "node:test";
import { callerListProblems, judgeCallers, PASS, FAIL } from "./acceptance-callers.mjs";

const SHA = "9da554ea136a70b7a82fb466ca7e00e0fd85ce37";
const defs = {
  callers: [
    { id: "ci", file: "ci.yml", kind: "exit0", marker: "rightkit-gate check --repo .", args: ["check", "--commit", "{sha}"] },
    { id: "admit", file: "admit.mjs", kind: "admission", marker: "--full-a", verdictExport: "gateVerdict", args: ["check", "--full-a"], refuseArgs: ["check"] },
  ],
  namesOnly: { "notes.md": "a note" },
};
const files = { "ci.yml": "run: rightkit-gate check --repo . --commit x", "admit.mjs": "const c = `check --full-a`;" };
const list = (over = {}) => callerListProblems(defs, "/repo", { files: Object.keys({ ...files, "notes.md": 1, ...over.extra }), read: (f) => { const t = (over.files ?? files)[f]; if (t === undefined) throw new Error("gone"); return t; } });
// The admission verdict as publish-admission applies it: exit 0 and a full pass line, never PARTIAL.
const gateVerdict = (r) => { if (r.status !== 0 || !/class A (scoped|full) pass/.test(r.stdout) || /PARTIAL/.test(r.stdout)) throw new Error("refused"); };
const gate = (answers) => (c, args) => ({ sha: SHA, args, ...(answers[args.join(" ")] ?? { status: 0, stdout: "" }), stderr: "", error: null });
const full = { status: 0, stdout: "rightkit-gate: class A full pass: 9 deterministic check(s)" };
const partial0 = { status: 0, stdout: "rightkit-gate: class A content pass: 9\nrightkit-gate: PARTIAL: 3 executing check(s) did not run" };

test("NN13 passes when every caller gets the answer its rule needs", async () => {
  const r = await judgeCallers(defs, "/repo", { run: gate({ "check --full-a": full, check: partial0 }), verdictOf: async () => gateVerdict, listProblems: list() });
  assert.equal(r.verdict, PASS, r.why);
});

test("NN13 fails on rightkit#1695: a plain check that exits 3 breaks an exit-0 caller", async () => {
  const r = await judgeCallers(defs, "/repo", { run: gate({ "check --commit {sha}": { status: 3, stdout: "PARTIAL" }, "check --full-a": full, check: partial0 }), verdictOf: async () => gateVerdict, listProblems: [] });
  assert.equal(r.verdict, FAIL); assert.match(r.why, /caller ci .* needs exit 0 .* exited 3/);
});

test("NN13 fails when an admission caller admits the content-only run", async () => {
  const lax = (r) => { if (r.status !== 0) throw new Error("refused"); }; // reads the exit alone
  const r = await judgeCallers(defs, "/repo", { run: gate({ "check --full-a": full, check: partial0 }), verdictOf: async () => lax, listProblems: [] });
  assert.equal(r.verdict, FAIL); assert.match(r.why, /admitted the content-only run/);
});

test("NN13 fails when an admission caller refuses the full run of a clean commit", async () => {
  const r = await judgeCallers(defs, "/repo", { run: gate({ "check --full-a": { status: 0, stdout: "class A content pass" }, check: partial0 }), verdictOf: async () => gateVerdict, listProblems: [] });
  assert.equal(r.verdict, FAIL); assert.match(r.why, /refused the full gate run/);
});

test("NN13 discovery: a file naming the gate that is in neither list fails; a caller whose call changed fails", () => {
  assert.deepEqual(list(), []);
  assert.match(list({ extra: { "new-workflow.yml": 1 } }).join(), /new-workflow\.yml names rightkit-gate and is in neither callers nor namesOnly/);
  assert.match(list({ files: { ...files, "ci.yml": "run: rightkit-gate --to compile" } }).join(), /caller ci: ci\.yml no longer contains its recorded call/);
  assert.match(list({ files: { "admit.mjs": files["admit.mjs"] } }).join(), /caller ci: ci\.yml is gone/);
});

test("the real cases file lists every file of RightKit that names the gate, and each caller's call is still there", async () => {
  const fs = await import("node:fs"); const path = await import("node:path"); const { fileURLToPath } = await import("node:url");
  const here = path.dirname(fileURLToPath(import.meta.url));
  const cases = JSON.parse(fs.readFileSync(path.join(here, "acceptance-cases.json"), "utf8"));
  assert.deepEqual(callerListProblems(cases, path.resolve(here, "..", "..")), []);
  for (const c of cases.callers.filter((x) => x.kind === "admission")) assert.ok(c.args.includes("--full-a") && !c.refuseArgs.includes("--full-a"), c.id);
});
