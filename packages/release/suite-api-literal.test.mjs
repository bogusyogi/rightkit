import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { suiteApiLiteralDetail, suiteApiLiteralFindings } from "./suite-api-literal.mjs";
import { unmanagedEnv } from "./test-support/unmanaged-env.mjs";

const ENDPOINT = (app) => `https://api.orthiclabs.com/v1/apps/${app}/releases/latest.json?platform={{target}}`;
const confWith = (endpoints) => JSON.stringify({ plugins: { updater: { pubkey: "k", endpoints } } }, null, 2);

// A temp git repo whose tracked files are `files`; `untracked` files exist on disk but are never added.
function appRepo(files, untracked = {}) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "suite-api-literal-"));
  const write = (rel, body) => {
    mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    writeFileSync(path.join(dir, rel), body);
  };
  for (const [rel, body] of Object.entries(files)) write(rel, body);
  const git = (...args) => spawnSync("git", args, { cwd: dir, encoding: "utf8", env: unmanagedEnv(process.env) });
  assert.equal(git("init", "-q").status, 0);
  assert.equal(git("add", "-A").status, 0);
  for (const [rel, body] of Object.entries(untracked)) write(rel, body);
  return { dir, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}

function scan(files, appKey, untracked) {
  const repo = appRepo(files, untracked);
  try { return suiteApiLiteralFindings({ root: repo.dir, appKey }); } finally { repo.cleanup(); }
}

test("a contract host literal in src is refused with file, line and host", () => {
  const findings = scan({ "src/config.ts": "const a = 1;\nexport const host = \"https://api.orthiclabs.com/v1\";\n" }, "viewright");
  assert.deepEqual(findings.map(({ file, line, host, kind }) => ({ file, line, host, kind })), [
    { file: "src/config.ts", line: 2, host: "api.orthiclabs.com", kind: "host-literal" },
  ]);
  const detail = suiteApiLiteralDetail("viewright", findings);
  assert.match(detail, /src\/config\.ts:2: api\.orthiclabs\.com/u);
});

test("legacy and artifact contract hosts are refused like the API host", () => {
  const findings = scan({
    "src/a.ts": "fetch('https://api.spoares.com/x')\n",
    "src/b.ts": "const u = 'https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev/k';\n",
    "src/c.ts": "const g = 'https://rightapps-license-gate.adrdsouza.workers.dev/k';\n",
  }, "mailright");
  assert.deepEqual(findings.map((f) => f.host).sort(), [
    "api.spoares.com", "pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev", "rightapps-license-gate.adrdsouza.workers.dev",
  ].sort());
});

test("a tauri conf updater endpoints entry is refused for a contract app", () => {
  const findings = scan({ "src-tauri/tauri.conf.json": `{\n  "plugins": {\n    "updater": {\n      "endpoints": ["${ENDPOINT("viewright")}"]\n    }\n  }\n}\n` }, "viewright");
  const endpointFinding = findings.find((f) => f.kind === "updater-endpoints");
  assert.ok(endpointFinding, "endpoints must be refused");
  assert.equal(endpointFinding.file, "src-tauri/tauri.conf.json");
  assert.equal(endpointFinding.line, 4);
  assert.ok(findings.some((f) => f.kind === "host-literal" && f.line === 4), "the endpoint host literal is refused too");
});

test("a capability JSON scope on a contract host is refused", () => {
  const findings = scan({
    "src-tauri/capabilities/default.json": JSON.stringify({ permissions: [{ identifier: "http:default", allow: [{ url: "https://api.orthiclabs.com/*" }] }] }, null, 2),
  }, "coderight");
  assert.deepEqual(findings.map((f) => [f.file, f.kind, f.host]), [["src-tauri/capabilities/default.json", "capability-scope", "api.orthiclabs.com"]]);
});

test("Markdown and docs/** are ignored", () => {
  assert.deepEqual(scan({
    "README.md": "Set RIGHTAPPS_API_URL to https://api.orthiclabs.com\n",
    "docs/runbook.txt": "https://api.spoares.com\n",
    "docs/guide/x.json": "{\"u\":\"https://api.orthiclabs.com\"}\n",
    "src/ok.ts": "export const x = 1;\n",
  }, "viewright"), []);
});

test("third-party API hosts are allowed", () => {
  assert.deepEqual(scan({ "src/translate.rs": "const URL: &str = \"https://api.deepl.com/v2/translate\";\n" }, "mailright"), []);
});

test("a look-alike host that merely contains a contract name is allowed", () => {
  assert.deepEqual(scan({ "src/x.ts": "const h = 'notapi.orthiclabs.com';\n" }, "viewright"), []);
});

test("heardright may keep exactly the pending contract updater endpoint", () => {
  assert.deepEqual(scan({ "src-tauri/tauri.conf.json": confWith([ENDPOINT("heardright")]) }, "heardright"), []);
});

test("heardright with a different updater endpoint is refused", () => {
  const findings = scan({ "src-tauri/tauri.conf.json": confWith(["https://api.orthiclabs.com/v1/apps/heardright/releases/latest.json"]) }, "heardright");
  assert.ok(findings.some((f) => f.kind === "updater-endpoints" && f.file === "src-tauri/tauri.conf.json"));
  assert.ok(findings.some((f) => f.kind === "host-literal"));
});

test("heardright's exempt endpoint does not excuse another literal in its source", () => {
  const findings = scan({
    "src-tauri/tauri.conf.json": confWith([ENDPOINT("heardright")]),
    "src/autoUpdates.ts": "const status = 'https://api.orthiclabs.com/v1/apps/heardright/releases/status.json';\n",
  }, "heardright");
  assert.deepEqual(findings.map((f) => [f.file, f.kind]), [["src/autoUpdates.ts", "host-literal"]]);
});

test("a pending endpoint in a non-pending app is refused", () => {
  const findings = scan({ "src-tauri/tauri.conf.json": confWith([ENDPOINT("viewright")]) }, "viewright");
  assert.ok(findings.some((f) => f.kind === "updater-endpoints"));
});

test("apps outside the contract keep only the host and capability checks", () => {
  assert.deepEqual(scan({ "src-tauri/tauri.conf.json": confWith(["https://scraperight.cc/v1/apps/scraperight/releases/latest.json?platform={{target}}"]) }, "scraperight"), []);
  const findings = scan({ "src-tauri/capabilities/default.json": "{\"allow\":[{\"url\":\"https://api.spoares.com/*\"}]}\n" }, "scraperight");
  assert.deepEqual(findings.map((f) => f.kind), ["capability-scope"]);
});

test("untracked files are not scanned", () => {
  assert.deepEqual(scan({ "src/ok.ts": "export {};\n" }, "viewright", { "src/scratch.ts": "https://api.orthiclabs.com\n" }), []);
});
