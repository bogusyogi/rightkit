import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { canonicalJson, canonicalRepositoryIdentity, createDraft, keyIdForPublicKey, provenanceContentDigest, sha256 } from "./ownership-approval.mjs";
import { aggregateOwnership, catalogDigest, checkApp, contentInventory, directDependencies, evaluate, MANIFEST_FILE, scanApp } from "./ownership.mjs";

const rightkitRoot = path.resolve(import.meta.dirname, "../..");
const shipped = JSON.parse(readFileSync(path.join(import.meta.dirname, MANIFEST_FILE), "utf8"));
const SAMPLES = {
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
  "ort.env.dylib": 'std::env::var("ORT_DYLIB_PATH")',
  "mcp.protocol.initialized": 'self.notify("notifications/initialized", json!({})).await?;',
};
const NEUTRAL = "let greeting = 'hello';";

function app(files = {}) {
  const root = mkdtempSync(path.join(tmpdir(), "rk-ownership-"));
  for (const [rel, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    writeFileSync(path.join(root, rel), text);
  }
  return root;
}

function signedException(findings, { root, fingerprint, file, today = "2026-10-07", agentField = false } = {}) {
  const selected = findings.filter((f) => f.fingerprint === fingerprint && f.file === file);
  const draft = createDraft({ repositoryIdentity: root, catalogDigest: catalogDigest(shipped), fingerprint,
    findings: selected, paths: [file], reason: "bounded migration", owner: "CodeRight", expires: "2026-12-31",
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
  assert.deepEqual(shipped, aggregateOwnership(rightkitRoot));
  assert.ok(shipped.capabilities.flatMap((c) => c.fingerprints).some((f) => f.id === "secrets.swift.scoped-migration"));
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

test("ignored, hidden, vendored, generated & nested-repository source is scanned; metadata is excluded", () => {
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
    for (const rel of ["src/tracked.rs", ".hidden/auth.rs", "vendor/lib.rs", "generated/auth.swift", "nested/src/auth.rs", "models/cache.rs"]) assert.ok(found.some((f) => f.file === rel), rel);
    assert.ok(!found.some((f) => f.file.includes(".git/")));
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
    const result = checkApp(root, shipped);
    assert.equal(result.ok, false);
    assert.ok(result.rows.some((r) => r.status === "unscanned"));
    assert.ok(result.coverage.some((x) => x.path === "src/oversize.rs" && x.reason === "source-file-over-2MiB"));
    assert.ok(result.coverage.some((x) => x.path === "broken/Cargo.toml" && x.reason === "invalid-cargo-toml"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("unsigned, forged, broad, expired & stale exceptions never cover; valid signed exact baseline does", () => {
  const findings = [{ fingerprint: "oauth.endpoint.github", capability: "oauth-sign-in", package: "rightkit-oauth", file: "engine/src/auth.rs", line: 1, text: SAMPLES["oauth.endpoint.github"] }];
  const metadata = { repositoryIdentity: "https://github.com/org/repo", catalogDigest: catalogDigest(shipped), scannerVersion: "2.0.0", catalogVersion: "1" };
  let result = evaluate(findings, [{ fingerprint: findings[0].fingerprint, paths: [findings[0].file], approvedBy: "agent" }], { metadata, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.ok(result.problems.length);
  const { record, trust } = signedException(findings, { root: metadata.repositoryIdentity, fingerprint: findings[0].fingerprint, file: findings[0].file });
  result = evaluate(findings, [record], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 1); assert.equal(result.violations.length, 0); assert.deepEqual(result.problems, []);
  result = evaluate(findings, [{ ...record, approvedBy: "agent" }], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /approvedBy must be Adrian/);
  result = evaluate(findings, [{ ...record, payload: { ...record.payload, approvedBy: "agent" } }], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /approvedBy must be Adrian/);
  result = evaluate(findings, [{ ...record, payload: { ...record.payload, paths: ["**"] } }], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /exact repository-relative/);
  result = evaluate(findings, [{ ...record, payload: { ...record.payload, expires: "" } }], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /missing expires/);
  result = evaluate(findings, [{ ...record, payload: { ...record.payload, expires: "2026-99-99" } }], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /invalid or expired expires/);
  result = evaluate(findings, [{ ...record, payload: { ...record.payload, trackingRef: "issue RK-123456" } }], { metadata, trust, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.match(result.problems[0], /tracking must be a verified/);
  result = evaluate(findings, [record], { metadata, trust: undefined, today: "2026-10-07" });
  assert.equal(result.covered.length, 0); assert.ok(result.problems.some((p) => /enrollment pending/.test(p)));
});

test("CodeRight device-flow endpoint is a finding and untrusted backlog cannot pass", () => {
  const root = app({ "engine/bins/coderight/src/github_signin.rs": 'let url = "https://github.com/login/device/code";\n' });
  try {
    const findings = scanApp(root, shipped);
    assert.ok(findings.some((f) => f.fingerprint === "oauth.endpoint.github" && f.file.endsWith("github_signin.rs")));
    const result = checkApp(root, shipped);
    assert.equal(result.ok, false);
    assert.ok(result.violations.some((f) => f.fingerprint === "oauth.endpoint.github"));
    assert.ok(result.problems.some((p) => /enrollment pending/.test(p)));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("clean RightKit dependency integration fixture passes with declaration-only status", () => {
  const root = app({ "Cargo.toml": '[package]\nname = "clean-consumer"\nversion = "0.1.0"\n[dependencies]\nrightkit-oauth = "0.2.0"\n' });
  try {
    const result = checkApp(root, shipped);
    assert.equal(result.ok, true);
    assert.equal(result.violations.length, 0);
    assert.equal(result.problems.length, 0);
    assert.equal(result.rows.find((r) => r.capability === "oauth-sign-in").status, "declared-dependency");
    assert.ok(Array.isArray(result.metadata.contentDigests));
    assert.ok(result.metadata.contentDigests.some((x) => x.path === "Cargo.toml"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});
