import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { fileURLToPath, pathToFileURL } from "node:url";
import test, { after } from "node:test";
import {
  assertNoRightKitCargoOverrides,
  assertPublishedRightKitCargoDependencies,
  cargoArguments,
  cargoExecutable,
  isolatedCargoMetadataEnv,
  managedCargoShimOnPath,
  validateRightKitCargoContract,
} from "./cargo-contract.mjs";
import { assertAsrAdapterPair } from "./asr-artifact-adoption.mjs";
import { resolveConfiguredBuildInputs } from "./release-invocation.mjs";
import { compareVersions } from "./rightkit-sync.mjs";

const workspace = path.resolve(
  process.env.RIGHT_SUITE_CONTRACT_WORKSPACE
    ?? new URL("../../../..", import.meta.url).pathname.replace(/^\/(\w:)/, "$1"),
);
// The Right Suite web layer's repo + filesystem path is `rightsites` post the 2026-07-15 naming lock;
// a not-yet-renamed checkout may still have `rightapps`. Resolve whichever exists so the contract
// passes on both. (The runtime service namespace stays `rightapps` — that is deliberately untouched.)
const siteRepoDir = existsSync(path.join(workspace, "rightsites")) ? "rightsites" : "rightapps";
const versionsPath = path.join(path.dirname(fileURLToPath(import.meta.url)), "rightkit-versions.json");
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

// ScreenRight is intentionally a macOS-only AppKit consumer. It has no Tauri
// manifest or Windows target, so keep it in an explicit registry lane rather
// than manufacturing unsupported platform entries.
const macOnlyApps = [
  { key: "screenright", root: "tools/screenright", releaseFiles: ["scripts/release-package.mjs"] },
];

// Consuming suite apps whose packaging/build scripts must resolve Cargo build
// output from `cargo metadata`'s `target_directory`, never by reading
// CARGO_TARGET_DIR (or a shell/PowerShell equivalent) directly to locate it.
// This defect recurred three times across the suite (broker-managed hosts own
// CARGO_TARGET_DIR, so a stale/wrong value silently reads the wrong tree).
// rightsites and tools/screenright are intentionally out of scope here.
const cargoTargetDirGuardApps = [
  "coderight/apps/coderight-tauri",
  "cutright/apps/studio",
  "genright",
  "heardright/tauri-app-next",
  "mailright",
  "membrane/apps/membrane-hub",
  "orthic",
  "scraperight",
  "viewright",
];

function assertReleasePackageScripts(scripts, label, provider = "github-releases") {
  for (const platform of ["mac", "win"]) {
    assert.equal(scripts[`release:build:${platform}`], `right-release build --platform ${platform}`, `${label} must use the tier-neutral ${platform} build entry point`);
    for (const tier of ["patch", "update"]) {
      const expected = provider === "r2-rightapps"
        ? `right-release publish-update --config right-release.config.mjs --platform ${platform} --tier ${tier}`
        : `right-release upload --platform ${platform} --tier ${tier}`;
      assert.ok([expected, `right-release upload --config right-release.config.mjs --platform ${platform} --tier ${tier}`].includes(scripts[`release:upload:${tier}:${platform}`]), `${label} must select ${tier} through its ${provider} publisher`);
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
  assert.ok(
    [
      ["run", "rightkit:package:mac"],
      ["run", "rightkit:package:mac", "--", "--notarize"],
    ].some((allowed) => JSON.stringify(packageCommand.args) === JSON.stringify(allowed)),
    `${label} R2 release must use the dedicated signed and notarized macOS package entry point`,
  );
  const implementation = scripts["rightkit:package:mac"];
  assert.equal(typeof implementation, "string", `${label} must define rightkit:package:mac`);
  assert.doesNotMatch(implementation, /(?<![-\w])right-release(?![-\w])/, `${label} rightkit:package:mac must package directly instead of recursing into the release build`);
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
  const result = runCargoSync(
    ["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path", manifestPath],
    {
      cwd: process.cwd(),
      encoding: "utf8",
      env: isolatedCargoMetadataEnv(isolatedCargoHome),
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

test("Cargo contract ignores release caches and build outputs", () => {
  const fixtureRoot = mkdtempSync(path.join(tmpdir(), "rightkit-cargo-scan-"));
  const appRoot = path.join(fixtureRoot, "app");
  mkdirSync(path.join(appRoot, ".right-release", "cache", "cargo-home", "registry", "cached"), { recursive: true });
  writeFileSync(
    path.join(appRoot, "Cargo.toml"),
    '[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = "fixture.rs"\n',
    "utf8",
  );
  writeFileSync(path.join(appRoot, "fixture.rs"), "", "utf8");
  writeFileSync(
    path.join(appRoot, ".right-release", "cache", "cargo-home", "registry", "cached", "Cargo.toml"),
    "this is deliberately invalid cached TOML",
    "utf8",
  );
  try {
    assert.equal(validateRightKitCargoContract(appRoot, new Map(), "fixture"), 1);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
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
  assert.doesNotThrow(() => assertMacPackageEntry(
    { cmd: "pnpm", args: ["run", "rightkit:package:mac", "--", "--notarize"] },
    { "rightkit:package:mac": "node scripts/mac-dmg.mjs" },
    "fixture",
  ));
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
  assert.equal(versions.schema, 2);
  assert.match(versions.packageManager, /^pnpm@\d+\.\d+\.\d+$/);
  assert.equal(versions.npm["@rightkit/release"], "0.2.47");
  assert.equal(versions.npm["@rightkit/legal"], "0.2.0");
  assert.equal(versions.npm["@rightkit/license"], "0.1.5");
  assert.equal(versions.npm["@rightkit/logs"], "0.1.3");
  assert.equal(versions.npm["@rightkit/tauri"], "0.1.0");
  assert.equal(versions.npm["@rightkit/updates"], "0.2.3");
  // recordPublished rewrites the staged table on every publish; assert its shape, not a snapshot.
  for (const [name, version] of Object.entries(versions.stagedNpm)) assert.match(version, /^\d+\.\d+\.\d+$/, `stagedNpm ${name}`);
  for (const [name, list] of Object.entries(versions.legacyNpm ?? {})) for (const version of list) assert.match(version, /^\d+\.\d+\.\d+$/, `legacyNpm ${name}`);
  assert.ok(
    new Set([versions.npm["@rightkit/release"], ...versions.legacyNpm["@rightkit/release"]]).has("0.2.42"),
    "previously published @rightkit/release versions must remain accepted during the 0.2.77 rollout",
  );
  for (const [name, version] of Object.entries(versions.stagedNpm)) {
    const packageName = name.slice("@rightkit/".length);
    const manifest = JSON.parse(readFileSync(
      path.join(workspace, "tools/rightkit/packages", packageName, "package.json"),
      "utf8",
    ));
    assert.equal(manifest.name, name);
    assert.equal(manifest.version, version, `${name} package version must equal stagedNpm`);
  }
  assert.equal(versions.cargo["rightkit-license"], "0.1.2");
  assert.equal(versions.cargo["rightkit-logs"], "0.1.0");
  assert.equal(versions.cargo["rightkit-tauri"], "0.1.0");
  // recordPublished rewrites the staged table on every publish; assert its shape, not a snapshot.
  for (const [name, version] of Object.entries(versions.stagedCargo)) assert.match(version, /^\d+\.\d+\.\d+$/, `stagedCargo ${name}`);
  for (const [name, list] of Object.entries(versions.legacyCargo ?? {})) for (const version of list) assert.match(version, /^\d+\.\d+\.\d+$/, `legacyCargo ${name}`);
  assert.deepEqual(versions.stagedSwift, { "rightkit-swift": { url: "https://github.com/bogusyogi/rightkit-swift.git", version: "0.3.1" } });
  getCurrentCargoVersionContract();
});

test("superseded published rollout versions remain in legacy", () => {
  // One bounded local lookup; exported/standalone packages still enforce recorded publication below.
  const head = spawnSync("git", ["show", "HEAD:./rightkit-versions.json"], {
    cwd: path.dirname(versionsPath), encoding: "utf8", timeout: 5_000, maxBuffer: 1024 * 1024,
  });
  const previous = head.status === 0 ? JSON.parse(head.stdout) : null;
  for (const [registry, stagedKey, legacyKey] of [["npm", "stagedNpm", "legacyNpm"], ["cargo", "stagedCargo", "legacyCargo"]]) {
    for (const [name, published] of Object.entries(versions.registry?.[registry] ?? {})) {
      const staged = versions[stagedKey]?.[name];
      if (!staged || compareVersions(published, staged) >= 0 || published === versions[registry]?.[name]) continue;
      assert.ok(versions[legacyKey]?.[name]?.includes(published), `${name} published ${published} is older than staged ${staged}; retain it in ${legacyKey}`);
    }
    for (const [name, stagedBefore] of Object.entries(previous?.[stagedKey] ?? {})) {
      const stagedNow = versions[stagedKey]?.[name];
      const published = stagedBefore === previous.registry?.[registry]?.[name]
        || stagedBefore === versions.registry?.[registry]?.[name];
      if (!published || !stagedNow || compareVersions(stagedBefore, stagedNow) >= 0) continue;
      assert.ok(versions[legacyKey]?.[name]?.includes(stagedBefore), `${name} published HEAD staging ${stagedBefore} was superseded by ${stagedNow}; retain it in ${legacyKey}`);
    }
  }
});

test("Cargo metadata uses managed Cargo directly on Windows", () => {
  assert.equal(cargoExecutable("win32"), "cargo");
  assert.deepEqual(cargoArguments(["metadata"], "win32"), ["metadata"]);
  assert.equal(cargoExecutable("darwin"), "cargo");
});

test("Cargo metadata delegates controlled storage to managed RightKit", () => {
  const ambient = {
    CARGO_HOME: "/ambient/cargo",
    CARGO_TARGET_DIR: "/ambient/target",
    RIGHTKIT_BUILD_BROKER_SOCKET: "/managed/broker.sock",
  };
  assert.deepEqual(isolatedCargoMetadataEnv("/isolated/cargo", ambient), {
    RIGHTKIT_BUILD_BROKER_SOCKET: "/managed/broker.sock",
  });
  assert.deepEqual(isolatedCargoMetadataEnv("/isolated/cargo", { CARGO_HOME: "/ambient/cargo" }, process.platform, () => false), {
    CARGO_HOME: "/isolated/cargo",
  });
});

test("Cargo metadata recognizes managed Cargo from PATH without inherited broker environment", () => {
  const macShim = "/Volumes/D/.rightkit-managed/agent-bin/cargo";
  const macEnv = { PATH: `/usr/bin:${path.dirname(macShim)}`, CARGO_HOME: "" };
  assert.equal(managedCargoShimOnPath(macEnv, "darwin", (candidate) => candidate === macShim), true);
  assert.deepEqual(isolatedCargoMetadataEnv("/isolated/cargo", macEnv, "darwin", (candidate) => candidate === macShim), {
    PATH: macEnv.PATH,
  });

  const windowsShim = "D:\\RightKitManagedAgent\\agent-bin\\cargo.cmd";
  const windowsEnv = {
    PATH: "C:\\Windows\\System32;D:\\RightKitManagedAgent\\agent-bin",
    PATHEXT: ".EXE;.CMD",
  };
  assert.equal(managedCargoShimOnPath(windowsEnv, "win32", (candidate) => candidate.toLowerCase() === windowsShim.toLowerCase()), true);
  assert.deepEqual(isolatedCargoMetadataEnv("D:\\isolated\\cargo", windowsEnv, "win32", (candidate) => candidate.toLowerCase() === windowsShim.toLowerCase()), windowsEnv);
});

test("Cargo metadata respects PATH precedence when unmanaged Cargo resolves first", () => {
  const env = { PATH: "/usr/local/bin:/Volumes/D/.rightkit-managed/agent-bin" };
  const existing = new Set([
    "/usr/local/bin/cargo",
    "/Volumes/D/.rightkit-managed/agent-bin/cargo",
  ]);
  assert.equal(managedCargoShimOnPath(env, "darwin", (candidate) => existing.has(candidate)), false);
  assert.deepEqual(isolatedCargoMetadataEnv("/isolated/cargo", env, "darwin", (candidate) => existing.has(candidate)), {
    PATH: env.PATH,
    CARGO_HOME: "/isolated/cargo",
  });
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
    const releaseVersion = pkg.devDependencies?.["@rightkit/release"];
    const acceptedPackageManagers = releaseVersion === versions.stagedNpm?.["@rightkit/release"]
      ? [versions.packageManager]
      : [versions.packageManager, "pnpm@11.12.0", "pnpm@11.18.0"];
    assert.ok(
      acceptedPackageManagers.includes(pkg.packageManager),
      `${app.key} packageManager must match its @rightkit/release rollout lane`,
    );
    assert.ok(
      new Set([
        versions.npm["@rightkit/release"],
        versions.stagedNpm?.["@rightkit/release"],
        ...(versions.legacyNpm?.["@rightkit/release"] ?? []),
      ]).has(pkg.devDependencies?.["@rightkit/release"]),
    );
    assert.equal(
      pkg.dependencies?.["@rightkit/updates"],
      versions.npm["@rightkit/updates"] === pkg.dependencies?.["@rightkit/updates"]
        ? versions.npm["@rightkit/updates"]
        : versions.stagedNpm?.["@rightkit/updates"],
      `${app.key} must consume the exact published shared updater runtime`,
    );
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/right-release"));
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/rightkit/packages/release"));
    assert(!JSON.stringify(pkg).includes("git+https://github.com/adrdsouza/rightkit.git"));
    assert.equal(pkg.scripts["release:doctor"], "right-release doctor");
    assert.equal(pkg.scripts["release:mac"], undefined, "tierless release entry points are forbidden");
    assert.equal(pkg.scripts["release:win"], undefined, "tierless release entry points are forbidden");
    assertReleasePackageScripts(pkg.scripts, app.key, "r2-rightapps");
    assert.equal(pkg.scripts["deps:check"], "right-release deps --check");
    assert.equal(pkg.scripts["deps:update"], "right-release deps --update");
    assert.ok(!Object.entries(pkg.scripts).some(([name, command]) => /^(?:release|publish):/i.test(name) && /unsigned|--no-sign/i.test(command)), `${app.key} release/publish commands must not expose an unsigned macOS DMG mode`);
    assert.ok(!JSON.stringify(pkg.scripts).includes("../tools/right-release"), `${app.key} scripts must not depend on the parent Claude workspace`);
    assert.ok(!JSON.stringify(pkg.scripts).includes("../tools/rightkit/packages/release"), `${app.key} scripts must not depend on the parent Claude workspace`);

    const config = (await import(`${pathToFileURL(path.join(root, "right-release.config.mjs"))}?contract=${Date.now()}`)).default;
    assert.equal(config.app, app.key);
    assert.ok(config.version);
    assert.equal(config.distribution?.provider, "r2-rightapps", `${app.key} private release config must select R2 + RightApps explicitly`);
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
      `https://${app.key === "scraperight" ? "scraperight.cc" : "api.spoares.com"}/v1/apps/${app.key}/releases/latest.json?platform={{target}}`,
    ]);
    assert.equal(tauri.bundle.createUpdaterArtifacts, true);

    const { consumer, canonical } = getCurrentCargoVersionContract();
    // Same allowance as the doctor (release.mjs buildAllowedVersions): published, staged and legacy crates.
    const legacy = versions.legacyCargo ?? {};
    const rollout = new Map([...new Set([...consumer.keys(), ...canonical.keys(), ...Object.keys(legacy)])]
      .map((name) => [name, new Set([consumer.get(name), canonical.get(name), ...(legacy[name] ?? [])].filter(Boolean))]));
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

for (const app of macOnlyApps) {
  test(`${app.key} follows the macOS-only AppKit Right Release contract`, async (context) => {
    const root = path.join(workspace, app.root);
    if (!existsSync(path.join(root, "package.json"))) {
      context.skip(`${app.key} is not present in this checkout`);
      return;
    }
    const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
    assert.equal(pkg.scripts["release:doctor"], "right-release doctor");
    assert.equal(pkg.scripts["release:build:mac"], "right-release build --platform mac");
    assert.equal(pkg.scripts["release:upload:patch:mac"], "right-release upload --platform mac --tier patch");
    assert.equal(pkg.scripts["release:build:win"], undefined, `${app.key} must not fake a Windows target`);
    assert.equal(pkg.scripts["release:upload:patch:win"], undefined, `${app.key} must not fake a Windows target`);
    assert.equal(pkg.scripts["release:upload:update:mac"], undefined, `${app.key} must not expose ungated feature-update upload`);
    assert.equal(pkg.scripts["release:upload:update:win"], undefined);

    const config = (await import(`${pathToFileURL(path.join(root, "right-release.config.mjs"))}?mac-only=${Date.now()}`)).default;
    assert.equal(config.app, app.key);
    assert.deepEqual(Object.keys(config.targets), ["mac"]);
    assert.equal(config.targets.mac.signed, true);
    assert.deepEqual(config.targets.mac.publish, {
      cmd: "right-release",
      args: ["publish-update", "--config", "right-release.config.mjs", "--platform", "mac"],
    });
    const updaters = config.targets.mac.updater.artifacts;
    assert.equal(updaters.length, 2);
    assert.deepEqual(updaters.map((updater) => updater.platform).sort(), ["darwin-aarch64", "darwin-x86_64"]);
    assert.equal(new Set(updaters.map((updater) => updater.file)).size, 1, `${app.key} universal updater must share one DMG`);
    assert.equal(new Set(updaters.map((updater) => updater.signature)).size, 1, `${app.key} universal updater must share one signature`);
    assert.equal(new Set(updaters.map((updater) => updater.key)).size, 1, `${app.key} universal updater must share one current R2 object`);
    for (const updater of updaters) {
      assert.match(updater.file, /\.dmg$/);
      assert.equal(updater.signature, `${updater.file}.sig`);
      assert.equal(updater.key, `${app.key}/updates/mac/current/ScreenRight.dmg`);
    }
    assertBuildInputs(config, config.targets.mac, `${app.key} mac`);
    assertMacPackageEntry(config.targets.mac.package, pkg.scripts, app.key);
    assert.ok(config.targets.mac.installer.artifacts.length >= 1);
    assert.ok(config.checks.includes("legal:check"), `${app.key} must gate generated legal notices`);
    assert.deepEqual(config.deps, { workdirs: ["."] });
    assert.equal(existsSync(path.join(root, "src-tauri", "tauri.conf.json")), false, `${app.key} must not invent a Tauri manifest`);
    for (const releaseFile of app.releaseFiles) {
      const source = readFileSync(path.join(root, releaseFile), "utf8");
      assert.match(source, /mirror-root-artifact/);
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

const CARGO_TARGET_DIR_GUARD_EXTENSIONS = new Set([".mjs", ".js", ".ts", ".sh", ".ps1"]);
const CARGO_TARGET_DIR_GUARD_ALLOW_MARKER = /rightkit-allow-cargo-target-dir:\s*\S/;
// Test/contract files assert *about* the CARGO_TARGET_DIR pattern (e.g. inside
// a regex literal passed to assert.match/assert.doesNotMatch) rather than
// locating build output with it; they are not packaging/build scripts.
const CARGO_TARGET_DIR_GUARD_SKIP_BASENAME = /(?:^test-.*|\.test)\.(?:mjs|js|ts)$/;

function cargoTargetDirGuardPatternFor(extension) {
  if (extension === ".ps1") return /\$env:CARGO_TARGET_DIR\b/;
  if (extension === ".sh") return /\$\{?CARGO_TARGET_DIR\b/;
  return /process\.env(?:\.CARGO_TARGET_DIR\b|\[["']CARGO_TARGET_DIR["']\])/;
}

function findCargoTargetDirGuardFiles(root) {
  const found = [];
  const visit = (dir) => {
    if (!existsSync(dir)) return;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory() && ["node_modules", "target", "vendor", ".git", ".cache", ".right-release"].includes(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile() && CARGO_TARGET_DIR_GUARD_EXTENSIONS.has(path.extname(entry.name))) found.push(full);
    }
  };
  const scriptsDir = path.join(root, "scripts");
  if (existsSync(scriptsDir)) visit(scriptsDir);
  for (const topLevel of ["package.sh", "package.ps1"]) {
    const candidate = path.join(root, topLevel);
    if (existsSync(candidate)) found.push(candidate);
  }
  return found;
}

test("consuming suite apps resolve Cargo build output from cargo metadata, never CARGO_TARGET_DIR directly", () => {
  for (const appRoot of cargoTargetDirGuardApps) {
    const root = path.join(workspace, appRoot);
    if (!existsSync(root)) continue;
    for (const filePath of findCargoTargetDirGuardFiles(root)) {
      const basename = path.basename(filePath);
      if (CARGO_TARGET_DIR_GUARD_SKIP_BASENAME.test(basename)) continue;
      const extension = path.extname(filePath);
      const pattern = cargoTargetDirGuardPatternFor(extension);
      const lines = readFileSync(filePath, "utf8").split("\n");
      const relativePath = path.relative(workspace, filePath);
      lines.forEach((line, index) => {
        if (!pattern.test(line)) return;
        if (/^\s*(?:\/\/|#)/.test(line)) return; // prose describing the pattern, not code reading it
        const previousLine = index > 0 ? lines[index - 1] : "";
        const allowed = CARGO_TARGET_DIR_GUARD_ALLOW_MARKER.test(line) || CARGO_TARGET_DIR_GUARD_ALLOW_MARKER.test(previousLine);
        assert.ok(
          allowed,
          `${relativePath}:${index + 1} reads CARGO_TARGET_DIR directly to locate Cargo build output: \`${line.trim()}\`\n`
          + "resolve build output from `cargo metadata`'s `target_directory`; the environment is never authoritative for locating output.\n"
          + "add a `rightkit-allow-cargo-target-dir: <reason>` marker on this line (or the line above) only if this use is not locating output.",
        );
      });
    }
  }
});

// The shared resolver helpers themselves must keep asking cargo metadata for
// target_directory. Guarding only the wrong pattern's reintroduction is not
// enough — gutting/deleting the correct implementation must also fail here.
const cargoTargetRootHelpers = [
  "coderight/apps/coderight-tauri/scripts/lib/target-root.mjs",
  "heardright/tauri-app-next/scripts/lib/target-root.mjs",
  "genright/scripts/lib/target-root.mjs",
  "mailright/scripts/lib/target-root.mjs",
  "orthic/scripts/lib/target-root.mjs",
  "viewright/scripts/lib/target-root.mjs",
  "membrane/apps/membrane-hub/scripts/lib/target-root.mjs",
];

test("consumer target-root helpers delegate to RightKit's canonical resolver", () => {
  for (const helperPath of cargoTargetRootHelpers) {
    const appRoot = path.join(workspace, helperPath.split("/scripts/lib/target-root.mjs")[0]);
    if (!existsSync(path.join(appRoot, "package.json"))) continue;
    const fullPath = path.join(workspace, helperPath);
    assert.ok(existsSync(fullPath), `${helperPath} is missing; the app must keep its shared cargo-metadata target resolver`);
    const source = readFileSync(fullPath, "utf8");
    assert.match(source, /@rightkit\/release\/cargo-target\.mjs/, `${helperPath} must import RightKit's resolver`);
    assert.match(source, /resolveTargetRoot/, `${helperPath} must delegate target resolution to RightKit`);
  }
});

test("consuming app target-root helpers do not reimplement Cargo metadata", () => {
  const LOCAL_RESOLVER_PATTERN = /function\s+(?:cargoTargetRoot|resolveManagedCargoTarget)\s*\([^)]*\)\s*\{[^}]*cargo[^}]*metadata/s;
  for (const helperPath of cargoTargetRootHelpers) {
    const appRoot = path.join(workspace, helperPath.split("/scripts/lib/target-root.mjs")[0]);
    if (!existsSync(appRoot)) continue;
    const fullPath = path.join(workspace, helperPath);
    if (!existsSync(fullPath)) continue;
    const source = readFileSync(fullPath, "utf8");
    assert.doesNotMatch(
      source,
      LOCAL_RESOLVER_PATTERN,
      `${helperPath} must re-export @rightkit/release's resolveTargetRoot instead of reimplementing the cargo metadata call locally`,
    );
  }
});

const cargoAuthorityRoots = [
  ...cargoTargetDirGuardApps,
  "citadel",
  "heardright",
  "legion",
  "rightsites",
  "rightsuite",
  "sellright",
  "tools/screenright",
  "voiceright",
  "workright",
];
const cargoAuthorityNames = [
  "CARGO_HOME",
  "CARGO_TARGET_DIR",
  "CARGO_BUILD_TARGET_DIR",
  "CARGO_BUILD_BUILD_DIR",
  "CARGO_BUILD_JOBS",
  "CARGO_ENCODED_RUSTFLAGS",
  "RUSTFLAGS",
  "RUSTC_WRAPPER",
  "RUSTC_WORKSPACE_WRAPPER",
  "SCCACHE_DIR",
  "SCCACHE_BASEDIRS",
  "SCCACHE_CACHE_SIZE",
];
const cargoAuthorityExtensions = new Set([".mjs", ".js", ".cjs", ".ts", ".sh", ".ps1", ".py", ".json"]);

function findFirstPartyBuildScripts(root) {
  const found = [];
  const visit = (dir) => {
    if (!existsSync(dir)) return;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory() && [
        ".agent", ".audit", ".cache", ".git", ".right-release", "bakeoff", "dist", "docs",
        "node_modules", "reference", "target", "tests", "vendor",
      ].includes(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        visit(full);
        continue;
      }
      if (!entry.isFile() || !cargoAuthorityExtensions.has(path.extname(entry.name))) continue;
      const relative = path.relative(root, full).replaceAll(path.sep, "/");
      if (/(?:^|\/)(?:test-|[^/]+\.test\.)/.test(relative)) continue;
      if (
        relative.includes("/scripts/")
        || relative.startsWith("scripts/")
        || /(?:^|\/)(?:package\.json|package\.(?:sh|ps1)|right-release\.config\.mjs)$/.test(relative)
        || (!relative.includes("/") && /\.(?:sh|ps1)$/.test(relative))
      ) found.push(full);
    }
  };
  visit(root);
  return found;
}

function executableScriptSource(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((line) => !/^\s*(?:\/\/|#)/.test(line))
    .join("\n");
}

test("product scripts never override or destroy RightKit-owned Cargo cache state", () => {
  const authorityName = cargoAuthorityNames.join("|");
  const jsMutation = new RegExp(`(?:delete\\s+process\\.env\\.(?:${authorityName})|process\\.env\\.(?:${authorityName})\\s*=|\\b(?:${authorityName})\\s*:)`);
  const shellMutation = new RegExp(`(?:^|\\n)\\s*(?:export\\s+|unset\\s+|env\\s+(?:-[^\\n ]+\\s+)*-u\\s+)?(?:${authorityName})(?:\\s*=|\\b)`);
  const powershellMutation = new RegExp(`\\$env:(?:${authorityName})\\s*=`, "i");
  const cargoClean = /\bcargo\s+clean\b|["']cargo["'][\s\S]{0,400}?["']clean["']/;

  for (const appRoot of cargoAuthorityRoots) {
    const root = path.join(workspace, appRoot);
    if (!existsSync(root)) continue;
    for (const filePath of findFirstPartyBuildScripts(root)) {
      const source = executableScriptSource(readFileSync(filePath, "utf8"));
      const relative = path.relative(workspace, filePath);
      assert.doesNotMatch(source, jsMutation, `${relative} mutates RightKit-owned Cargo/cache environment`);
      assert.doesNotMatch(source, shellMutation, `${relative} mutates RightKit-owned Cargo/cache environment`);
      assert.doesNotMatch(source, powershellMutation, `${relative} mutates RightKit-owned Cargo/cache environment`);
      assert.doesNotMatch(source, cargoClean, `${relative} destroys reusable Cargo output with cargo clean`);
      assert.doesNotMatch(source, /--target-dir\b/, `${relative} bypasses RightKit target ownership`);
      assert.doesNotMatch(source, /(?:^|[\\/])\.cargo[\\/]bin(?:[\\/]|\b)/i, `${relative} bypasses managed Cargo by injecting a toolchain directory`);
      assert.doesNotMatch(source, /build-guard\.mjs/, `${relative} depends on obsolete product-local build/cache control`);
    }
    assert.equal(existsSync(path.join(root, "scripts", "build-guard.mjs")), false, `${appRoot} must not carry a product-local build guard`);
  }
});

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
      assert.match(source, /resolveApiBase\s*\(/, `${app.key} ${route}.json proxy must use its imported API base resolver`);
      const envHelper = readFileSync(path.join(siteRoot, "src", "lib", "env.ts"), "utf8");
      assert.match(envHelper, /RIGHTSITES_API_URL/, `${app.key} API base resolver must read the configured RightSites API URL`);
      assert.match(source, /x-app-key/i, `${app.key} ${route}.json proxy must forward the app key header`);
    }
  }
});
