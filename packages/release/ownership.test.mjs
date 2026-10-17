import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { canonicalJson, canonicalRepositoryIdentity, createDraft, keyIdForPublicKey, occurrenceHash, policyEntryDigest, provenanceContentDigest, sha256 } from "./ownership-approval.mjs";
import { aggregateOwnership, catalogDigest, checkApp, checkAppForTest, checkCommittedAppForTest, contentInventory, directDependencies, evaluate, MANIFEST_FILE, scanApp, scanAppForTest, SCANNER_VERSION } from "./ownership.mjs";
import { __testOnly as authorityTestOnly } from "./ownership-key-authority.mjs";
import { createKeyEvent, eventHash, keyFingerprint } from "./ownership-key-log.mjs";
import { generateOwnershipCatalog } from "./ownership-catalog.mjs";
import { buildOwnershipReport, renderOwnershipMarkdown } from "./ownership-report.mjs";
import { FINGERPRINT_SAMPLES } from "./fixtures/ownership/source-fingerprint-samples.mjs";

const rightkitRoot = path.resolve(import.meta.dirname, "../..");
const canonicalCatalog = JSON.parse(readFileSync(path.join(import.meta.dirname, MANIFEST_FILE), "utf8"));
const shipped = structuredClone(canonicalCatalog);
const approvalCatalog = { ...shipped, capabilities: shipped.capabilities.map((c) => ({ ...c, tier: "block" })) };
const SAMPLES = {
  "llm.endpoint.provider-request": 'client.post("https://api.anthropic.com/v1/messages").send();',
  "oauth.endpoint.github": 'const URL: &str = "https://github.com/login/device/code";',
  "oauth.endpoint.google": 'let u = "https://oauth2.googleapis.com/device/code";',
  "oauth.endpoint.openai": 'const AUTH = "https://auth.openai.com/oauth/authorize";',
  "oauth.endpoint.apple": 'let url = URL(string: "https://appleid.apple.com/auth/authorize")!',
  "oauth.endpoint.cloudflare": 'const u = "https://dash.cloudflare.com/oauth2/auth";',
  "oauth.device-grant": 'grant_type: "urn:ietf:params:oauth:grant-type:device_code",',
  "secrets.api.wincred": "let ok = unsafe { CredWriteW(&cred, 0) };",
  "secrets.api.secitem": "let status = SecItemCopyMatching(query as CFDictionary, &item)",
  "secrets.swift.scoped-migration": "public func copySyncedToDeviceOnly(_ account: String) -> Result<Bool, AppleCredentialError> {",
  "sandbox.api.appcontainer": "unsafe { CreateAppContainerProfile(name, display, desc, caps, 0, &mut sid) }",
  "sandbox.api.seatbelt": "let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut err) };",
  "process.api.kill-on-job-close": "info.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;",
  "ort.env.dylib": 'std::env::set_var("ORT_DYLIB_PATH", p)',
  "ort.api.init": "ort::init()",
  "browser.source.chrome-discovery": 'let path = r"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";',
  "control.api.cgevent-post": "unsafe { CGEventPostToPid(pid, event) };",
  "control.api.ax-element": "let app = unsafe { AXUIElementCreateApplication(pid) };",
  "control.api.sendinput": "let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };",
  "mcp.protocol.initialized": 'self.notify("notifications/initialized", json!({})).await?;',
  ...Object.fromEntries(Object.entries(FINGERPRINT_SAMPLES).map(([id, { positive }]) => [id, positive[0]])),
};
// Scanner mechanism tests use exact sample rules independent of production policy.
// Removed catalog patterns stay removed; these fixtures exercise cache/provenance.
const SAMPLE_OWNERS = {
  "browser.source.chrome-discovery": "rightkit-browser",
  "control.api.cgevent-post": "rightkit-qa-driver",
  "control.api.ax-element": "rightkit-qa-driver",
  "control.api.sendinput": "rightkit-qa-driver",
  "mcp.protocol.initialized": "rightkit-mcp",
  "oauth.endpoint.github": "rightkit-oauth",
  "oauth.endpoint.google": "rightkit-oauth",
  "oauth.endpoint.openai": "rightkit-oauth",
  "oauth.endpoint.apple": "rightkit-oauth",
  "oauth.endpoint.cloudflare": "rightkit-oauth",
  "oauth.device-grant": "rightkit-oauth",
  "process.api.kill-on-job-close": "rightkit-child-process",
  "sandbox.api.appcontainer": "rightkit-sandbox",
  "sandbox.api.seatbelt": "rightkit-sandbox",
  "secrets.api.wincred": "rightkit-credentials",
  "secrets.api.secitem": "rightkit-credentials",
  "secrets.swift.scoped-migration": "RightKitSwift"
};
for (const [id, owner] of Object.entries(SAMPLE_OWNERS)) {
  const entry = shipped.capabilities.find((capability) => capability.package === owner);
  const pattern = [...SAMPLES[id]].map((ch) => "\\^$.*+?()[]{}|".includes(ch) ? "\\" + ch : ch).join("");
  entry.fingerprints.push({ id, type: "source", pattern, tier: "block" });
}

const NEUTRAL = "let greeting = 'hello';";

function app(files = {}) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rk-ownership-")));
  assert.equal(spawnSync("git", ["-C", root, "init", "-q"], { windowsHide: true }).status, 0);
  for (const [rel, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    writeFileSync(path.join(root, rel), text);
  }
  return root;
}

function signedException(findings, { root, fingerprint = "oauth.endpoint.github", file, today = "2026-10-07", agentField = false } = {}) {
  const selected = findings.filter((f) => f.fingerprint === fingerprint && f.file === file);
  const draft = createDraft({ repositoryIdentity: root, policyDigest: policyEntryDigest(approvalCatalog, fingerprint), minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1", issued: today, fingerprint,
    findings: selected, reason: "bounded migration", owner: "CodeRight", expires: "2026-12-31",
    requestId: "RK-OWNERSHIP-001", trackingRef: { id: "RK-123456", url: "https://github.com/org/repo/issues/1", verified: true } });
  const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const { publicKey: macPublicKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const winKeyId = keyIdForPublicKey("windows", publicKey), macKeyId = keyIdForPublicKey("macos", macPublicKey);
  const record = { ...draft, signature: { keyId: winKeyId, host: "windows", value: sign("sha256", Buffer.from(canonicalJson(draft.payload)), privateKey).toString("base64") } };
  if (agentField) record.approvedBy = "agent";
  const pem = publicKey.export({ type: "spki", format: "pem" });
  const trackingRef = record.payload.trackingRef;
  const trust = { schema: 1, minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1", hosts: {
    windows: { keyId: winKeyId, publicKeyPem: pem }, macos: { keyId: macKeyId, publicKeyPem: macPublicKey.export({ type: "spki", format: "pem" }) },
  }, revokedKeyIds: [], verifiedTrackingEvidence: [sha256({ repositoryIdentity: root, requestId: record.payload.requestId, trackingRef })],
    repositories: [{ repositoryIdentity: root, roots: ["."], visibility: "public" }], provenance: [] };
  return { record, trust };
}

test("shipped catalog equals declarations including canonical Swift declarations", () => {
  assert.deepEqual(canonicalCatalog, generateOwnershipCatalog(rightkitRoot));
  assert.equal(canonicalCatalog.capabilities.find((c) => c.package === "RightKitSwift").detection, "fingerprinted");
});

test("every source fingerprint has a realistic positive and neutral negative sample", () => {
  for (const c of shipped.capabilities) for (const f of c.fingerprints.filter((x) => x.type === "source")) {
    assert.ok(SAMPLES[f.id], `add realistic positive sample for ${f.id}`);
    const re = new RegExp(f.pattern, f.flags ?? "");
    assert.ok(re.test(SAMPLES[f.id]), `${f.id} misses its sample`);
    assert.ok(!re.test(NEUTRAL), `${f.id} matches neutral source`);
    if (f.id === "secrets.swift.scoped-migration") assert.ok(!re.test("store.copySyncedToDeviceOnly(account)"), "API use must remain valid without being classified as copied implementation");
  }
});

test("rightkit-onnx adoption permits ORT mentions, reads, docs, logs and comments", () => {
  const root = app({
    "Cargo.toml": '[package]\nname = "ort-consumer"\nversion = "0.1.0"\n[dependencies]\nrightkit-onnx = "0.1.0"\n',
    "Cargo.lock": 'version=4\n[[package]]\nname="ort-consumer"\nversion="0.1.0"\ndependencies=["rightkit-onnx"]\n[[package]]\nname="rightkit-onnx"\nversion="0.1.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n',
    "src/doctor.rs": [
      'let doctor = "Set ORT_DYLIB_PATH to select a runtime";',
      'let runtime = std::env::var("ORT_DYLIB_PATH");',
      'let runtime_os = std::env::var_os("ORT_DYLIB_PATH");',
      '// std::env::set_var("ORT_DYLIB_PATH", p); ort::init();',
      'let ready = true; // remove_var("ORT_DYLIB_PATH"); ort::init_from(p);',
      '/* documentation',
      'set_var("ORT_DYLIB_PATH", p); ort::init();',
      '/* nested comment */ ort::init_from(p);',
      '*/',
      'println!(r#"Example: set_var("ORT_DYLIB_PATH", p); ort::init();"#);',
      'let example = r"ort::init_from(p)";',
    ].join("\n"),
    "src/doctor.ts": [
      'console.log("Doctor: ORT_DYLIB_PATH selects runtime");',
      'const runtime = process.env.ORT_DYLIB_PATH;',
      'const matches = process.env.ORT_DYLIB_PATH === expected;',
      'const matchesAgain = process.env.ORT_DYLIB_PATH == expected;',
      'const example = \'process.env.ORT_DYLIB_PATH = "runtime"\';',
      'console.log("delete process.env.ORT_DYLIB_PATH");',
      'const template = `Example:\nprocess.env.ORT_DYLIB_PATH = "runtime"`;',
      '// process.env.ORT_DYLIB_PATH = runtime;',
      'const ready = true; // delete process.env.ORT_DYLIB_PATH;',
      '/* documentation',
      'process.env.ORT_DYLIB_PATH = runtime; delete process.env.ORT_DYLIB_PATH;',
      '*/',
    ].join("\n"),
    "README.md": 'ORT_DYLIB_PATH\nstd::env::set_var("ORT_DYLIB_PATH", p);\nort::init();',
  });
  try {
    const result = checkApp(root, shipped, { trust: { enrolled: true, trust: null } });
    assert.equal(result.ok, true);
    assert.deepEqual(result.findings, []);
    assert.deepEqual(result.violations, []);
    assert.equal(result.rows.find((row) => row.capability === "onnx-runtime").status, "declared-dependency");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("ORT source findings require env mutation or direct Rust initialization", () => {
  const cases = [
    ["set.rs", 'std::env::set_var("ORT_DYLIB_PATH", p);', "ort.env.dylib", 1],
    ["remove.rs", 'std::env::remove_var("ORT_DYLIB_PATH");', "ort.env.dylib", 1],
    ["unqualified-set.rs", 'set_var ( "ORT_DYLIB_PATH" , p );', "ort.env.dylib", 1],
    ["unqualified-remove.rs", 'remove_var ( "ORT_DYLIB_PATH" );', "ort.env.dylib", 1],
    ["spaced.rs", 'std :: env :: set_var\t(\t"ORT_DYLIB_PATH"\t, p);', "ort.env.dylib", 1],
    ["multiline.rs", 'unsafe {\n std::env::set_var\n (\n "ORT_DYLIB_PATH",\n p\n );\n}', "ort.env.dylib", 2],
    ["init.rs", "ort::init();", "ort.api.init", 1],
    ["init-from.rs", 'ort::init_from("runtime.dylib");', "ort.api.init", 1],
    ["spaced-init.rs", "ort \n :: \n init_from \n (p);", "ort.api.init", 1],
    ["set.ts", "process.env.ORT_DYLIB_PATH = p;", "ort.env.dylib", 1],
    ["remove.js", "delete process.env.ORT_DYLIB_PATH;", "ort.env.dylib", 1],
    ["spaced.tsx", "process \n . env \n . ORT_DYLIB_PATH \n = p;", "ort.env.dylib", 1],
    ["spaced-remove.mjs", "delete \n process . env . ORT_DYLIB_PATH;", "ort.env.dylib", 1],
    ["after-log.cjs", 'console.log("delete process.env.ORT_DYLIB_PATH"); delete process.env.ORT_DYLIB_PATH;', "ort.env.dylib", 1],
    ["after-comment.jsx", "/* process.env.ORT_DYLIB_PATH = fake; */ process.env.ORT_DYLIB_PATH = p;", "ort.env.dylib", 1],
  ];
  const root = app({
    ...Object.fromEntries(cases.map(([file, source]) => [`src/${file}`, source])),
    // Identical blobs in different languages must not share filtered findings.
    "src/init-copy.ts": "ort::init();",
    "src/not-init.rs": "ort::initialize(); other_ort::init(); ort::init_from_path(p);",
    "src/not-env.ts": "process.env.ORT_DYLIB_PATH_EXTRA = p; delete process.env.ORT_DYLIB_PATH_EXTRA;",
    "src/not-env.rs": 'std::env::set_var("OTHER_ENV", "ORT_DYLIB_PATH");',
  });
  try {
    const findings = scanApp(root, shipped);
    const expected = cases.map(([file, , fingerprint, line]) => ({ file: `src/${file}`, fingerprint, line }));
    const summarize = (items) => items.map(({ file, fingerprint, line }) => ({ file, fingerprint, line })).sort((a, b) => a.file.localeCompare(b.file));
    assert.deepEqual(summarize(findings), summarize(expected));
    assert.ok(findings.every((finding) => finding.package === "rightkit-onnx" && finding.capability === "onnx-runtime"));
    assert.deepEqual(scanApp(root, shipped), findings, "warm cache preserves code-only findings");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("Cargo renames/workspace/target/build/dev, npm aliases/local wrappers & Swift identities resolve", () => {
  const root = app({
    "Cargo.toml": '[workspace]\nmembers = ["engine"]\n[workspace.dependencies]\nauth-lib = {\n package = "oauth2",\n version = "4"\n}\n',
    "engine/Cargo.toml": '[dependencies]\nauth-lib = { workspace = true }\n[target.\'cfg(windows)\'.build-dependencies]\nkey-lib = { package = "keyring", version = "3" }\n[dev-dependencies]\nnotify = "6"\n[dependencies.notify-debouncer-full]\nversion = "0.1"\n',
    "web/package.json": JSON.stringify({ dependencies: { authAlias: "npm:oauth4webapi@^1", wrapper: "file:../local-wrapper" } }),
    "ios/Package.swift": '.package(url: "https://github.com/example/RightKitOAuth.GIT", from: "1.0.0"), .package(name: "RightKitSwift", path: "../RightKitSwift")',
    "node_modules/transitive/package.json": JSON.stringify({ dependencies: { puppeteer: "22" } }),
  });
  try {
    const deps = directDependencies(root);
    for (const name of ["cargo:oauth2", "cargo:keyring", "cargo:notify", "cargo:notify-debouncer-full", "npm:oauth4webapi", "npm:wrapper", "swiftpm:rightkitoauth", "swiftpm:RightKitSwift"]) assert.ok(deps.some((d) => `${d.ecosystem}:${d.name}` === name), name);
    assert.ok(!deps.some((d) => d.name === "puppeteer"), "installed third-party transitive is not a direct dependency");
    assert.ok(deps.some((d) => d.alias === "authAlias" && d.name === "oauth4webapi"));
    assert.ok(deps.some((d) => d.local && d.name === "wrapper"));
    assert.ok(deps.coverage.some((x) => /local-npm-dependency-path-unresolved/.test(x.reason)));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("QA and fixture paths get no exemption: a dev dependency used only in tests is a finding", () => {
  const root = app({
    "package.json": JSON.stringify({ devDependencies: { "puppeteer-core": "24" } }),
    "tests/a.test.ts": "import puppeteer from 'puppeteer-core';",
    "fixtures/qa-capture.ts": "const puppeteer = await import('puppeteer-core');",
  });
  try {
    const result = checkApp(root, shipped, { trust: { enrolled: true, trust: null } });
    assert.equal(result.findings.length, 1);
    assert.equal(result.findings[0].fingerprint, "browser.npm.puppeteer-core");
    assert.equal(result.findings[0].qaOnly, undefined);
    assert.equal(result.violations.length, 1);
    assert.equal(result.qaOnly, undefined);
    assert.notEqual(result.rows.find((row) => row.capability === "browser-automation").status, "qa-only");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) test(`npm dependency finding binds ${section} without import usage`, () => {
  const root = app({ "package.json": JSON.stringify({ [section]: { "puppeteer-core": "24" } }) });
  try {
    const result = checkApp(root, shipped, { trust: { enrolled: true, trust: null } });
    assert.equal(result.findings.length, 1);
    assert.equal(result.findings[0].section, section);
    assert.equal(result.findings[0].qaOnly, undefined);
    assert.equal(result.violations.length, 1);
    assert.equal(result.rows.find((row) => row.capability === "browser-automation").status, "blocked");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("workspace-inherited Cargo path resolves from workspace root; local boundary escapes are explicit", () => {
  const root = app({
    "Cargo.toml": '[workspace]\nmembers = ["engine"]\n[workspace.dependencies]\nlocal-auth = { package = "oauth2", path = "shared/auth" }\n',
    "engine/Cargo.toml": '[package]\nname = "engine"\nversion = "0.1.0"\n[dependencies]\nlocal-auth = { workspace = true }\n',
    "shared/auth/Cargo.toml": '[package]\nname = "oauth2"\nversion = "1.0.0"\n',
  });
  try {
    const deps = directDependencies(root);
    assert.ok(deps.some((d) => d.name === "oauth2" && d.local));
    assert.ok(!deps.coverage.some((x) => x.reason.startsWith("local-dependency-")), "valid inherited path resolves from workspace root");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("node_modules layout cannot spoof provenance; untrusted installed source remains a finding", () => {
  const root = app({
    "node_modules/.pnpm/fake/src/auth.rs": SAMPLES["oauth.endpoint.github"],
    "node_modules/local-wrapper/src/auth.rs": SAMPLES["oauth.endpoint.google"],
    "src/main.rs": NEUTRAL,
  });
  try {
    const result = checkApp(root, shipped, { trust: { enrolled: true, trust: null } });
    assert.equal(result.ok, false);
    assert.ok(result.findings.some((f) => f.file === "node_modules/local-wrapper/src/auth.rs"), "installed source without provenance is scanned as a finding");
    assert.ok(result.coverage.some((x) => x.path === "node_modules" && x.reason === "node-modules-provenance-not-verified"));
    const inventory = contentInventory(root);
    assert.equal(inventory.complete, false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

function repositoryTrust(root, relPaths, kind = "third-party") {
  const identity = "https://github.com/org/repo";
  const files = relPaths.filter((rel) => { try { readFileSync(path.join(root, ...rel.split("/"))); return true; } catch { return false; } })
    .map((rel) => ({ path: rel, content: readFileSync(path.join(root, ...rel.split("/"))) }));
  const { trust } = signedException([], { root: identity });
  const requestId = "RK-PROVENANCE-001";
  trust.provenance = [{ repositoryIdentity: identity, paths: relPaths, contentDigest: provenanceContentDigest(files), kind, requestId }];
  return trust;
}

function setOrigin(root) {
  const git = (...a) => spawnSync("git", ["-C", root, ...a], { encoding: "utf8", windowsHide: true });
  assert.equal(git("init", "-q").status, 0);
  assert.equal(git("remote", "add", "origin", "git@github.com:Org/Repo.git").status, 0);
}

test("exact protected provenance classifies only matching bytes and records their digest", () => {
  const root = app({ "vendor/lib.rs": SAMPLES["oauth.endpoint.github"], "src/auth.rs": SAMPLES["oauth.endpoint.google"] });
  try {
    setOrigin(root);
    const trust = repositoryTrust(root, ["vendor/lib.rs"]);
    const result = checkApp(root, shipped, { trust });
    assert.ok(!result.findings.some((f) => f.file === "vendor/lib.rs"));
    assert.ok(result.findings.some((f) => f.file === "src/auth.rs"));
    assert.deepEqual(result.metadata.provenance, [{ kind: "third-party", requestId: "RK-PROVENANCE-001", contentDigest: trust.provenance[0].contentDigest, paths: ["vendor/lib.rs"] }]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("changed and missing protected provenance paths stay scanned or explicitly unscanned", () => {
  const root = app({ "vendor/lib.rs": SAMPLES["oauth.endpoint.github"] });
  try {
    setOrigin(root);
    const trust = repositoryTrust(root, ["vendor/lib.rs", "vendor/absent.rs"]);
    writeFileSync(path.join(root, "vendor/lib.rs"), SAMPLES["oauth.endpoint.google"]);
    const result = checkApp(root, shipped, { trust });
    assert.ok(result.findings.some((f) => f.file === "vendor/lib.rs"));
    assert.ok(result.coverage.some((x) => x.path === "vendor/lib.rs" && x.reason === "protected-provenance-stale-or-unreadable"));
    assert.ok(result.coverage.some((x) => x.path === "vendor/absent.rs" && x.reason === "protected-provenance-stale-or-unreadable"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("node_modules local wrapper source and manifest are scanned while unrelated transitives stay opaque", () => {
  const root = app({ "package.json": JSON.stringify({ dependencies: { wrapper: "file:node_modules/local-wrapper" } }), "node_modules/local-wrapper/package.json": JSON.stringify({ dependencies: { puppeteer: "22" } }), "node_modules/local-wrapper/src/auth.rs": SAMPLES["oauth.endpoint.github"], "node_modules/transitive/package.json": JSON.stringify({ dependencies: { puppeteer: "22" } }) });
  try {
    const result = checkApp(root, shipped, { trust: { enrolled: true, trust: null } });
    assert.ok(result.findings.some((f) => f.file === "node_modules/local-wrapper/src/auth.rs"));
    assert.ok(result.findings.some((f) => f.fingerprint === "browser.npm.puppeteer" && f.file === "node_modules/local-wrapper/package.json"));
    assert.ok(result.coverage.some((x) => x.path === "node_modules" && x.reason === "node-modules-provenance-not-verified"));
    assert.ok(directDependencies(root).some((d) => d.name === "puppeteer" && d.file === "node_modules/local-wrapper/package.json"));
    assert.ok(!directDependencies(root).some((d) => d.name === "puppeteer" && d.file === "node_modules/transitive/package.json"), "unrelated installed transitive manifests stay opaque");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("metadata repository identity uses approval canonicalization", () => {
  const root = app({ "src/main.rs": NEUTRAL });
  try {
    const git = (...a) => spawnSync("git", ["-C", root, ...a], { encoding: "utf8", windowsHide: true });
    assert.equal(git("init", "-q").status, 0);
    assert.equal(git("remote", "add", "origin", "git@github.com:Org/Repo.git").status, 0);
    const found = scanApp(root, shipped);
    assert.equal(found.metadata.repositoryIdentity, canonicalRepositoryIdentity("git@github.com:Org/Repo.git"));
    assert.equal(found.metadata.repositoryIdentity, "https://github.com/org/repo");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("nested scans use canonical repository paths and exact provenance stays within its prefix", () => {
  const root = app({ "engine/src/auth.rs": SAMPLES["oauth.endpoint.github"], "apps/src/auth.rs": SAMPLES["oauth.endpoint.google"] });
  try {
    setOrigin(root);
    const trust = repositoryTrust(root, ["engine/src/auth.rs"]);
    const nested = path.join(root, "engine"), other = path.join(root, "apps");
    const findings = scanApp(nested, shipped, { trust: { enrolled: true, trust: null } });
    assert.ok(findings.some((x) => x.file === "engine/src/auth.rs"));
    assert.equal(findings.metadata.pathBase, "repository");
    assert.equal(findings.metadata.pathPrefix, "engine/");
    assert.ok(findings.metadata.contentDigests.every((x) => x.path.startsWith("engine/")));
    const approved = checkApp(nested, shipped, { trust });
    assert.ok(!approved.findings.some((x) => x.file === "engine/src/auth.rs"));
    assert.ok(checkApp(other, shipped, { trust }).findings.some((x) => x.file === "apps/src/auth.rs"));
    const stale = repositoryTrust(root, ["engine/src/auth.rs", "engine/missing.rs"]);
    const staleResult = checkApp(nested, shipped, { trust: stale });
    assert.ok(staleResult.coverage.some((x) => x.path === "engine/src/auth.rs" && x.reason === "protected-provenance-stale-or-unreadable"));
    assert.ok(!staleResult.coverage.some((x) => x.path.startsWith("engine/engine/")));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("only Git-listed tracked/untracked source is scanned; ignored source & metadata are excluded", () => {
  const root = app({
    ".gitignore": "models/\n", "src/tracked.rs": SAMPLES["oauth.endpoint.github"],
    ".hidden/auth.rs": SAMPLES["oauth.endpoint.google"], "vendor/lib.rs": SAMPLES["secrets.api.wincred"],
    "generated/auth.swift": SAMPLES["secrets.swift.scoped-migration"], "models/cache.rs": SAMPLES["ort.env.dylib"],
    "nested/.git/HEAD": "ref: refs/heads/main\n", "nested/src/auth.rs": SAMPLES["oauth.endpoint.openai"],
  });
  try {
    const git = (...a) => spawnSync("git", ["-C", root, ...a], { windowsHide: true });
    assert.equal(git("init", "-q").status, 0); assert.equal(git("add", ".gitignore", "src/tracked.rs").status, 0);
    const found = scanApp(root, shipped);
    for (const rel of ["src/tracked.rs", ".hidden/auth.rs", "vendor/lib.rs", "generated/auth.swift", "nested/src/auth.rs"]) assert.ok(found.some((f) => f.file === rel), rel);
    assert.ok(!found.some((f) => f.file.includes(".git/") || f.file === "models/cache.rs"));
    assert.ok(found.metadata.commit === null || found.metadata.commit.length === 40);
    assert.equal(found.metadata.catalogDigest, catalogDigest(shipped));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("symlink escapes and opaque build caches are explicit incomplete coverage", () => {
  const root = app({ "target/generated.rs": SAMPLES["oauth.endpoint.github"], "src/oversize.rs": "x".repeat(2 * 1024 * 1024 + 1), "broken/Cargo.toml": "[dependencies\nwrong = \"1\"\n" });
  try {
    const inventory = contentInventory(root);
    assert.equal(inventory.complete, false);
    assert.ok(inventory.coverage.some((x) => x.path === "target" && x.status === "unscanned"));
    const result = checkApp(root, shipped, { trust: { enrolled: true, trust: null } });
    assert.equal(result.ok, false);
    assert.ok(result.rows.some((r) => r.status === "unscanned"));
    assert.ok(result.coverage.some((x) => x.path === "src/oversize.rs" && x.reason === "source-file-over-2MiB"));
    assert.ok(result.coverage.some((x) => x.path === "broken/Cargo.toml" && x.reason === "invalid-cargo-toml"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("plain exception entries (#262): a matching entry marks the finding excepted; malformed, legacy-signed and non-matching entries never cover; stale entries warn", () => {
  const findings = [{ fingerprint: "oauth.endpoint.github", capability: "oauth-sign-in", package: "rightkit-oauth", file: "engine/src/auth.rs", line: 1, lineText: SAMPLES["oauth.endpoint.github"], ordinal: 0, text: SAMPLES["oauth.endpoint.github"] }];
  const entry = { capability: "oauth-sign-in", package: "rightkit-oauth", reason: "bounded migration", owner: "CodeRight", date: "2026-10-17" };
  const staleWarnings = (r) => r.warnings.filter((w) => /stale/.test(w));
  // Matching entry: excepted, no signature, no trust, no approvedBy needed.
  let result = evaluate(findings, [{ ...entry, index: 0 }], {});
  assert.equal(result.covered.length, 1); assert.equal(result.violations.length, 0); assert.deepEqual(result.problems, []); assert.deepEqual(staleWarnings(result), []);
  assert.equal(result.covered[0].exception.owner, "CodeRight");
  // `crate` is an accepted alias for `package`; optional fingerprint and path narrow the match.
  const { package: owner, ...withoutPackage } = entry;
  assert.equal(evaluate(findings, [{ ...withoutPackage, crate: owner, index: 0 }], {}).covered.length, 1);
  assert.equal(evaluate(findings, [{ ...entry, fingerprint: "oauth.endpoint.github", path: "engine/src", index: 0 }], {}).covered.length, 1);
  result = evaluate(findings, [{ ...entry, fingerprint: "oauth.endpoint.google", index: 0 }], {});
  assert.equal(result.covered.length, 0); assert.equal(result.violations.length, 1); assert.equal(staleWarnings(result).length, 1);
  result = evaluate(findings, [{ ...entry, path: "apps/src", index: 0 }], {});
  assert.equal(result.covered.length, 0); assert.equal(staleWarnings(result).length, 1);
  // Stale entry (different capability/package): finding stays a violation, entry is a warning not a problem.
  result = evaluate(findings, [{ ...entry, package: "rightkit-browser", index: 0 }], {});
  assert.equal(result.covered.length, 0); assert.equal(result.violations.length, 1); assert.deepEqual(result.problems, []);
  assert.match(staleWarnings(result)[0], /exception #0.*stale, matches no current finding/);
  // A stale entry beside a live one reports only the stale one.
  result = evaluate(findings, [{ ...entry, index: 0 }, { ...entry, capability: "gone", index: 1 }], {});
  assert.equal(result.covered.length, 1); assert.equal(staleWarnings(result).length, 1); assert.match(staleWarnings(result)[0], /exception #1/);
  // Two entries for one finding cover it once.
  assert.equal(evaluate(findings, [{ ...entry, index: 0 }, { ...entry, index: 1 }], {}).covered.length, 1);
  // Malformed entries are problems and never cover.
  for (const [broken, pattern] of [
    [{ ...entry, reason: "" }, /missing reason/], [{ ...entry, owner: undefined }, /missing owner/], [{ ...entry, capability: undefined }, /missing capability/],
    [{ ...withoutPackage }, /missing package/], [{ ...entry, date: "2026-99-99" }, /date must be YYYY-MM-DD/], [{ ...entry, date: undefined }, /date must be YYYY-MM-DD/],
  ]) {
    result = evaluate(findings, [{ ...broken, index: 0 }], {});
    assert.equal(result.covered.length, 0); assert.match(result.problems[0], pattern);
  }
  // Legacy signed records are refused with a pointer to the plain format.
  result = evaluate(findings, [{ schema: 2, payload: { fingerprint: "oauth.endpoint.github" }, signature: null, index: 0 }], {});
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /legacy signed exception record/);
  // Enrollment state no longer affects whether an entry covers.
  result = evaluate(findings, [{ ...entry, index: 0 }], { trust: undefined });
  assert.equal(result.covered.length, 1);
});

test("sample source occurrence fails closed while enrollment is pending", () => {
  const root = app({ "engine/bins/coderight/src/github_signin.rs": `${SAMPLES["oauth.endpoint.github"]}\n` });
  try {
    const findings = scanApp(root, shipped);
    assert.ok(findings.some((f) => f.fingerprint === "oauth.endpoint.github" && f.file.endsWith("github_signin.rs")));
    const result = checkApp(root, shipped, { trust: { enrolled: false, pending: true, trust: null } });
    assert.equal(result.ok, false);
    assert.equal(result.enrollmentPending, true);
    assert.ok(result.violations.some((f) => f.fingerprint === "oauth.endpoint.github"));
    assert.ok(result.warnings.some((p) => /enrollment pending/.test(p)));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("clean RightKit dependency integration fixture fails closed pending enrollment with declaration-only status", () => {
  const root = app({ "Cargo.toml": '[package]\nname = "clean-consumer"\nversion = "0.1.0"\n[dependencies]\nrightkit-oauth = "0.2.0"\n' });
  try {
    const result = checkApp(root, shipped, { trust: { enrolled: false, pending: true, trust: null } });
    assert.equal(result.ok, false);
    assert.equal(result.violations.length, 0);
    assert.equal(result.problems.length, 0);
    assert.equal(result.enrollmentPending, true);
    assert.ok(result.warnings.some((p) => /enrollment pending/.test(p)));
    assert.equal(result.rows.find((r) => r.capability === "oauth-sign-in").status, "declared-dependency");
    assert.ok(Array.isArray(result.metadata.contentDigests));
    assert.ok(result.metadata.contentDigests.some((x) => x.path === "Cargo.toml"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("blob scan cache preserves findings, rebases paths & invalidates for dirty input or catalog changes", () => {
  const root = app({ "src/auth.rs": SAMPLES["oauth.endpoint.github"], "other/auth.rs": SAMPLES["oauth.endpoint.github"] });
  const git = (...args) => spawnSync("git", ["-C", root, ...args], { windowsHide: true });
  try {
    assert.equal(git("add", ".").status, 0);
    const first = scanApp(root, shipped), warm = scanApp(root, shipped);
    assert.deepEqual(warm, first);
    assert.deepEqual(first.map((f) => f.file).sort(), ["other/auth.rs", "src/auth.rs"]);
    assert.ok(warm.metadata.contentDigests.every((entry) => entry.digest.startsWith("git:")));
    writeFileSync(path.join(root, "src/auth.rs"), SAMPLES["oauth.endpoint.google"]);
    const dirty = scanApp(root, shipped);
    assert.ok(dirty.some((f) => f.file === "src/auth.rs" && f.fingerprint === "oauth.endpoint.google"));
    assert.ok(!dirty.some((f) => f.file === "src/auth.rs" && f.fingerprint === "oauth.endpoint.github"));
    const changed = structuredClone(shipped);
    changed.capabilities.flatMap((c) => c.fingerprints).find((f) => f.id === "oauth.endpoint.google").pattern = "NEVER_MATCH_THIS_CATALOG";
    assert.ok(!scanApp(root, changed).some((f) => f.fingerprint === "oauth.endpoint.google"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("combined prefilter preserves per-line anchors, flags & backreferences", () => {
  const manifest = { capabilities: [{ package: "rightkit-test", capability: "test", fingerprints: [
    { id: "anchored", type: "source", pattern: "^credwritew$", flags: "i" },
    { id: "backreference", type: "source", pattern: "^(Cred)\\1$" },
  ] }] };
  const root = app({ "main.rs": "neutral\nCredWriteW\nCredCred\n//CredWriteW\n" });
  try {
    assert.deepEqual(scanApp(root, manifest).map((f) => [f.fingerprint, f.line]), [["anchored", 2], ["backreference", 3]]);
    manifest.capabilities[0].fingerprints.pop();
    assert.deepEqual(scanApp(root, manifest).map((f) => [f.fingerprint, f.line]), [["anchored", 2]]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("committed scanner verifies pinned key log from Git blobs & ignores forged worktree log", () => {
  const root = app({ "source.rs": "fn main() {}\n" });
  const runGit = (...args) => {
    const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  try {
    runGit("config", "user.email", "test@example.invalid");
    runGit("config", "user.name", "test");
    runGit("remote", "add", "origin", "https://github.com/test/repo.git");
    const pair = generateKeyPairSync("rsa", { modulusLength: 2048, publicExponent: 65537 });
    const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
    const key = { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform: "windows", deviceName: "root" };
    const draft = createKeyEvent({ schema: 1, events: [] }, { operation: "root", subject: key, signerKeyId: key.keyId });
    const log = { schema: 1, events: [draft] };
    mkdirSync(path.join(root, ".rightkit"), { recursive: true });
    writeFileSync(path.join(root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(log)}\n`);
    runGit("add", ".");
    runGit("commit", "-qm", "pinned key log");
    const trust = { schema: 1, rootFingerprint: key.keyId, keyCheckpoint: { sequence: 0, head: eventHash(draft) }, revokedKeyIds: [], hosts: {}, verifiedTrackingEvidence: [], minimumScannerVersion: "0.0.0", minimumCatalogVersion: "0.0.0", repositories: [{ repositoryIdentity: "https://github.com/test/repo", roots: ["."], visibility: "private" }], provenance: [] };
    const committedBytes = Buffer.from(`${JSON.stringify(log)}\n`);
    const trustFile = path.join(root, ".git", "host-state", "trust.json");
    mkdirSync(path.dirname(trustFile));
    writeFileSync(trustFile, JSON.stringify(trust));
    authorityTestOnly.persistCheckpoint(trust, committedBytes, trustFile, () => {});
    const readAuthorityForTest = (pin) => authorityTestOnly.readAuthority(pin, { file: trustFile, audit: () => {} });
    const manifest = { schema: 1, version: "1", capabilities: [] };
    assert.equal(checkAppForTest(root, manifest, { trust, readAuthorityForTest }).enrollmentPending, true);
    assert.equal(scanAppForTest(root, manifest, { trust, readAuthorityForTest }).metadata.keyLogDigest, sha256(committedBytes));
    writeFileSync(path.join(root, ".rightkit", "ownership-keys.json"), '{"schema":1,"events":[]}\n');
    const scanned = checkCommittedAppForTest(root, manifest, { trust });
    assert.equal(scanned.enrollmentPending, true);
    assert.equal(scanned.ok, false);
    assert.match(scanned.warnings.join(" "), /enrollment pending/);
    assert.equal(scanned.metadata.rootFingerprint, key.keyId);
    assert.equal(scanned.metadata.keyCheckpoint.head, eventHash(draft));
    assert.equal(scanned.metadata.keyLogDigest, sha256(committedBytes));
    const forgedDoctor = checkAppForTest(root, manifest, { trust, readAuthorityForTest });
    assert.equal(forgedDoctor.enrollmentPending, true);
    assert.equal(forgedDoctor.ok, false);
    assert.equal(forgedDoctor.metadata.keyLogDigest, sha256(committedBytes));
    rmSync(path.join(root, ".rightkit", "ownership-keys.json"));
    const deletedDoctor = checkAppForTest(root, manifest, { trust, readAuthorityForTest });
    assert.equal(deletedDoctor.enrollmentPending, true);
    assert.equal(deletedDoctor.ok, false);
    assert.equal(deletedDoctor.metadata.keyLogDigest, sha256(committedBytes));
    let injected = false;
    const forbiddenReader = () => { injected = true; throw new Error("production must not call test reader"); };
    const malformedAuthority = { ...trust, keyAuthority: { repositoryIdentity: "not-canonical", checkoutPath: root } };
    for (const production of [checkApp, scanApp]) {
      assert.throws(() => production(root, manifest, { trust: malformedAuthority, readAuthorityForTest: forbiddenReader, trustPath: trustFile }), /protected ownership key authority failed:.*identity/);
    }
    assert.equal(injected, false);
    assert.throws(() => checkCommittedAppForTest(root, manifest, { trust: { ...trust, rootFingerprint: "f".repeat(64) } }), /key log failed/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("one-key enrollment fails closed with backlog; both distinct keys enforce findings", () => {
  const root = app({ "src/auth.rs": SAMPLES["oauth.endpoint.github"] });
  try {
    const { trust } = signedException([], { root: "https://github.com/org/repo" });
    const pending = structuredClone(trust);
    delete pending.hosts.macos;
    const advisory = checkApp(root, shipped, { trust: pending });
    assert.equal(advisory.ok, false);
    assert.equal(advisory.enrollmentPending, true);
    assert.equal(advisory.violations.length, 1);
    assert.match(advisory.warnings[0], /enrollment pending/);
    const enforced = checkApp(root, shipped, { trust });
    assert.equal(enforced.ok, false);
    assert.equal(enforced.enrollmentPending, false);
    assert.deepEqual(enforced.warnings, []);
    const reused = structuredClone(trust);
    reused.hosts.macos = reused.hosts.windows;
    assert.equal(checkApp(root, shipped, { trust: reused }).enrollmentPending, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

import { stripSwiftComments } from "./ownership.mjs";
test("Swift manifest comments never declare dependencies; URLs and escaped quotes survive", () => {
  const src = `deps: [\n  .package(url: "https://github.com/a/b", from: "1.0.0"), // trailing\n  // .package(url: "https://github.com/stackotter/TermKit", revision: "x"),\n  /* .package(url: "https://x/y", /* nested */ from: "2") */\n  .package(url: "https://e.com/q\\"//", exact: "3")\n]`;
  const out = stripSwiftComments(src);
  assert.equal(out.length, src.length);
  assert.match(out, /https:\/\/github.com\/a\/b/);
  assert.doesNotMatch(out, /TermKit/);
  assert.doesNotMatch(out, /x\/y/);
  assert.match(out, /https:\/\/e.com\/q\\"\/\//);
  assert.equal((out.match(/\.package\(/g) ?? []).length, 2);
});
