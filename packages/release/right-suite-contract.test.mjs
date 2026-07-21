import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import test, { after } from "node:test";
import {
  assertNoRightKitCargoOverrides,
  assertPublishedRightKitCargoDependencies,
  validateRightKitCargoContract,
} from "./cargo-contract.mjs";
import { assertAsrAdapterPair } from "./asr-artifact-adoption.mjs";
import { resolveConfiguredBuildInputs } from "./release-invocation.mjs";

const workspace = path.resolve(
  process.env.RIGHT_SUITE_CONTRACT_WORKSPACE
    ?? new URL("../../../..", import.meta.url).pathname.replace(/^\/(\w:)/, "$1"),
);
// The Right Suite web layer's repo + filesystem path is `rightsites` post the 2026-07-15 naming lock;
// a not-yet-renamed checkout may still have `rightapps`. Resolve whichever exists so the contract
// passes on both. (The runtime service namespace stays `rightapps` — that is deliberately untouched.)
const siteRepoDir = existsSync(path.join(workspace, "rightsites")) ? "rightsites" : "rightapps";
const versionsPath = path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, "$1")), "rightkit-versions.json");
const versions = JSON.parse(readFileSync(versionsPath, "utf8"));
const isolatedCargoHome = mkdtempSync(path.join(tmpdir(), "rightkit-cargo-home-"));
after(() => rmSync(isolatedCargoHome, { recursive: true, force: true }));
const pubkey = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDI5Mzk1RjlGRjQ2NjI2MUQKUldRZEptYjBuMTg1S1VSUXlBdFM4WmtzaHArYko0U2hRMDVlSDJmSExVZG82Q0hoQ2srUlhqanAK";
const apps = [
  { key: "viewright", root: "viewright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/build-mac-notarized.sh"] },
  { key: "scraperight", root: "scraperight", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["package.sh"] },
  { key: "heardright", root: "heardright/tauri-app-next", repoRoot: "heardright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs", "scripts/publish-release.mjs"] },
  // mac-dmg.mjs is the release file, as in HeardRight/CodeRight. build-mac.sh only delegates to it,
  // so asserting the mirror call against the wrapper failed a pipeline that does mirror.
  { key: "mailright", root: "mailright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs"] },
  { key: "coderight", root: "coderight/apps/coderight-tauri", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs"] },
];

function assertReleasePackageScripts(scripts, label) {
  for (const platform of ["mac", "win"]) {
    assert.equal(scripts[`release:build:${platform}`], `right-release build --platform ${platform}`, `${label} must use the tier-neutral ${platform} build entry point`);
    for (const tier of ["patch", "update"]) {
      assert.equal(scripts[`release:upload:${tier}:${platform}`], `right-release upload --platform ${platform} --tier ${tier}`, `${label} must select ${tier} only during ${platform} upload`);
    }
  }

  const genericTauri = scripts.tauri;
  if (typeof genericTauri === "string") {
    assert.doesNotMatch(
      genericTauri.trim(),
      /^(?:(?:pnpm|npm)\s+(?:(?:exec|dlx)\s+)?|npx\s+)?(?:(?:\.\/)?node_modules\/\.bin\/)?(?:@tauri-apps\/cli\s+)?tauri(?:\s|$)/,
      `${label} generic \"tauri\" package script bypasses the RightKit release entry points; use an explicitly named dev, debug, unsigned, or local command instead`,
    );
  }
}

function assertMacPackageEntry(packageCommand, scripts, label) {
  assert.equal(packageCommand.cmd, "pnpm", `${label} must use the shared pnpm package entry point`);
  assert.deepEqual(packageCommand.args, ["run", "rightkit:package:mac"], `${label} R2 release must use the dedicated signed and notarized macOS package entry point`);
  const implementation = scripts["rightkit:package:mac"];
  assert.equal(typeof implementation, "string", `${label} must define rightkit:package:mac`);
  assert.doesNotMatch(implementation, /\bright-release\b/, `${label} rightkit:package:mac must package directly instead of recursing into the release build`);
  assert.doesNotMatch(implementation, /\bpnpm(?:\s+run)?\s+rightkit:package:mac\b/, `${label} rightkit:package:mac must not recurse into itself`);
}

function assertBuildInputs(config, target, label) {
  resolveConfiguredBuildInputs(config, target, label);
}

function resolveCargoVersionContract(versionManifest, canonicalVersions) {
  const consumer = new Map(Object.entries(versionManifest.cargo ?? {}));
  const staged = new Map(Object.entries(versionManifest.stagedCargo ?? {}));
  const canonical = new Map();

  for (const [crate, version] of staged) {
    if (!canonicalVersions.has(crate)) throw new Error(`unknown staged Cargo crate: ${crate}`);
    const actual = canonicalVersions.get(crate);
    if (actual !== version) {
      throw new Error(`${crate} canonical manifest is ${actual}, expected staged version ${version}`);
    }
    canonical.set(crate, version);
  }

  for (const [crate, version] of consumer) {
    if (!canonicalVersions.has(crate)) throw new Error(`unknown published Cargo crate: ${crate}`);
    const expected = staged.get(crate) ?? version;
    const actual = canonicalVersions.get(crate);
    if (actual !== expected) {
      const source = staged.has(crate) ? "staged" : "published";
      throw new Error(`${crate} canonical manifest is ${actual}, expected ${source} version ${expected}`);
    }
    canonical.set(crate, expected);
  }

  return { canonical, consumer };
}

let currentCargoVersionContract;
function getCurrentCargoVersionContract() {
  currentCargoVersionContract ??= resolveCargoVersionContract(versions, readCanonicalCrateVersions());
  return currentCargoVersionContract;
}

const cargoContractCache = new Map();
function readCargoManifestContract(manifestPath, label) {
  const cacheKey = path.resolve(manifestPath).toLowerCase();
  if (cargoContractCache.has(cacheKey)) return cargoContractCache.get(cacheKey);
  const result = spawnSync(
    "cargo",
    ["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path", manifestPath],
    {
      cwd: path.dirname(manifestPath),
      encoding: "utf8",
      env: { ...process.env, CARGO_HOME: isolatedCargoHome },
      windowsHide: true,
    },
  );
  assert.equal(
    result.status,
    0,
    `${label} Cargo metadata rejected manifest; RightKit dependencies must use exact crates.io versions: ${String(result.stderr ?? "").trim()}`,
  );
  const metadata = JSON.parse(result.stdout);
  const expectedPath = path.resolve(manifestPath).toLowerCase();
  const pkg = metadata.packages.find(({ manifest_path: candidate }) => path.resolve(candidate).toLowerCase() === expectedPath);
  if (!pkg && path.resolve(metadata.workspace_root, "Cargo.toml").toLowerCase() === expectedPath) {
    const contract = { dependencies: [], workspaceRoot: path.resolve(metadata.workspace_root) };
    cargoContractCache.set(cacheKey, contract);
    return contract;
  }
  assert.ok(pkg, `${label} was not returned by Cargo metadata`);
  const contract = { dependencies: pkg.dependencies, workspaceRoot: path.resolve(metadata.workspace_root) };
  cargoContractCache.set(cacheKey, contract);
  return contract;
}

function assertCargoFixture(manifest, published, label) {
  const fixtureRoot = mkdtempSync(path.join(tmpdir(), "rightkit-cargo-contract-"));
  const manifestPath = path.join(fixtureRoot, "Cargo.toml");
  const packageMetadata = /^\s*\[\s*package\s*\]/m.test(manifest)
    ? ""
    : '\n[package]\nname = "rightkit-contract-fixture"\nversion = "0.0.0"\nedition = "2021"\n';
  writeFileSync(manifestPath, `${manifest}${packageMetadata}\n[lib]\npath = "fixture.rs"\n`, "utf8");
  writeFileSync(path.join(fixtureRoot, "fixture.rs"), "", "utf8");
  try {
    return assertPublishedRightKitCargoDependencies(
      readCargoManifestContract(manifestPath, label).dependencies,
      published,
      label,
    );
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
}

function assertCargoOverrideFixture({ manifest, workspaceManifest, localConfig, ancestorConfig, outsideConfig }) {
  const fixtureRoot = mkdtempSync(path.join(tmpdir(), "rightkit-cargo-override-"));
  const repoRoot = path.join(fixtureRoot, "repo");
  const appRoot = path.join(repoRoot, "app");
  mkdirSync(appRoot, { recursive: true });
  const manifestPath = path.join(appRoot, "Cargo.toml");
  writeFileSync(manifestPath, manifest, "utf8");
  if (workspaceManifest) writeFileSync(path.join(repoRoot, "Cargo.toml"), workspaceManifest, "utf8");
  for (const [root, filename, source] of [
    [appRoot, "config", localConfig],
    [repoRoot, "config.toml", ancestorConfig],
    [fixtureRoot, "config.toml", outsideConfig],
  ]) {
    if (!source) continue;
    const cargoDir = path.join(root, ".cargo");
    mkdirSync(cargoDir, { recursive: true });
    writeFileSync(path.join(cargoDir, filename), source, "utf8");
  }
  try {
    assertNoRightKitCargoOverrides(manifestPath, repoRoot, "fixture/Cargo.toml");
    if (workspaceManifest) {
      assertNoRightKitCargoOverrides(path.join(repoRoot, "Cargo.toml"), repoRoot, "fixture/workspace Cargo.toml");
    }
    return 0;
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
}

test("Cargo override contract accepts exact registry pins without repo-local overrides", () => {
  assert.equal(assertCargoOverrideFixture({
    manifest: `[package]\nname = "fixture"\nversion = "0.0.0"\n[dependencies]\nrightkit-license = "=0.1.1"`,
    localConfig: `[net]\nretry = 2\n[registries.private]\nindex = "https://example.test/index"`,
    outsideConfig: `[source.crates-io]\nreplace-with = "user-global-vendor"`,
  }), 0);
});

test("Cargo override contract rejects RightKit patches and replacements in repo-local Cargo config", () => {
  for (const config of [
    { localConfig: `[patch.crates-io]\nrightkit-license = { path = "../rightkit-license" }` },
    { localConfig: `patch . "https://github.com/rust-lang/crates.io-index" . "rightkit-logs" = { git = "https://example.test/rightkit-logs.git" }` },
    { localConfig: `[ replace ]\n"rightkit-license:0.1.1" = { path = "../rightkit-license" }` },
    { ancestorConfig: `[patch.private-source]\nadapter = { package = "rightkit-license", registry = "private" }` },
  ]) {
    assert.throws(
      () => assertCargoOverrideFixture({
        manifest: `[package]\nname = "fixture"\nversion = "0.0.0"\n[dependencies]\nrightkit-license = "=0.1.1"`,
        ...config,
      }),
      /forbidden RightKit Cargo override/,
    );
  }
});

test("Cargo override contract rejects path patches, Git patches, and replace entries", () => {
  for (const manifest of [
    `patch . "crates-io" . "rightkit-license" = { path = "../rightkit-license" } # dotted path patch`,
    `[ patch . "https://github.com/rust-lang/crates.io-index" ]\n"rightkit-logs" = { git = "https://example.test/rightkit-logs.git" }`,
    `[ replace ]\n"rightkit-license:0.1.1" = { path = "../rightkit-license" }`,
  ]) {
    assert.throws(
      () => assertCargoOverrideFixture({ manifest }),
      /forbidden RightKit Cargo override/,
    );
  }
});

test("Cargo override contract rejects workspace-root patches inherited by a member", () => {
  assert.throws(
    () => assertCargoOverrideFixture({
      manifest: `[package]\nname = "fixture"\nversion = "0.0.0"`,
      workspaceManifest: `[workspace]\nmembers = ["app"]\n[patch.crates-io]\nrightkit-license = { path = "patched/rightkit-license" }`,
    }),
    /forbidden RightKit Cargo override/,
  );
});

test("Cargo override contract rejects local and repo-ancestor crates.io source replacement", () => {
  for (const fixture of [
    {
      localConfig: `source . crates-io . replace-with = "vendored-sources"\n[source.vendored-sources]\ndirectory = "vendor"`,
    },
    {
      ancestorConfig: `[source."crates-io"]\nreplace-with = "rightkit-git"\n[source.rightkit-git]\ngit = "https://example.test/rightkit-index.git"`,
    },
    {
      ancestorConfig: `[source.crates-io]\nreplace-with = "private-registry"\n[source.private-registry]\nregistry = "https://example.test/index"`,
    },
  ]) {
    assert.throws(
      () => assertCargoOverrideFixture({
        manifest: `[package]\nname = "fixture"\nversion = "0.0.0"\n[dependencies]\nrightkit-license = "=0.1.1"`,
        ...fixture,
      }),
      /forbidden crates\.io source replacement/,
    );
  }
});

test("direct Cargo dependency contract accepts only published exact RightKit pins", () => {
  const published = new Map([
    ["rightkit-license", "0.1.2"],
    ["rightkit-logs", "0.1.0"],
    ["rightkit-tauri", "0.1.0"],
  ]);
  const manifest = `
[ package ]
name = "rightkit-tauri"
# rightkit-tauri = "=0.1.0"

[ dependencies ]
"rightkit-license" = "=0.1.2" # published exact pin
"rightkit-tauri" = "=0.1.0"

[ target . 'cfg(windows)' . dependencies . "rightkit-logs" ]
version = "=0.1.0"

[ target . 'cfg(unix)' . dev-dependencies ]
license-adapter = { package = "rightkit-license", version = "=0.1.2" }
`;

  assert.equal(assertCargoFixture(manifest, published, "fixture/Cargo.toml"), 4);
});

test("direct Cargo dependency contract rejects staged-only RightKit crates", () => {
  const { consumer } = resolveCargoVersionContract(
    {
      cargo: { "rightkit-license": "0.1.2" },
      stagedCargo: { "rightkit-tauri": "0.1.0" },
    },
    new Map([
      ["rightkit-license", "0.1.2"],
      ["rightkit-tauri", "0.1.0"],
    ]),
  );
  assert.equal(consumer.has("rightkit-tauri"), false);
  assert.throws(
    () => assertCargoFixture(
      `dependencies . "rightkit-tauri" = "=0.1.0" # staged only`,
      consumer,
      "fixture/Cargo.toml",
    ),
    /fixture\/Cargo\.toml rightkit-tauri is not a published RightKit crate/,
  );
});

test("direct Cargo dependency contract still rejects path, Git, and custom registry sources", () => {
  const published = new Map([
    ["rightkit-license", "0.1.1"],
    ["rightkit-logs", "0.1.0"],
  ]);
  for (const manifest of [
    `dependencies . rightkit-license = { path = "../rightkit-license" }`,
    `target . 'cfg(windows)' . dependencies . "rightkit-logs" = { git = "https://example.test/rightkit.git" }`,
    `dependencies . rightkit-license = { version = "=0.1.1", registry = "private" }`,
  ]) {
    assert.throws(
      () => assertCargoFixture(manifest, published, "fixture/Cargo.toml"),
      /must use exact crates\.io versions|must use the exact crates\.io version/,
    );
  }
});

test("Cargo version contract keeps staged versions out of consumer pins", () => {
  const resolved = resolveCargoVersionContract(
    {
      cargo: { "rightkit-license": "0.1.1" },
      stagedCargo: { "rightkit-license": "0.1.2" },
    },
    new Map([["rightkit-license", "0.1.2"]]),
  );

  assert.equal(resolved.consumer.get("rightkit-license"), "0.1.1");
  assert.equal(resolved.canonical.get("rightkit-license"), "0.1.2");
});

test("Cargo version contract rejects unknown staged crates", () => {
  assert.throws(
    () => resolveCargoVersionContract(
      { cargo: {}, stagedCargo: { "rightkit-unknown": "0.1.0" } },
      new Map(),
    ),
    /unknown staged Cargo crate: rightkit-unknown/,
  );
});

test("Cargo version contract rejects staged versions that mismatch canonical manifests", () => {
  assert.throws(
    () => resolveCargoVersionContract(
      { cargo: { "rightkit-license": "0.1.1" }, stagedCargo: { "rightkit-license": "0.1.3" } },
      new Map([["rightkit-license", "0.1.2"]]),
    ),
    /rightkit-license canonical manifest is 0\.1\.2, expected staged version 0\.1\.3/,
  );
});

test("release package scripts use tier-neutral builds and tiered uploads", () => {
  const scripts = {
    "release:build:mac": "right-release build --platform mac",
    "release:build:win": "right-release build --platform win",
    "release:upload:patch:mac": "right-release upload --platform mac --tier patch",
    "release:upload:patch:win": "right-release upload --platform win --tier patch",
    "release:upload:update:mac": "right-release upload --platform mac --tier update",
    "release:upload:update:win": "right-release upload --platform win --tier update",
  };
  assert.doesNotThrow(() => assertReleasePackageScripts(scripts, "fixture"));
});

test("release package scripts reject a generic direct Tauri alias but allow named local commands", () => {
  const scripts = {
    "release:build:mac": "right-release build --platform mac",
    "release:build:win": "right-release build --platform win",
    "release:upload:patch:mac": "right-release upload --platform mac --tier patch",
    "release:upload:patch:win": "right-release upload --platform win --tier patch",
    "release:upload:update:mac": "right-release upload --platform mac --tier update",
    "release:upload:update:win": "right-release upload --platform win --tier update",
    "tauri:dev": "tauri dev",
    "mac:build:debug": "tauri build --debug",
    "build:win:unsigned": "tauri build --bundles nsis --no-sign",
    "cargo:check": "cargo check --locked",
  };
  assert.doesNotThrow(() => assertReleasePackageScripts(scripts, "fixture"));
  for (const genericTauri of ["tauri", "pnpm exec tauri", "pnpm dlx tauri", "./node_modules/.bin/tauri"]) {
    assert.throws(
      () => assertReleasePackageScripts({ ...scripts, tauri: genericTauri }, "fixture"),
      /generic \"tauri\" package script bypasses/i,
      genericTauri,
    );
  }
});

test("mac release config uses a dedicated non-recursive package entry point", () => {
  const command = { cmd: "pnpm", args: ["run", "rightkit:package:mac"] };
  assert.doesNotThrow(() => assertMacPackageEntry(command, { "rightkit:package:mac": "node scripts/mac-dmg.mjs --notarize" }, "fixture"));
  assert.throws(
    () => assertMacPackageEntry(command, { "rightkit:package:mac": "right-release build --platform mac" }, "fixture"),
    /instead of recursing into the release build/i,
  );
  assert.throws(
    () => assertMacPackageEntry(command, { "rightkit:package:mac": "pnpm run rightkit:package:mac" }, "fixture"),
    /must not recurse into itself/i,
  );
});

test("every app platform requires effective non-empty build inputs", () => {
  assert.doesNotThrow(() => assertBuildInputs(
    { buildInputs: { include: ["src/**"] } },
    {},
    "fixture mac",
  ));
  assert.throws(() => assertBuildInputs({}, {}, "fixture win"), /fixture win must declare non-empty buildInputs\.include paths/);
  assert.throws(() => assertBuildInputs({}, { buildInputs: { include: [] } }, "fixture mac"), /fixture mac must declare non-empty buildInputs\.include paths/);
});

test("RightKit exposes one current version manifest", () => {
  assert.equal(existsSync(versionsPath), true, "rightkit-versions.json must be the single current-version source");
  assert.match(versions.packageManager, /^pnpm@\d+\.\d+\.\d+$/);
  assert.equal(versions.npm["@rightkit/release"], "0.2.42");
  assert.equal(versions.npm["@rightkit/legal"], "0.2.0");
  assert.equal(versions.npm["@rightkit/license"], "0.1.5");
  assert.equal(versions.npm["@rightkit/logs"], "0.1.3");
  assert.equal(versions.npm["@rightkit/tauri"], "0.1.0");
  assert.equal(versions.npm["@rightkit/updates"], "0.2.3");
  assert.deepEqual(versions.stagedNpm, {
    "@rightkit/legal": "0.3.0",
    "@rightkit/legal-ui": "0.1.0",
    "@rightkit/license": "0.1.6",
  });
  assert.deepEqual(versions.legacyNpm, {
    "@rightkit/release": ["0.2.22", "0.2.29", "0.2.30", "0.2.31", "0.2.41"],
  });
  assert.ok(
    new Set([versions.npm["@rightkit/release"], ...versions.legacyNpm["@rightkit/release"]]).has("0.2.41"),
    "the previously published @rightkit/release 0.2.41 must remain accepted during the 0.2.42 rollout",
  );
  const licensePackage = JSON.parse(readFileSync(
    path.join(workspace, "tools/rightkit/packages/license/package.json"),
    "utf8",
  ));
  assert.equal(licensePackage.version, versions.stagedNpm["@rightkit/license"]);
  assert.equal(versions.cargo["rightkit-license"], "0.1.2");
  assert.equal(versions.cargo["rightkit-logs"], "0.1.0");
  assert.equal(versions.cargo["rightkit-tauri"], "0.1.0");
  assert.deepEqual(versions.stagedCargo, {
    "rightkit-license": "0.1.3",
    "rightkit-tauri": "0.1.1",
  });
  getCurrentCargoVersionContract();
});

test("license v2 public vector is identical at every portable consumer boundary", () => {
  const canonical = readFileSync(
    path.join(workspace, "tools/rightkit/crates/rightkit-license/test-vectors/license-v2.json"),
    "utf8",
  );
  for (const relativePath of [
    "tools/rightkit/packages/license/test-vectors/license-v2.json",
    `${siteRepoDir}/packages/api/src/licensing/test-vectors/license-v2.json`,
    "scraperight/tests/fixtures/license-v2.json",
  ]) {
    assert.equal(
      readFileSync(path.join(workspace, relativePath), "utf8"),
      canonical,
      `${relativePath} must be the canonical public license vector byte-for-byte`,
    );
  }
});

for (const app of apps) {
  test(`${app.key} follows the signed tiered Right Release contract`, async () => {
    const root = path.join(workspace, app.root);
    const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
    const rightKitDeps = { ...pkg.dependencies, ...pkg.devDependencies };
    for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
      assert.doesNotMatch(specifier, /^(?:git|file|link|workspace):|github/i, `${app.key} ${name} must come from the published npm package`);
      const allowed = new Set(
        [
          versions.npm[name],
          versions.stagedNpm?.[name],
          ...(versions.legacyNpm?.[name] ?? []),
        ].filter(Boolean),
      );
      if (allowed.size) assert.ok(allowed.has(specifier), `${app.key} ${name} must match a published rollout version`);
    }
    assert.equal(pkg.packageManager, versions.packageManager);
    assert.ok(
      new Set([
        versions.npm["@rightkit/release"],
        versions.stagedNpm?.["@rightkit/release"],
        ...(versions.legacyNpm?.["@rightkit/release"] ?? []),
      ]).has(pkg.devDependencies?.["@rightkit/release"]),
    );
    assert.equal(
      pkg.dependencies?.["@rightkit/updates"],
      versions.npm["@rightkit/updates"],
      `${app.key} must consume the exact published shared updater runtime`,
    );
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/right-release"));
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/rightkit/packages/release"));
    assert(!JSON.stringify(pkg).includes("git+https://github.com/adrdsouza/rightkit.git"));
    assert.equal(pkg.scripts["release:doctor"], "right-release doctor");
    assert.equal(pkg.scripts["release:mac"], undefined, "tierless release entry points are forbidden");
    assert.equal(pkg.scripts["release:win"], undefined, "tierless release entry points are forbidden");
    assertReleasePackageScripts(pkg.scripts, app.key);
    assert.equal(pkg.scripts["deps:check"], "right-release deps --check");
    assert.equal(pkg.scripts["deps:update"], "right-release deps --update");
    assert.ok(!Object.entries(pkg.scripts).some(([name, command]) => /^(?:release|publish):/i.test(name) && /unsigned|--no-sign/i.test(command)), `${app.key} release/publish commands must not expose an unsigned macOS DMG mode`);
    assert.ok(!JSON.stringify(pkg.scripts).includes("../tools/right-release"), `${app.key} scripts must not depend on the parent Claude workspace`);
    assert.ok(!JSON.stringify(pkg.scripts).includes("../tools/rightkit/packages/release"), `${app.key} scripts must not depend on the parent Claude workspace`);

    const config = (await import(`${pathToFileURL(path.join(root, "right-release.config.mjs"))}?contract=${Date.now()}`)).default;
    assert.equal(config.app, app.key);
    assert.ok(config.version);
    for (const platform of ["mac", "win"]) {
      const target = config.targets[platform];
      assertBuildInputs(config, target, `${app.key} ${platform}`);
      assert.equal(target.signed, true);
      assert.equal(target.upload, undefined, "generic uploads bypass tier manifest routing");
      assert.equal(target.publish.cmd, "right-release");
      assert.match(target.publish.args.join(" "), /^publish-update\b/);
      assert.ok(new Set(target.updater.artifacts.map((artifact) => artifact.key)).size <= 1, `${app.key} ${platform} must expose at most one current updater R2 object key`);
      assert.ok(new Set(target.installer.artifacts.map((artifact) => artifact.key)).size <= 1, `${app.key} ${platform} must expose at most one current installer R2 object key`);
      for (const installer of target.installer.artifacts) {
        assert.match(installer.key, /\/installers\/(mac|windows)\/current\//, `${app.key} ${platform} installers must replace the stable current R2 object`);
      }
      for (const updater of target.updater.artifacts) {
        assert.match(updater.key, /\/updates\/(mac|windows)\/current\//, `${app.key} ${platform} updaters must replace the stable current R2 object`);
      }
    }
    assertMacPackageEntry(config.targets.mac.package, pkg.scripts, app.key);
    for (const installer of config.targets.mac.installer.artifacts) {
      assert.equal(path.dirname(installer.file), ".", `${app.key} macOS installer must be copied to the app package root before upload`);
    }
    assert.ok(config.targets.win.sign.files.length);
    const winUpdaterFiles = new Set(config.targets.win.updater.artifacts.map((artifact) => artifact.file));
    for (const signed of config.targets.win.sign.files) assert.ok(winUpdaterFiles.has(signed), `${signed} must be both Azure-signed and updater-signed`);

    const tauri = JSON.parse(readFileSync(path.join(root, app.tauri), "utf8"));
    assert.equal(tauri.plugins.updater.pubkey, pubkey);
    assert.deepEqual(tauri.plugins.updater.endpoints, [
      `https://api.spoares.com/v1/apps/${app.key}/releases/latest.json?platform={{target}}`,
    ]);
    assert.equal(tauri.bundle.createUpdaterArtifacts, true);

    const { consumer, canonical } = getCurrentCargoVersionContract();
    const rollout = new Map([...consumer].map(([name, version]) => [name, new Set([version, canonical.get(name)].filter(Boolean))]));
    validateRightKitCargoContract(root, rollout, app.key);

    for (const localCopy of [
      "src-tauri/vendor/rightkit-license",
      "src-tauri/vendor/rightkit-logs",
      "crates/rightkit-license",
    ]) {
      assert.equal(
        existsSync(path.join(root, localCopy)),
        false,
        `${app.key} must not carry an app-local ${localCopy} copy`,
      );
    }

    const repoRoot = path.join(workspace, app.repoRoot ?? app.root);
    for (const packagePath of findFiles(repoRoot, "package.json")) {
      const candidate = JSON.parse(readFileSync(packagePath, "utf8"));
      assert.equal(candidate.scripts?.["release:win"], undefined, `${path.relative(workspace, packagePath)} has a forbidden tierless Windows release script`);
      assert.equal(candidate.scripts?.["release:mac"], undefined, `${path.relative(workspace, packagePath)} has a forbidden tierless macOS release script`);
      for (const command of Object.values(candidate.scripts ?? {})) {
        assert.doesNotMatch(command, /\brelease:(?:win|mac)\b/, `${path.relative(workspace, packagePath)} delegates to a forbidden tierless release command`);
      }
    }
    for (const instructionName of ["AGENTS.md", "CLAUDE.md"]) {
      const instructionPath = path.join(repoRoot, instructionName);
      if (!existsSync(instructionPath)) continue;
      const instruction = readFileSync(instructionPath, "utf8");
      assert.doesNotMatch(instruction, /pnpm(?: run)? release:(?:win|mac)\b/, `${app.key} ${instructionName} documents a forbidden tierless release`);
      assert.doesNotMatch(instruction, /shared tooling at [`']?\.\.\/tools\/right-release/i, `${app.key} ${instructionName} documents a sibling source dependency`);
      assert.doesNotMatch(instruction, /\$env:RIGHTAPPS_ADMIN_TOKEN|pnpm publish:r2/, `${app.key} ${instructionName} documents obsolete release credentials or publishers`);
    }
    for (const releaseFile of app.releaseFiles) {
      const source = readFileSync(path.join(root, releaseFile), "utf8");
      assert.doesNotMatch(source, /(?:\.\.\/)+tools\/right-release|tools\/right-release\//, `${app.key} ${releaseFile} must consume the installed package`);
      assert.match(source, /right-release["']?,?\s*["']mirror-root-artifact|right-release mirror-root-artifact/, `${app.key} ${releaseFile} must mirror the final DMG to the canonical package root`);
    }
  });
}

test("HeardRight and ScrapeRight expose one locked ASR promotion adapter contract", async () => {
  const heard = (await import(`${pathToFileURL(path.join(workspace, "heardright/tauri-app-next/right-release.config.mjs"))}?asr=${Date.now()}`)).default;
  const scrape = (await import(`${pathToFileURL(path.join(workspace, "scraperight/right-release.config.mjs"))}?asr=${Date.now()}`)).default;
  assert.equal(assertAsrAdapterPair(heard.runtimeArtifacts?.asr, scrape.runtimeArtifacts?.asr), true);
});

function findFiles(root, filename) {
  const found = [];
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory() && [".audit", ".cache", ".git", ".claude", ".worktrees", "node_modules", "target", "vendor"].includes(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile() && entry.name === filename) found.push(full);
    }
  };
  visit(root);
  return found;
}

function readCanonicalCrateVersions() {
  const crateRoot = path.join(workspace, "tools/rightkit/crates");
  return new Map(
    readdirSync(crateRoot, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && existsSync(path.join(crateRoot, entry.name, "Cargo.toml")))
      .map((entry) => {
        const manifest = readFileSync(path.join(crateRoot, entry.name, "Cargo.toml"), "utf8");
        const version = manifest.match(/^version\s*=\s*"([^"]+)"$/m)?.[1];
        assert.ok(version, `${entry.name} canonical manifest must declare a package version`);
        return [entry.name, version];
      }),
  );
}

test("Right Suite has no hosted workflow files", () => {
  for (const root of ["viewright", "scraperight", "heardright", "mailright", "coderight", "genright", "voiceright", "tools/rightkit"]) {
    const workflowDir = path.join(workspace, root, ".github", "workflows");
    const workflows = existsSync(workflowDir) ? readdirSync(workflowDir) : [];
    assert.deepEqual(workflows, [], `${root} must not contain hosted workflow files`);
  }
});

test("RightApps brand hosts expose app-keyed update manifest proxies", () => {
  for (const app of apps) {
    const siteRoot = path.join(workspace, siteRepoDir, app.key);
    for (const route of ["patches", "latest"]) {
      const routeFile = path.join(siteRoot, "src", "routes", "releases", `${route}.json`, "index.ts");
      const source = readFileSync(routeFile, "utf8");
      assert.match(source, new RegExp(`APP_KEY\\s*=\\s*["']${app.key}["']`), `${app.key} ${route}.json proxy must set the app key`);
      assert.match(source, /RIGHTAPPS_API_URL/, `${app.key} ${route}.json proxy must use the configurable RightApps API base`);
      assert.match(source, /x-app-key/i, `${app.key} ${route}.json proxy must forward the app key header`);
    }
  }
});
