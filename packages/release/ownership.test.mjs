import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  aggregateOwnership, checkApp, directDependencies, evaluate, MANIFEST_FILE, scanApp,
} from "./ownership.mjs";

const rightkitRoot = path.resolve(import.meta.dirname, "../..");
const shipped = JSON.parse(readFileSync(path.join(import.meta.dirname, MANIFEST_FILE), "utf8"));

// One real-world line per source fingerprint. A new source fingerprint must add a sample here,
// so a pattern that matches nothing (or everything) cannot ship.
const SAMPLES = {
  "oauth.endpoint.github": 'const URL: &str = "https://github.com/login/device/code";',
  "oauth.endpoint.google": 'let u = "https://oauth2.googleapis.com/device/code";',
  "oauth.endpoint.openai": 'const AUTH = "https://auth.openai.com/oauth/authorize";',
  "oauth.endpoint.apple": 'let url = URL(string: "https://appleid.apple.com/auth/authorize")!',
  "oauth.endpoint.cloudflare": 'const u = "https://dash.cloudflare.com/oauth2/auth";',
  "oauth.device-grant": 'grant_type: "urn:ietf:params:oauth:grant-type:device_code",',
  "secrets.api.wincred": "let ok = unsafe { CredWriteW(&cred, 0) };",
  "secrets.api.secitem": "let status = SecItemCopyMatching(query as CFDictionary, &item)",
  "sandbox.api.appcontainer": "unsafe { CreateAppContainerProfile(name, display, desc, caps, 0, &mut sid) }",
  "sandbox.api.seatbelt": "let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut err) };",
  "process.api.kill-on-job-close": "info.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;",
  "ort.env.dylib": 'std::env::var("ORT_DYLIB_PATH")',
  "mcp.protocol.initialized": 'self.notify("notifications/initialized", json!({})).await?;',
};
const NEUTRAL = 'let greeting = "hello from an app that owns nothing";';

function app(files) {
  const root = mkdtempSync(path.join(tmpdir(), "rk-ownership-"));
  for (const [rel, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    writeFileSync(path.join(root, rel), text);
  }
  return root;
}

test("the shipped manifest is exactly the aggregate of every package's declaration", () => {
  assert.deepEqual(shipped, aggregateOwnership(rightkitRoot), "run: right-release ownership --write-manifest");
});

test("every source fingerprint matches its sample, and nothing matches a neutral line", () => {
  for (const c of shipped.capabilities) {
    for (const f of c.fingerprints.filter((x) => x.type === "source")) {
      assert.ok(SAMPLES[f.id], `add a sample for ${f.id}`);
      const re = new RegExp(f.pattern, f.flags ?? "");
      assert.ok(re.test(SAMPLES[f.id]), `${f.id} misses its sample`);
      assert.ok(!re.test(NEUTRAL), `${f.id} matches a neutral line`);
    }
  }
});

test("direct dependencies are read from Cargo, npm and SwiftPM manifests, not lockfiles", () => {
  const root = app({
    "Cargo.toml": '[package]\nname = "x"\n[dependencies]\noauth2 = "4"\n[target.\'cfg(windows)\'.dependencies]\nkeyring = "3"\n[dependencies.notify]\nversion = "6"\n',
    "Cargo.lock": '[[package]]\nname = "chromiumoxide"\n',
    "web/package.json": JSON.stringify({ dependencies: { keytar: "7" }, devDependencies: { puppeteer: "22" } }),
    "ios/Package.swift": '.package(url: "https://github.com/example/SomeKit.git", from: "1.0.0")',
    "node_modules/x/package.json": JSON.stringify({ dependencies: { chokidar: "3" } }),
  });
  try {
    const names = directDependencies(root).map((d) => `${d.ecosystem}:${d.name}`).sort();
    assert.deepEqual(names, ["cargo:keyring", "cargo:notify", "cargo:oauth2", "npm:keytar", "npm:puppeteer", "swiftpm:SomeKit"]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("an app's own copies are reported; comments, vendored and nested repos are not", () => {
  const root = app({
    "Cargo.toml": '[dependencies]\noauth2 = "4"\nrightkit-watch = "0.1"\n',
    "src/auth.rs": `${SAMPLES["oauth.endpoint.github"]}\n// ${SAMPLES["oauth.endpoint.google"]}\n${NEUTRAL}\n`,
    "vendor/lib.rs": SAMPLES["secrets.api.wincred"],
    "nested/.git/HEAD": "ref: refs/heads/main\n",
    "nested/src/x.rs": SAMPLES["secrets.api.wincred"],
    "docs/notes.md": SAMPLES["oauth.endpoint.openai"],
  });
  try {
    const found = scanApp(root, shipped).map((f) => `${f.fingerprint}@${f.file}`).sort();
    assert.deepEqual(found, ["oauth.cargo.oauth2@Cargo.toml", "oauth.endpoint.github@src/auth.rs"]);
    const result = checkApp(root, shipped);
    assert.equal(result.ok, false);
    const row = (cap) => result.rows.find((r) => r.capability === cap).status;
    assert.match(row("oauth-sign-in"), /OWN COPY, NOT EXCEPTED \(2\)/);
    assert.equal(row("file-watching"), "uses RightKit");
    assert.equal(row("browser-automation"), "not needed");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("in a git repository, ignored trees are not scanned and untracked files are", () => {
  const root = app({
    ".gitignore": "models/\n",
    "src/tracked.rs": SAMPLES["secrets.api.wincred"],
    "src/new.rs": SAMPLES["sandbox.api.seatbelt"],
    "models/cache.rs": SAMPLES["ort.env.dylib"],
  });
  try {
    const git = (...a) => spawnSync("git", ["-C", root, ...a], { windowsHide: true });
    assert.equal(git("init", "-q").status, 0);
    assert.equal(git("add", "src/tracked.rs", ".gitignore").status, 0);
    const found = scanApp(root, shipped).map((f) => `${f.fingerprint}@${f.file}`).sort();
    assert.deepEqual(found, ["sandbox.api.seatbelt@src/new.rs", "secrets.api.wincred@src/tracked.rs"]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("exceptions cover only what they name, and expired, stale or wildcard ones fail", () => {
  const findings = [
    { fingerprint: "oauth.cargo.oauth2", capability: "oauth-sign-in", file: "engine/Cargo.toml" },
    { fingerprint: "oauth.endpoint.github", capability: "oauth-sign-in", file: "engine/src/auth.rs" },
  ];
  const base = { reason: "migrating", owner: "CodeRight", approvedBy: "Adrian", expires: "2026-12-31" };
  const live = { ...base, index: 0, fingerprint: "oauth.cargo.oauth2", paths: ["engine/Cargo.toml"] };
  let r = evaluate(findings, [live], { today: "2026-10-07" });
  assert.equal(r.covered.length, 1);
  assert.deepEqual(r.violations.map((v) => v.fingerprint), ["oauth.endpoint.github"]);
  assert.deepEqual(r.problems, []);

  r = evaluate(findings, [{ ...live, expires: "2026-10-01" }], { today: "2026-10-07" });
  assert.match(r.problems[0], /expired/);
  r = evaluate(findings, [{ ...live, fingerprint: "secrets.npm.keytar" }], { today: "2026-10-07" });
  assert.match(r.problems[0], /stale/);
  r = evaluate(findings, [{ ...live, paths: ["**"] }], { today: "2026-10-07" });
  assert.match(r.problems[0], /wildcard/);
  r = evaluate(findings, [{ ...live, approvedBy: "" }], { today: "2026-10-07" });
  assert.match(r.problems[0], /missing approvedBy/);
  r = evaluate(findings, [{ ...live, expires: undefined, tracking: "rightkit-oauth 0.2 cutover" }], { today: "2026-10-07" });
  assert.equal(r.covered.length, 1);
});
