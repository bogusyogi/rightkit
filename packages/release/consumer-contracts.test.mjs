// Consumer contracts (CA-01, CA-05, CA-06, CA-14, CA-15): each rule against the original failing source (an excerpt of the committed
// blob named beside it), its repair, and positive controls that keep legitimate shapes green. Rules run through app-check's runRules,
// the function `rightkit-gate check` and the right-release guard call, with a pinned clock.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { RULES, runRules } from "./app-check.mjs";
import { CONSUMER_CONTRACT_RULES, compareVersions, contractData, docCommentBareCrs, excluded, selectionBelow, workflowJobs } from "./consumer-contracts.mjs";
import { planScans, runScans } from "./deterministic-scans.mjs";

/** Run rule `id` over a tree written from `tree` ({ path: string | Buffer }) at clock `now`; returns the problems. */
function check(id, tree, { now = new Date("2026-10-26T00:00:00Z") } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-contracts-"));
  try {
    for (const [file, body] of Object.entries(tree)) {
      fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
      fs.writeFileSync(path.join(root, file), body);
    }
    const { failures } = runRules({ root, commit: "0".repeat(40), files: Object.keys(tree).sort(), only: [id], now });
    return failures.map((f) => f.problem);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

test("the consumer contracts are app-check rules, in order, with their gaps", () => {
  assert.deepEqual(CONSUMER_CONTRACT_RULES.map((r) => [r.id, r.gap]), [
    ["release-config-identity", "CA-01"], ["pnpm-node-floor", "CA-05"], ["rust-dependency-msrv", "CA-05"], ["release-age", "CA-06"],
    ["notice-lock-digest", "CA-14"], ["doc-comment-bare-cr", "CA-15"],
  ]);
  for (const rule of CONSUMER_CONTRACT_RULES) assert.equal(RULES.find((r) => r.id === rule.id), rule, `${rule.id} runs in the app check`);
  const source = fs.readFileSync(new URL("./consumer-contracts.mjs", import.meta.url), "utf8");
  assert.deepEqual([...source.matchAll(/^import .* from "([^"]+)";$/gm)].map((m) => m[1]).sort(), ["node:crypto", "node:fs", "node:path"], "builtins only: no repository code, no network");
});

test("the pinned inputs name their source and fetch time", () => {
  const data = contractData();
  assert.match(data.cratesRustVersion.fetchedAt, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
  assert.match(data.npmPublishTimes.fetchedAt, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
  // The registry facts the original failures printed.
  assert.equal(data.cratesRustVersion.crates["rightkit-db"]["0.3.2"], "1.98.0");
  assert.equal(data.cratesRustVersion.crates["rightkit-fs"]["0.3.3"], "1.98.0");
  assert.equal(data.npmPublishTimes.packages["@rightkit/release"]["0.2.122"], "2026-10-07T20:32:40.773Z");
  assert.equal(data.npmPublishTimes.packages["@rightkit/app-shell"]["0.6.1"], "2026-10-11T02:22:25.064Z");
  assert.deepEqual(data.pnpm.nodeFloor, [{ pnpm: "11.0.0", node: "22.13.0" }]);
  assert.deepEqual(data.pnpm.defaultMinimumReleaseAge, [{ pnpm: "11.0.0", minutes: 1440 }]);
});

test("versions: a partial selection resolves to the newest of its line", () => {
  assert.equal(selectionBelow("20", "22.13.0"), true);
  assert.equal(selectionBelow("22", "22.13.0"), false);
  assert.equal(selectionBelow("22.12", "22.13.0"), true);
  assert.equal(selectionBelow("v22.13.0", "22.13.0"), false);
  assert.equal(selectionBelow("1.90.0", "1.98.0"), true);
  assert.equal(selectionBelow("1.98", "1.98.0"), false);
  assert.equal(selectionBelow("lts/*", "22.13.0"), false, "not a plain version: not judged");
  assert.equal(compareVersions("11.16.0", "11.0.0"), 1);
});

// ---- CA-01 -------------------------------------------------------------------------------------------------------------------------

// Ember 9961562:right-release.config.mjs, the config of run 38858187099 (jobs 116841421176 and 116841421279), verbatim.
const EMBER_9961562 = `import path from 'node:path';
const build = { cmd: 'node', args: ['scripts/desktop/build-native.mjs'] };
export default {
  schema: 1,
  app: 'lightcraft-desktop',
  packageManager: 'pnpm@11.24.0',
  development: {
    targets: {
      mac: { build: [build], install: { kind: 'tauri', productName: 'Ember', targetTriple: 'aarch64-apple-darwin', manifestPath: 'apps/lightcraft-desktop/Cargo.toml', destination: '/Applications/Ember.app' } },
      win: { build: [build], install: { kind: 'tauri', productName: 'Ember', targetTriple: 'x86_64-pc-windows-msvc', manifestPath: 'apps/lightcraft-desktop/Cargo.toml', expectedInstalledPath: path.join(process.env.LOCALAPPDATA || '.', 'Ember', 'lightcraft-desktop.exe') } },
    },
  },
};
`;

test("release-config-identity: Ember's config without a version is refused before install (original 9961562)", () => {
  const found = check("release-config-identity", { "right-release.config.mjs": EMBER_9961562 });
  assert.equal(found.length, 1);
  assert.match(found[0], /^right-release\.config\.mjs:3 the default export has no top-level `version`: right-release build refuses it \("release config must expose app and version"/);
  // Repair: the version the job lacked.
  assert.deepEqual(check("release-config-identity", { "right-release.config.mjs": EMBER_9961562.replace("  schema: 1,\n", "  schema: 1,\n  version: '0.7.0',\n") }), []);
});

test("release-config-identity: computed, wrapped, named and JSON-derived identities stay green", () => {
  // Ember today: the version is read from Cargo.toml at load and exported by shorthand.
  const computed = "import { readFileSync } from 'node:fs';\nconst version = /x/.exec(readFileSync('Cargo.toml', 'utf8'))?.[1];\n// version: in a comment does not count\nexport default {\n  schema: 1,\n  app: 'lightcraft-desktop',\n  version,\n  targets: { mac: { app: 'x', version: '1' } },\n};\n";
  assert.deepEqual(check("release-config-identity", { "right-release.config.mjs": computed }), []);
  assert.deepEqual(check("release-config-identity", { "apps/a/right-release.config.mjs": "const config = { schema: 1, app: 'a', 'version': '1.2.3' };\nexport default config;\n" }), []);
  assert.deepEqual(check("release-config-identity", { "right-release.config.mjs": "export default defineConfig({ schema: 1, app: 'a', version: pkg.version });\n" }), []);
  assert.deepEqual(check("release-config-identity", { "right-release.config.mjs": "export default { schema: 1, ...shared, app: 'a' };\n" }), [], "a spread may supply version: not judged");
  assert.deepEqual(check("release-config-identity", { "right-release.config.json": JSON.stringify({ schema: 1, app: "rightapp", version: { $fromJson: { file: "src-tauri/tauri.conf.json", pointer: "/version" } } }) }), []);
  assert.deepEqual(check("release-config-identity", { "right-release.config.json": JSON.stringify({ schema: 1, app: "a", version: "{{x}}" }) }), []);
  // A nested `version` is not the release identity; a missing JSON version and an unsafe literal are findings.
  assert.equal(check("release-config-identity", { "right-release.config.mjs": "export default { schema: 1, app: 'a', targets: { mac: { version: '1' } } };\n" }).length, 1);
  assert.match(check("release-config-identity", { "right-release.config.json": '{ "schema": 1, "app": "a" }' })[0], /has no top-level "version"/);
  assert.match(check("release-config-identity", { "right-release.config.mjs": "export default { schema: 1, app: 'a b', version: '1' };\n" })[0], /`app: "a b"` is not a safe release-id component/);
  assert.deepEqual(check("release-config-identity", { "node_modules/x/right-release.config.mjs": EMBER_9961562 }), []);
  assert.deepEqual(check("release-config-identity", { "right-release.config.mjs": "export default {};\n", "b/right-release.config.json": "{}" }), [], "a stub without schema is not a release config");
});

// ---- CA-05 pnpm-node-floor ---------------------------------------------------------------------------------------------------------

// CutRight c252c36:.github/workflows/ci.yml (run 30718913302, jobs 91419254795 and 91419254810), the gate job's steps.
const CUTRIGHT_CI = `name: ci
on:
  push:
jobs:
  gate:
    name: gate (\${{ matrix.os }})
    runs-on: \${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4

      - name: Resolve pnpm version from packageManager pin
        id: pnpm
        run: |
          v="$(sed -n 's/.*"packageManager"[[:space:]]*:[[:space:]]*"pnpm@\\([0-9][0-9.]*\\)".*/\\1/p' apps/studio/package.json)"
          echo "version=$v" >> "$GITHUB_OUTPUT"

      - uses: pnpm/action-setup@v4
        with:
          version: \${{ steps.pnpm.outputs.version }}

      - uses: actions/setup-node@v4
        with:
          node-version: 20
          cache: pnpm
          cache-dependency-path: apps/studio/pnpm-lock.yaml
`;
const STUDIO_PACKAGE = '{\n  "name": "studio",\n  "private": true,\n  "packageManager": "pnpm@11.16.0"\n}\n';

test("pnpm-node-floor: CutRight's Node 20 job under pnpm 11.16.0 is refused (original c252c36)", () => {
  const found = check("pnpm-node-floor", { ".github/workflows/ci.yml": CUTRIGHT_CI, "apps/studio/package.json": STUDIO_PACKAGE });
  assert.equal(found.length, 1);
  assert.match(found[0], /^\.github\/workflows\/ci\.yml:23 job gate sets up Node 20 and runs pnpm: apps\/studio\/package\.json pins pnpm 11\.16\.0 \(packageManager\), which requires Node 22\.13\.0/);
  assert.deepEqual(check("pnpm-node-floor", { ".github/workflows/ci.yml": CUTRIGHT_CI.replace("node-version: 20", "node-version: 22.13.0"), "apps/studio/package.json": STUDIO_PACKAGE }), []);
});

test("pnpm-node-floor: jobs without pnpm, line-only selections, expressions and older pnpm stay green", () => {
  const noPnpm = "jobs:\n  docs:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/setup-node@v4\n        with:\n          node-version: 20\n      - run: node scripts/docs.mjs\n";
  assert.deepEqual(check("pnpm-node-floor", { ".github/workflows/docs.yml": noPnpm, "package.json": STUDIO_PACKAGE }), []);
  assert.deepEqual(check("pnpm-node-floor", { ".github/workflows/ci.yml": CUTRIGHT_CI.replace("node-version: 20", "node-version: '22'"), "apps/studio/package.json": STUDIO_PACKAGE }), []);
  assert.deepEqual(check("pnpm-node-floor", { ".github/workflows/ci.yml": CUTRIGHT_CI.replace("node-version: 20", "node-version: ${{ matrix.node }}"), "apps/studio/package.json": STUDIO_PACKAGE }), []);
  assert.deepEqual(check("pnpm-node-floor", { ".github/workflows/ci.yml": CUTRIGHT_CI, "apps/studio/package.json": STUDIO_PACKAGE.replace("11.16.0", "10.18.0") }), []);
  // A `run: pnpm ...` step is pnpm too, and a node-version-file is read.
  const runPnpm = "jobs:\n  t:\n    steps:\n      - uses: actions/setup-node@v4\n        with:\n          node-version-file: .nvmrc\n      - run: pnpm install --frozen-lockfile\n";
  assert.equal(check("pnpm-node-floor", { ".github/workflows/t.yml": runPnpm, ".nvmrc": "20.11.1\n", "package.json": STUDIO_PACKAGE }).length, 2);
  assert.deepEqual(check("pnpm-node-floor", { ".github/workflows/t.yml": runPnpm, ".nvmrc": "26.8.1\n", "package.json": STUDIO_PACKAGE }), []);
  assert.deepEqual(workflowJobs(CUTRIGHT_CI).map((j) => [j.id, j.steps.length]), [["gate", 4]]);
});

// ---- CA-05 rust-dependency-msrv ----------------------------------------------------------------------------------------------------

// Pulse b407fe30 (run 38026724922, job 114139038212): rust-toolchain.toml verbatim and the two Cargo.lock entries the error named.
const PULSE_TOOLCHAIN = '[toolchain]\nchannel = "1.90.0"\nprofile = "minimal"\ncomponents = ["rustfmt", "clippy"]\n';
const PULSE_LOCK = `# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "base64"
version = "0.22.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "72b3254f16251a8381aa12e40e3c4d2f0199f8c6508fbecb9d91f575e0fbb8c6"

[[package]]
name = "rightkit-db"
version = "0.3.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "14468922a816b997dc6e9723ef36d860291fcec50e4ad902029c9502ec012451"

[[package]]
name = "rightkit-fs"
version = "0.3.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "b4c2822f001b358c402c4d2804071eeb71b9fbacf0bd2bb24b22bcf1eec4d246"

[[package]]
name = "rightkit-local"
version = "0.3.2"
`;

test("rust-dependency-msrv: Pulse's 1.90.0 toolchain against rightkit-db 0.3.2 and rightkit-fs 0.3.3 (original b407fe30)", () => {
  const found = check("rust-dependency-msrv", { "rust-toolchain.toml": PULSE_TOOLCHAIN, "Cargo.lock": PULSE_LOCK });
  assert.deepEqual(found.map((p) => p.split(" (")[0]), ["Cargo.lock:11 resolves rightkit-db 0.3.2, which requires rustc 1.98.0", "Cargo.lock:17 resolves rightkit-fs 0.3.3, which requires rustc 1.98.0"]);
  assert.match(found[0], /but rust-toolchain\.toml selects 1\.90\.0: cargo refuses it \("rustc 1\.90\.0 is not supported by the following packages"\)/);
  // Repair (Pulse 67b24a11: "toolchain: Rust 1.98.0").
  assert.deepEqual(check("rust-dependency-msrv", { "rust-toolchain.toml": PULSE_TOOLCHAIN.replace("1.90.0", "1.98.0"), "Cargo.lock": PULSE_LOCK }), []);
});

test("rust-dependency-msrv: unpinned channels, path crates, unknown versions and the nearest toolchain stay correct", () => {
  assert.deepEqual(check("rust-dependency-msrv", { "rust-toolchain.toml": PULSE_TOOLCHAIN.replace("1.90.0", "stable"), "Cargo.lock": PULSE_LOCK }), []);
  assert.deepEqual(check("rust-dependency-msrv", { "rust-toolchain": "1.90.0\n", "Cargo.lock": PULSE_LOCK.replaceAll('"0.3.2"', '"9.9.9"').replace('"0.3.3"', '"9.9.9"') }), [], "a version the snapshot does not list is not judged");
  // hub/ has its own newer toolchain; only the root lock is judged against 1.90.0.
  const tree = { "rust-toolchain.toml": PULSE_TOOLCHAIN, "hub/rust-toolchain.toml": PULSE_TOOLCHAIN.replace("1.90.0", "1.98.0"), "hub/Cargo.lock": PULSE_LOCK, "Cargo.lock": PULSE_LOCK.split("[[package]]\nname = \"rightkit-db\"")[0] };
  assert.deepEqual(check("rust-dependency-msrv", tree), []);
  assert.equal(check("rust-dependency-msrv", { ...tree, "hub/rust-toolchain.toml": PULSE_TOOLCHAIN }).length, 2);
});

// ---- CA-06 release-age -------------------------------------------------------------------------------------------------------------

// Pulse a5188983 (run 37683011855, job 113003652672): pnpm-workspace.yaml verbatim; package.json and pnpm-lock.yaml excerpts.
const PULSE_WORKSPACE = "minimumReleaseAgeExclude:\n  - '@rightkit/release@0.2.111'\nallowBuilds:\n  fs-xattr: true\n  macos-alias: true\n";
const PULSE_PACKAGE = '{\n  "name": "cockpit",\n  "private": true,\n  "packageManager": "pnpm@11.24.0"\n}\n';
const PULSE_PNPM_LOCK = `lockfileVersion: '9.0'

importers:

  .:
    devDependencies:
      '@rightkit/release':
        specifier: 0.2.122
        version: 0.2.122

packages:

  '@electron/osx-sign@2.7.1':
    resolution: {integrity: sha512-VZ7Ei2PpqBxPU0Kf1evYSiQKCaluIy6t/DSlNpYZQX0z27KwNNHuJR1bz3Pqj1DIL5EySD8RlH2PRrsrfLEdKg==}

  '@rightkit/release@0.2.122':
    resolution: {integrity: sha512-KfVEiTEVzxm/h+zmQHnqEMB/gbgHF3r+oR+Uj4rDpLdiGG/j8aZKfZGW1KPMvJh/02lnjf2NHMFgi1TdhLVbzg==}
    hasBin: true

snapshots:

  '@rightkit/release@0.2.122': {}
`;
// The clock of the original install: pnpm printed the cutoff 2026-10-06T20:34:42.597Z for its 1440-minute default.
const PULSE_INSTALL = new Date("2026-10-07T20:34:42.597Z");

test("release-age: @rightkit/release 0.2.122 two minutes old under the pnpm 11 default (original a5188983)", () => {
  const tree = { "pnpm-workspace.yaml": PULSE_WORKSPACE, "package.json": PULSE_PACKAGE, "pnpm-lock.yaml": PULSE_PNPM_LOCK };
  const found = check("release-age", tree, { now: PULSE_INSTALL });
  assert.equal(found.length, 1);
  assert.match(found[0], /^pnpm-lock\.yaml:16 @rightkit\/release@0\.2\.122 was published at 2026-10-07T20:32:40\.773Z, within the minimumReleaseAge cutoff \(2026-10-06T20:34:42\.597Z; 1440 minutes from the pnpm 11\.24\.0 default/);
  // Repairs: the same lock a day later, or the reviewed exact exemption.
  assert.deepEqual(check("release-age", tree, { now: new Date("2026-10-08T20:32:40.774Z") }), []);
  assert.deepEqual(check("release-age", { ...tree, "pnpm-workspace.yaml": PULSE_WORKSPACE.replace("0.2.111'", "0.2.111'\n  - '@rightkit/release@0.2.122'") }, { now: PULSE_INSTALL }), []);
});

test("release-age: the hub's two fresh entries with stale exemptions (original 2abe97b3, run 38110620758)", () => {
  const hubWorkspace = "packages:\n  - \".\"\nminimumReleaseAgeExclude:\n  - '@rightkit/app-shell@0.5.0'\n  - '@rightkit/platform-ui@0.2.1'\n";
  const hubLock = "lockfileVersion: '9.0'\n\npackages:\n\n  '@rightkit/app-shell@0.6.1':\n    resolution: {integrity: sha512-x}\n\n  '@rightkit/platform-ui@0.2.4':\n    resolution: {integrity: sha512-y}\n\nsnapshots:\n\n  '@rightkit/app-shell@0.6.1(@rightkit/platform-ui@0.2.4(react@19.3.0))(react@19.3.0)': {}\n";
  const found = check("release-age", { "package.json": PULSE_PACKAGE, "hub/pnpm-workspace.yaml": hubWorkspace, "hub/pnpm-lock.yaml": hubLock }, { now: new Date("2026-10-11T04:11:34.125Z") });
  assert.deepEqual(found.map((p) => /^(\S+) (\S+) was published at (\S+), within the minimumReleaseAge cutoff \((\S+);/.exec(p).slice(1)), [
    ["hub/pnpm-lock.yaml:5", "@rightkit/app-shell@0.6.1", "2026-10-11T02:22:25.064Z", "2026-10-10T04:11:34.125Z"],
    ["hub/pnpm-lock.yaml:8", "@rightkit/platform-ui@0.2.4", "2026-10-11T03:43:31.669Z", "2026-10-10T04:11:34.125Z"],
  ]);
});

test("release-age: an explicit age, a disabled policy, older pnpm, patterns and unknown packages", () => {
  const tree = { "pnpm-workspace.yaml": PULSE_WORKSPACE, "package.json": PULSE_PACKAGE, "pnpm-lock.yaml": PULSE_PNPM_LOCK };
  assert.deepEqual(check("release-age", { ...tree, "pnpm-workspace.yaml": `minimumReleaseAge: 0\n${PULSE_WORKSPACE}` }, { now: PULSE_INSTALL }), []);
  assert.deepEqual(check("release-age", { ...tree, "package.json": PULSE_PACKAGE.replace("11.24.0", "10.18.0") }, { now: PULSE_INSTALL }), [], "pnpm 10 has no default age");
  assert.equal(check("release-age", { ...tree, "pnpm-workspace.yaml": `minimumReleaseAge: 5\n${PULSE_WORKSPACE}`, "package.json": PULSE_PACKAGE.replace("11.24.0", "10.18.0") }, { now: PULSE_INSTALL }).length, 1, "an explicit age applies to any pnpm");
  assert.deepEqual(check("release-age", { ...tree, "pnpm-workspace.yaml": "minimumReleaseAgeExclude:\n  - '@rightkit/*'\n" }, { now: PULSE_INSTALL }), []);
  assert.deepEqual(check("release-age", { ...tree, "pnpm-lock.yaml": PULSE_PNPM_LOCK.replaceAll("@rightkit/release@0.2.122", "@other/pkg@1.0.0") }, { now: PULSE_INSTALL }), [], "not in the snapshot: not judged");
  assert.equal(excluded(["@rightkit/release"], "@rightkit/release", "0.2.122"), true);
  assert.equal(excluded(["@rightkit/release@0.2.121 || 0.2.122"], "@rightkit/release", "0.2.122"), true);
  assert.equal(excluded(["@rightkit/release@0.2.111"], "@rightkit/release", "0.2.122"), false);
});

// ---- CA-14 notice-lock-digest ------------------------------------------------------------------------------------------------------

const sha = (text) => createHash("sha256").update(text).digest("hex");
const LOCK_OLD = "lockfileVersion: '9.0'\n# the lock the notice was generated from\n";
const LOCK_NEW = "lockfileVersion: '9.0'\n# the committed lock after a dependency moved\n";
// HeardRight dec55223^:tauri-app-next/legal/THIRD_PARTY_NPM_LICENSES.html line 4 (the provenance line), digest substituted for the fixture lock.
const npmNotice = (digest) => `<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><title>HeardRight npm third-party licenses</title></head><body>\n<h1>HeardRight npm third-party licenses</h1>\n<p>Build provenance: HeardRight 0.1.230; inputs: pnpm-lock.yaml sha256=${digest}; generated-by=generate-npm-license-appendix.mjs</p>\n`;
// HeardRight THIRD_PARTY_NOTICES.md quotes the locks in backticks.
const mdNotice = (digest) => `# Third-party notices\n\n**Product:** HeardRight 0.1.230 for Windows and macOS\n\n- \`pnpm-lock.yaml\` sha256=\`${digest}\`\n`;

test("notice-lock-digest: HeardRight's npm appendix generated from a lock that later moved (original dec55223^)", () => {
  const tree = { "tauri-app-next/pnpm-lock.yaml": LOCK_NEW, "tauri-app-next/legal/THIRD_PARTY_NPM_LICENSES.html": npmNotice(sha(LOCK_OLD)), "tauri-app-next/legal/THIRD_PARTY_NOTICES.md": mdNotice(sha(LOCK_OLD)) };
  const found = check("notice-lock-digest", tree);
  assert.deepEqual(found.map((p) => p.split(" records")[0]), ["tauri-app-next/legal/THIRD_PARTY_NOTICES.md:5", "tauri-app-next/legal/THIRD_PARTY_NPM_LICENSES.html:4"]);
  assert.match(found[1], new RegExp(`records pnpm-lock\\.yaml sha256=${sha(LOCK_OLD)}, but the committed tauri-app-next/pnpm-lock\\.yaml hashes to ${sha(LOCK_NEW)}`));
  // Repair (dec55223: "rebind the third-party notices to the ... pnpm-lock.yaml").
  assert.deepEqual(check("notice-lock-digest", { ...tree, "tauri-app-next/legal/THIRD_PARTY_NPM_LICENSES.html": npmNotice(sha(LOCK_NEW)), "tauri-app-next/legal/THIRD_PARTY_NOTICES.md": mdNotice(sha(LOCK_NEW)) }), []);
});

test("notice-lock-digest: recorded history, non-lock digests and locks the tree does not hold are not judged", () => {
  assert.deepEqual(check("notice-lock-digest", { "pnpm-lock.yaml": LOCK_NEW, "docs/evidence/2026-10-07/THIRD_PARTY_NOTICES.md": mdNotice(sha(LOCK_OLD)) }), []);
  assert.deepEqual(check("notice-lock-digest", { "pnpm-lock.yaml": LOCK_NEW, "legal/THIRD_PARTY_BINARY_LICENSES.html": `<p>DirectML.dll sha256=${sha("x")}</p>\n` }), []);
  assert.deepEqual(check("notice-lock-digest", { "legal/THIRD_PARTY_NOTICES.md": mdNotice(sha(LOCK_OLD)) }), []);
  assert.deepEqual(check("notice-lock-digest", { "README.md": `pnpm-lock.yaml sha256=${sha(LOCK_OLD)}\n`, "pnpm-lock.yaml": LOCK_NEW }), [], "only notice and licence files carry provenance");
  // The nearest lock wins: an app's own lock, not the repository root's.
  assert.deepEqual(check("notice-lock-digest", { "pnpm-lock.yaml": LOCK_OLD, "app/pnpm-lock.yaml": LOCK_NEW, "app/legal/THIRD_PARTY_NOTICES.md": mdNotice(sha(LOCK_NEW)) }), []);
});

// ---- CA-15 doc-comment-bare-cr -----------------------------------------------------------------------------------------------------

// Pulse 0ff1d717:windows/src/hub.rs lines 1-6 (run 38110627257, job 114385259883): `bridge\relay.json` with the \r written as a CR byte.
const HUB_RS = "//! Runs and opens the shared Tauri hub from the notch (the Windows counterpart of the Mac\n//! notch's `HubLauncher`). The hub is Pulse's daemon: `supervise` starts it with\n//! `--background` (no window) when the notch starts, whether or not Nearby sharing is on, and\n//! starts it again, at most once every 30 seconds, if it exits. A hub that runs but whose\n//! Chat heartbeat (`bridge\relay.json`, written about every 2 s) is older than 90 seconds is\n//! ended and started again, at most once every ten minutes. Only quitting the notch\n";

test("doc-comment-bare-cr: the CR in Pulse's hub.rs doc comment is refused before rustc (original 0ff1d717)", () => {
  const found = check("doc-comment-bare-cr", { "windows/src/hub.rs": Buffer.from(HUB_RS, "latin1") });
  assert.deepEqual(found.map((p) => p.split(" a carriage")[0]), ["windows/src/hub.rs:5:28"], "rustc: --> windows\\src\\hub.rs:5:28");
  // Repair (Pulse main): a literal backslash.
  assert.deepEqual(check("doc-comment-bare-cr", { "windows/src/hub.rs": HUB_RS.replace("\r", "\\r") }), []);
});

test("doc-comment-bare-cr: CRLF files, plain comments and non-doc slashes stay green; doc blocks are read", () => {
  assert.deepEqual(check("doc-comment-bare-cr", { "src/a.rs": HUB_RS.replace("\r", "\\r").replaceAll("\n", "\r\n") }), []);
  assert.deepEqual(check("doc-comment-bare-cr", { "src/a.rs": "// a plain comment may hold a\rCR\n//// four slashes is not a doc comment\r\nfn main() {}\n" }), []);
  assert.deepEqual(docCommentBareCrs("/** a doc block\n * with a \r stray\n */\nfn f() {}\n"), [{ line: 2, column: 11 }]);
  assert.deepEqual(docCommentBareCrs("/* a plain block \r */\n/*! inner doc \r */\n"), [{ line: 2, column: 15 }]);
  assert.deepEqual(docCommentBareCrs("/// ends with CRLF\r\n/// and a stray\r\r\n"), [{ line: 2, column: 16 }]);
});

// ---- CA-15 duplicate required-features: the existing class A toml-parse scan -------------------------------------------------------

// Pulse rk-snapshot/rightkit-fs/Cargo.toml, blob 9e064779 (in PR merge ref 1bfef407 of runs 38150273072/38150304408, and on main at
// 7e5ac5a1 until 638e94a6 removed the stray lines): the [[bin]] tables and the first stray keys, verbatim.
const RK_SNAPSHOT_FS = `[package]
name = "rightkit-fs"
version = "0.3.4"
edition = "2021"

[[bin]]
name = "rightkit-clonefile"
path = "src/bin/rightkit-clonefile.rs"
required-features = ["json"]

[[bin]]
name = "rightkit-swap"
path = "src/bin/rightkit-swap.rs"
required-features = ["fs"]

required-features = ["json"]

required-features = ["fs"]

[features]
default = ["fs", "lock", "json", "digest"]
`;

test("toml-parse (class A scan) refuses the duplicate required-features of Pulse's committed rk-snapshot", async () => {
  const content = path.join(path.sep, "c", "pulse");
  const norm = (p) => p.replaceAll("\\", "/");
  const scan = async (text) => {
    const files = new Map([[norm(path.join(content, "rk-snapshot/rightkit-fs/Cargo.toml")), text]]);
    const fake = { exists: (p) => files.has(norm(p)) || [...files.keys()].some((k) => k.startsWith(`${norm(p)}/`)), readFile: (p) => { const v = files.get(norm(p)); if (v === undefined) throw new Error(`ENOENT ${p}`); return v; }, realpath: (p) => p };
    const plan = planScans({ changed: ["rk-snapshot/rightkit-fs/Cargo.toml"], contentRoot: content, liveRoot: content, ...fake, platform: "linux", includeTests: false });
    const spawn = (command, args) => (args.length === 1 && args[0].includes("version") ? { status: 0, stdout: "x 1.0.0\n", stderr: "" } : { status: 0, stdout: "", stderr: "" });
    const result = await runScans(plan, { spawn, ...fake, findRustfmt: () => "rustfmt", scanSecrets: async () => () => [], env: {}, execPath: "node", classes: ["A"] });
    return result.scans.find((s) => s.id === "toml-parse");
  };
  const red = await scan(RK_SNAPSHOT_FS);
  assert.equal(red.status, "fail", JSON.stringify(red));
  assert.match(JSON.stringify(red), /rk-snapshot\/rightkit-fs\/Cargo\.toml: Invalid TOML document: trying to redefine an already defined table or value/);
  // Repair (638e94a6 removed the stray lines); positive control: one required-features per [[bin]] table.
  const green = await scan(RK_SNAPSHOT_FS.replace('required-features = ["json"]\n\nrequired-features = ["fs"]\n\n[features]', "[features]"));
  assert.equal(green.status, "pass", JSON.stringify(green));
});
