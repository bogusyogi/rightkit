import "./test-support/broker-guard.mjs";
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
import { RENAMED_AWAY } from "./renamed-away.mjs";
import { rightKitVersionAllowed } from "./rightkit-version-policy.mjs";
import { assertPublishDoc, assertPublishWorkflow as assertPublishWorkflowContract, MUTATIONS, proveMutations, VERIFY_HOSTS } from "../../scripts/ci/workflow-contract.mjs";
import { assertReviewedDoc, REVIEWED_MUTATIONS } from "../../scripts/ci/reviewed-workflow-contract.mjs";
import { assertSwiftDoc, proveSwiftMutations } from "../../scripts/ci/swift-workflow-contract.mjs";
import { parseYaml } from "../../scripts/ci/workflow-yaml.mjs";

import { pendingUpdaterEndpoint, SUITE_API_CONTRACT_HOSTS } from "./suite-api-literal.mjs";
import { validateDevelopmentConfig } from "./development.mjs";

const workspace = path.resolve(
  process.env.RIGHT_SUITE_CONTRACT_WORKSPACE
    ?? fileURLToPath(new URL("../../..", import.meta.url)),
);
// The Right Suite web layer's repo + filesystem path is `rightsites` post the 2026-07-15 naming lock;
// a not-yet-renamed checkout may still have `rightapps`. Resolve whichever exists so the contract
// passes on both. (The runtime service namespace stays `rightapps` — that is deliberately untouched.)
const siteRepoDir = existsSync(path.join(workspace, "rightsites")) ? "rightsites" : "rightapps";
// Product checkouts sit beside rightkit in the owner's workspace; a hosted CI checkout holds rightkit alone. Missing
// checkouts skip their tests with the reason. RIGHTKIT_REQUIRE_ASSETS=1 runs them anyway, so the missing path fails them.
// RK_ISOLATED_CHECKOUT=1 is the same switch for a broker-admitted test row: the broker reserves every RIGHTKIT_* key in an
// adapter's environment, so the local publication pre-flight (hosted scope: rightkit alone) passes this one instead.
const isolatedCheckout = process.env.RIGHTKIT_ISOLATED_CHECKOUT === "1" || process.env.RK_ISOLATED_CHECKOUT === "1";
const checkoutSkip = (paths, what) => {
  const missing = paths.filter((file) => !existsSync(file)).map((file) => path.relative(workspace, file));
  return (missing.length === 0 && !isolatedCheckout) || process.env.RIGHTKIT_REQUIRE_ASSETS === "1" ? false : `${what} not checked out beside rightkit (missing ${missing.join(", ")})`;
};
const versionsPath = path.join(path.dirname(fileURLToPath(import.meta.url)), "rightkit-versions.json");
const versions = JSON.parse(readFileSync(versionsPath, "utf8"));
const isolatedCargoHome = mkdtempSync(path.join(tmpdir(), "rightkit-cargo-home-"));
after(() => rmSync(isolatedCargoHome, { recursive: true, force: true }));
const pubkey = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDI5Mzk1RjlGRjQ2NjI2MUQKUldRZEptYjBuMTg1S1VSUXlBdFM4WmtzaHArYko0U2hRMDVlSDJmSExVZG82Q0hoQ2srUlhqanAK";
const apps = [
  { key: "viewright", root: "viewright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/build-mac-notarized.sh"] },
  { key: "scraperight", root: "scraperight", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["package.sh"] },
  // Build routes were removed in HeardRight 5153c3f5; its config declares the disabled state.
  { key: "heardright", root: "heardright/tauri-app-next", repoRoot: "heardright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs"] },
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

// Declarative recipe lane (rightkit#345, package-recipe-targets.mjs): `targets.mac.macPackage` and `targets.win.winPackage`
// replace the legacy `package` / `prePackage` scripts. Notarization is the sibling `targets.mac.notarize { app, file }`,
// not a key inside macPackage. The generic `tauri` script bypass is still checked by assertReleasePackageScripts.
function usesRecipeLane(config) {
  const macPackage = config.targets?.mac?.macPackage;
  return macPackage !== null && typeof macPackage === "object" && !Array.isArray(macPackage);
}

function assertMacPackageRecipe(mac, label) {
  assert.equal(mac.package, undefined, `${label} macPackage replaces targets.mac.package; remove it`);
  assert.equal(mac.prePackage, undefined, `${label} macPackage replaces targets.mac.prePackage; remove it`);
  assert.notEqual(mac.dmg, false, `${label} macPackage always builds a DMG; remove dmg: false`);
  assert.ok(
    mac.notarize && typeof mac.notarize === "object" && typeof mac.notarize.app === "string" && mac.notarize.app.endsWith(".app")
      && typeof mac.notarize.file === "string" && mac.notarize.file.endsWith(".dmg"),
    `${label} R2 release must notarize through targets.mac.notarize { app: "<.app>", file: "<.dmg>" } alongside macPackage`,
  );
}

function assertWinPackageRecipe(win, label) {
  assert.equal(win.prePackage, undefined, `${label} winPackage replaces targets.win.prePackage; remove it`);
  assert.ok(win.winPackage && typeof win.winPackage === "object" && !Array.isArray(win.winPackage), `${label} must declare targets.win.winPackage as an object`);
  assert.equal(win.winPackage.sidecars, undefined, `${label} winPackage.sidecars is not supported; declare sidecars through externalBin or mediaSidecar`);
  assert.equal(win.winPackage.postBuild, undefined, `${label} winPackage.postBuild is not supported; use prepare`);
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
    // The tree may run ahead of staged between a bump and its publish; it may never be behind.
    if (compareVersions(actual, version) < 0) {
      throw new Error(`${crate} canonical manifest is ${actual}, expected staged version ${version}`);
    }
    canonical.set(crate, version);
  }

  for (const [crate, version] of consumer) {
    if (RENAMED_AWAY.has(crate) && !canonicalVersions.has(crate)) {
      const successor = RENAMED_AWAY.get(crate);
      if (successor && !canonicalVersions.has(successor)) throw new Error(`${crate} was renamed to ${successor}, which is missing from the tree`);
      continue;
    }
    if (!canonicalVersions.has(crate)) throw new Error(`unknown published Cargo crate: ${crate}`);
    const expected = staged.get(crate) ?? version;
    const actual = canonicalVersions.get(crate);
    if (compareVersions(actual, expected) < 0) {
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
    ["rightkit-device", "0.1.0"],
  ]);
  const manifest = `
[ package ]
name = "rightkit-device"
# rightkit-device = "=0.1.0"

[ dependencies ]
"rightkit-license" = "=0.1.2" # published exact pin
"rightkit-device" = "=0.1.0"

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
      stagedCargo: { "rightkit-device": "0.1.0" },
    },
    new Map([
      ["rightkit-license", "0.1.2"],
      ["rightkit-device", "0.1.0"],
    ]),
  );
  assert.equal(consumer.has("rightkit-device"), false);
  assert.throws(
    () => assertCargoFixture(
      `dependencies . "rightkit-device" = "=0.1.0" # staged only`,
      consumer,
      "fixture/Cargo.toml",
    ),
    /fixture\/Cargo\.toml rightkit-device is not a published RightKit crate/,
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

test("Cargo version contract accepts a tree that is ahead of staged and published, and keeps consumers on the recorded version", () => {
  const resolved = resolveCargoVersionContract(
    { cargo: { "rightkit-license": "0.1.1", "rightkit-logs": "0.1.0" }, stagedCargo: { "rightkit-license": "0.1.2" } },
    new Map([["rightkit-license", "0.1.3"], ["rightkit-logs", "0.2.0"]]),
  );
  assert.equal(resolved.canonical.get("rightkit-license"), "0.1.2");
  assert.equal(resolved.canonical.get("rightkit-logs"), "0.1.0");
  assert.equal(resolved.consumer.get("rightkit-license"), "0.1.1");
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
      path.join(workspace, "rightkit/packages", packageName, "package.json"),
      "utf8",
    ));
    assert.equal(manifest.name, name);
    // The tree runs ahead of staged between a version bump and its publish (recordPublished advances staged on
    // publish); what must never happen is staged naming a version the tree does not have yet reached.
    assert.ok(compareVersions(manifest.version, version) >= 0, `${name} package version ${manifest.version} must not be behind stagedNpm ${version}`);
  }
  assert.equal(versions.cargo["rightkit-license"], "0.1.2");
  assert.equal(versions.cargo["rightkit-logs"], "0.1.0");
  assert.equal(versions.cargo["rightkit-tauri"], "0.1.0"); // published registry state keeps the old name until rightkit-device is first published
  // recordPublished rewrites the staged table on every publish; assert its shape, not a snapshot.
  for (const [name, version] of Object.entries(versions.stagedCargo)) assert.match(version, /^\d+\.\d+\.\d+$/, `stagedCargo ${name}`);
  for (const [name, list] of Object.entries(versions.legacyCargo ?? {})) for (const version of list) assert.match(version, /^\d+\.\d+\.\d+$/, `legacyCargo ${name}`);
  assert.deepEqual(versions.stagedSwift, { "rightkit-swift": { url: "https://github.com/Orthic-Labs/rightkit", version: "0.8.1" } });
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

test("license v2 public vector is identical at every portable consumer boundary", { skip: checkoutSkip([
  path.join(workspace, "rightkit/crates/rightkit-license/test-vectors/license-v2.json"),
  path.join(workspace, "rightkit/packages/license/test-vectors/license-v2.json"),
  path.join(workspace, siteRepoDir, "packages/api/src/licensing/test-vectors/license-v2.json"),
  path.join(workspace, "scraperight/tests/fixtures/license-v2.json"),
], "license v2 consumer boundaries") }, () => {
  const canonical = readFileSync(
    path.join(workspace, "rightkit/crates/rightkit-license/test-vectors/license-v2.json"),
    "utf8",
  );
  for (const relativePath of [
    "rightkit/packages/license/test-vectors/license-v2.json",
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
  test(`${app.key} follows the signed tiered Right Release contract (or declares its disabled state)`, { skip: checkoutSkip([path.join(workspace, app.root, "package.json")], `${app.key} product checkout`) }, async (context) => {
    const root = path.join(workspace, app.root);
    const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
    const scripts = pkg.scripts ?? {};
    const rightKitDeps = { ...pkg.dependencies, ...pkg.devDependencies };
    for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
      assert.doesNotMatch(specifier, /^(?:git|file|link|workspace):|github/i, `${app.key} ${name} must come from the published npm package`);
      const allowed = new Set(
        [
          versions.npm[name],
          versions.stagedNpm?.[name],
          ...(versions.legacyNpm?.[name] ?? []),
          versions.registry?.npm?.[name],
        ].filter(Boolean),
      );
      // Same rule as the doctor: a recorded version, or an exact version inside the recorded range.
      if (allowed.size) assert.ok(rightKitVersionAllowed(allowed, specifier), `${app.key} ${name} must match a published rollout version (got ${specifier})`);
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
    // The updater runtime an app consumes must be a published rollout version: the mandatory one, the registry's latest, or a
    // retained legacy version (apps port to a newly published runtime in one batch, after RightKit publishes it; until then they
    // stay on the previous published one). The app's own release doctor is where "latest" is enforced, at the app's release.
    const updater = pkg.dependencies?.["@rightkit/app-update"];
    const publishedUpdaters = new Set([versions.npm["@rightkit/app-update"], versions.registry?.npm?.["@rightkit/app-update"], ...(versions.legacyNpm?.["@rightkit/app-update"] ?? [])].filter(Boolean));
    assert.ok(publishedUpdaters.has(updater), `${app.key} must consume a published shared updater runtime (got ${updater}; published: ${[...publishedUpdaters].join(", ")})`);
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/right-release"));
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/rightkit/packages/release"));
    assert(!JSON.stringify(pkg).includes("git+https://github.com/adrdsouza/rightkit.git"));
    assert.equal(scripts["release:mac"], undefined, "tierless release entry points are forbidden");
    assert.equal(scripts["release:win"], undefined, "tierless release entry points are forbidden");
    assert.ok(!Object.entries(scripts).some(([name, command]) => /^(?:release|publish):/i.test(name) && /unsigned|--no-sign/i.test(command)), `${app.key} release/publish commands must not expose an unsigned macOS DMG mode`);
    assert.ok(!JSON.stringify(scripts).includes("../tools/right-release"), `${app.key} scripts must not depend on the parent Claude workspace`);
    assert.ok(!JSON.stringify(scripts).includes("../rightkit/packages/release"), `${app.key} scripts must not depend on the parent Claude workspace`);

    const config = (await import(`${pathToFileURL(path.join(root, "right-release.config.mjs"))}?contract=${Date.now()}`)).default;
    assert.equal(config.app, app.key);
    assert.ok(config.version);
    assert.equal(config.distribution?.provider, "r2-rightapps", `${app.key} private release config must select R2 + RightApps explicitly`);
    const disabled = config.releaseReadiness?.state === "disabled";
    // HeardRight's release is re-enabled through the RightKit recipe lane (Adrian, 2026-10-28: HR to R2 through the proper
    // channel): with no releaseReadiness key it falls to the active release contract below; the disabled shape stays pinned only
    // while it is disabled.
    if (app.key === "heardright" && disabled) {
      assert.deepEqual(config.releaseReadiness, { state: "disabled", reason: "Removed build routes pending shared RightKit recipe migration", owner: "RightKit app adoption" });
      assert.deepEqual(config.targets, {}, `${app.key} disabled release must have no targets`);
      // Release stays disabled; development is declared again (heardright 015b1900, 2026-10-27) through shared RightKit owners
      // only: every build step is `pnpm exec right-release …` or `pnpm --dir <app> exec tauri …`, never an app script.
      assert.equal(pkg.scripts, undefined, `${app.key} disabled app must expose no package scripts`);
      for (const file of ["scripts/mac-dmg.mjs", "scripts/build-windows-nsis.mjs"]) {
        assert.equal(existsSync(path.join(root, file)), false, `${app.key} disabled app must not restore ${file}`);
      }
      for (const platform of ["mac", "win"]) {
        const target = config.development?.targets?.[platform];
        assert.ok(Array.isArray(target?.build) && target.build.length, `${app.key} development.targets.${platform}.build must be declared`);
        for (const step of target.build) {
          assert.equal(step.cmd, "pnpm", `${app.key} ${platform} dev step must run through pnpm: ${JSON.stringify(step)}`);
          const owner = step.args.includes("right-release") ? "right-release" : step.args.includes("tauri") ? "tauri" : null;
          assert.ok(owner, `${app.key} ${platform} dev step must call a shared owner (right-release or tauri): ${JSON.stringify(step.args)}`);
        }
        assert.doesNotThrow(() => validateDevelopmentConfig(config, platform, { root }), `${app.key} ${platform} development target must validate`);
      }
      context.diagnostic(`${app.key} release not ready: ${config.releaseReadiness.reason}`);
    } else {
      assert.equal(disabled, false, `${app.key} must satisfy the active release contract`);
    }
    if (!disabled && pkg.scripts) {
      assert.equal(scripts["release:doctor"], "right-release doctor");
      assertReleasePackageScripts(scripts, app.key, "r2-rightapps");
      assert.equal(scripts["deps:check"], "right-release deps --check");
      assert.equal(scripts["deps:update"], "right-release deps --update");
    } else if (!disabled) {
      assert.ok(usesRecipeLane(config), `${app.key} has no package scripts; its release must be declared through RightKit recipes`);
    }
    if (!disabled) {
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
      // Either lane: a declared macPackage object (recipe) or the legacy targets.mac.package entry point.
      if (usesRecipeLane(config)) {
        assertMacPackageRecipe(config.targets.mac, app.key);
        assertWinPackageRecipe(config.targets.win, app.key);
      } else {
        assertMacPackageEntry(config.targets.mac.package, scripts, app.key);
      }
      for (const installer of config.targets.mac.installer.artifacts) {
        assert.equal(path.dirname(installer.file), ".", `${app.key} macOS installer must be copied to the app package root before upload`);
      }
      assert.ok(config.targets.win.sign.files.length);
      const winUpdaterFiles = new Set(config.targets.win.updater.artifacts.map((artifact) => artifact.file));
      for (const signed of config.targets.win.sign.files) assert.ok(winUpdaterFiles.has(signed), `${signed} must be both Azure-signed and updater-signed`);
    }

    const tauri = JSON.parse(readFileSync(path.join(root, app.tauri), "utf8"));
    assert.equal(tauri.plugins.updater.pubkey, pubkey);
    if (app.key === "scraperight") {
      // ScrapeRight is in the registry as a pending native updater (C3-08) and keeps its own host until it migrates.
      assert.deepEqual(tauri.plugins.updater.endpoints, [
        `https://scraperight.cc/v1/apps/${app.key}/releases/latest.json?platform={{target}}`,
      ]);
    } else {
      // The shell supplies the endpoint from @rightkit/app-update/suite-api, so a migrated conf carries none. Until the
      // suite-api-literal gate is wired (after the app migrations), a conf may still carry exactly the contract endpoint;
      // any other endpoint, and the legacy host, are refused now.
      if (tauri.plugins.updater.endpoints !== undefined) {
        assert.deepEqual(tauri.plugins.updater.endpoints, [pendingUpdaterEndpoint(app.key)], `${app.key} plugins.updater.endpoints must be absent or exactly the contract endpoint`);
      }
      const confText = readFileSync(path.join(root, app.tauri), "utf8").split(pendingUpdaterEndpoint(app.key)).join("");
      for (const host of SUITE_API_CONTRACT_HOSTS) {
        assert.ok(!confText.includes(host), `${app.key} ${app.tauri} must not name the Right Suite API host ${host} outside the contract endpoint`);
      }
    }
    assert.equal(tauri.bundle.createUpdaterArtifacts, true);
    if (disabled) {
      assert.equal(tauri.build?.beforeBuildCommand, undefined);
      assert.equal(tauri.build?.beforeDevCommand, undefined);
    }

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
      if (disabled) {
        assert.equal(existsSync(path.join(root, releaseFile)), false, `${app.key} disabled release must not restore ${releaseFile}`);
        continue;
      }
      // Recipe lane: the declared macPackage / winPackage replaces the legacy release script, which may be removed.
      if (usesRecipeLane(config) && !existsSync(path.join(root, releaseFile))) continue;
      const source = readFileSync(path.join(root, releaseFile), "utf8");
      assert.doesNotMatch(source, /(?:\.\.\/)+tools\/right-release|tools\/right-release\//, `${app.key} ${releaseFile} must consume the installed package`);
      // Storage governance: installers go to RIGHT_RELEASE_ARTIFACT_DIR; the legacy root mirror stays accepted.
      assert.match(source, /right-release["']?,?\s*["']mirror-root-artifact|right-release mirror-root-artifact|RIGHT_RELEASE_ARTIFACT_DIR/, `${app.key} ${releaseFile} must write the final DMG to RIGHT_RELEASE_ARTIFACT_DIR (or mirror it to the canonical package root)`);
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

test("HeardRight and ScrapeRight expose one locked ASR promotion adapter contract", { skip: checkoutSkip([
  path.join(workspace, "heardright/tauri-app-next/right-release.config.mjs"),
  path.join(workspace, "scraperight/right-release.config.mjs"),
], "HeardRight and ScrapeRight checkouts") }, async () => {
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
  const crateRoot = path.join(workspace, "rightkit/crates");
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
// enough — gutting/deleting a still-used adapter must also fail here. GenRight/MailRight
// migrated to shared recipes/staging; its replacement route is checked below.
const cargoTargetRootHelpers = [
  "coderight/apps/coderight-tauri/scripts/lib/target-root.mjs",
  "heardright/tauri-app-next/scripts/lib/target-root.mjs",
  "orthic/scripts/lib/target-root.mjs",
  "membrane/apps/membrane-hub/scripts/lib/target-root.mjs",
];

test("ViewRight uses RightKit's declarative Cargo target-root resolver", async (t) => {
  const root = path.join(workspace, "viewright");
  if (!existsSync(path.join(root, "right-release.config.mjs"))) {
    if (process.env.RIGHTKIT_REQUIRE_ASSETS === "1") assert.fail("viewright/right-release.config.mjs is missing");
    t.skip("viewright is not checked out beside rightkit");
    return;
  }
  const config = (await import(`${pathToFileURL(path.join(root, "right-release.config.mjs"))}?target-root-contract=${Date.now()}`)).default;
  assert.equal(config.targets?.mac?.macPackage?.cargoTargetRoot, true);
  assert.equal(config.targets?.win?.winPackage?.cargoTargetRoot, true);
});

for (const app of ["genright", "mailright"]) test(`${app} delegates target resolution to shared recipes and sidecar staging`, { skip: checkoutSkip([
  path.join(workspace, app, "package.json"),
  path.join(workspace, app, "right-release.config.mjs"),
], `${app} checkout`) }, async () => {
  const root = path.join(workspace, app);
  const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  const config = (await import(pathToFileURL(path.join(root, "right-release.config.mjs")).href)).default;
  // These shared owners resolve actual target_directory; no unused local shim is required.
  assert.ok(config.targets?.mac?.macPackage && config.targets?.win?.winPackage);
  const packageStage = pkg.scripts?.["build:engine"] === "right-release stage-sidecar --config right-release.config.mjs";
  const recipeStages = [config.targets.mac, config.targets.win].every((target) =>
    target.externalBin?.stage?.cmd === "right-release"
    && JSON.stringify(target.externalBin.stage.args) === JSON.stringify(["stage-sidecar", "--config", "right-release.config.mjs"]));
  const sidecars = Array.isArray(config.sidecars) ? config.sidecars : config.sidecars?.sidecars ?? [];
  const declarativeSidecars = sidecars.some((sidecar) => sidecar.kind === "cargo-bin" && sidecar.package === `${app}-engine`);
  assert.ok(packageStage || recipeStages || declarativeSidecars, `${app} must delegate sidecar staging to RightKit`);
  assert.ok(sidecars?.some((entry) => entry.kind === "cargo-bin"));
  for (const source of findCargoTargetDirGuardFiles(root)) {
    if (CARGO_TARGET_DIR_GUARD_SKIP_BASENAME.test(path.basename(source))) continue;
    assert.doesNotMatch(readFileSync(source, "utf8"), /["'][^"']*target-root\.mjs["']/,
      `${path.relative(workspace, source)} still refers to the removed target-root adapter`);
  }
});

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
        ".agent", ".audit", ".build", ".cache", ".git", ".right-release", "bakeoff", "dist", "docs",
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

// This allowlist names only committed workflows: verification, manual evidence collection, reviewed
// Windows build exception, sealed-archive qualification, and reviewed-candidate promotion. Product repositories host none.
const RIGHTKIT_VERIFICATION_WORKFLOWS = ["android.yml", "chrome-pins.yml", "ci.yml"];
const RIGHTKIT_EXCEPTION_WORKFLOWS = ["ffmpeg-windows.yml"];
const RIGHTKIT_EVIDENCE_WORKFLOWS = ["qualify-fs.yml", "qualify-runtime.yml", "qualify-existing-swift.yml", "registry-readback.yml", "reviewed-registry-readback.yml", "supplement-archive-scan.yml", "chrome-native-diagnostic.yml", "repair-hygiene.yml"];
// Owner-authorized registry administration (yank / deprecate) with registry readback; never publishes.
const RIGHTKIT_ADMIN_WORKFLOWS = ["registry-admin.yml"];
// The trusted publisher for every app (Adrian, 2026-10-25: one door, publication only from the protected environment).
const RIGHTKIT_APP_PUBLISHING_WORKFLOWS = ["publish-app.yml"];
const RIGHTKIT_QUALIFICATION_WORKFLOWS = ["publish.yml"];
const RIGHTKIT_PUBLISHING_WORKFLOWS = ["publish-reviewed.yml"];
// Direct publication (Adrian, 2026-10-22): a green publish.yml qualification run on main uploads its own sealed bytes.
const RIGHTKIT_DIRECT_PUBLISHING_WORKFLOWS = ["publish-direct.yml"];
const RIGHTKIT_SWIFT_PUBLISHING_WORKFLOWS = ["publish-swift.yml"];
const publishWorkflowPresent = existsSync(new URL("../../.github/workflows/publish.yml", import.meta.url));
const reviewedWorkflowPresent = existsSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url));
const PUBLISHING_WORKFLOW = /secrets\.|right-release\s+(?:build|upload|publish)|gh\s+release|action-gh-release/;
function pinnedActionLines(text, name) {
  const usesLines = text.split("\n").filter((line) => /^\s*(?:- )?uses:/.test(line));
  assert.ok(usesLines.length > 0, `${name} must use at least one pinned action`);
  for (const line of usesLines) {
    if (/uses:\s+\.\/\.github\/actions\/rk-setup\s*$/.test(line)) continue;
    assert.match(line, /@[0-9a-f]{40}(?:\s|$)/, `${name} action must be pinned to a 40-hex commit SHA: ${line.trim()}`);
  }
  const checkouts = usesLines.filter((line) => /actions\/checkout@/.test(line)).length;
  assert.equal((text.match(/persist-credentials:\s*false/g) ?? []).length, checkouts, `${name} every checkout must set persist-credentials: false`);
}
function assertManualBuildWorkflow(text, name) {
  assert.match(text, /^on:\n  workflow_dispatch:/m, `${name} must be triggered by workflow_dispatch`);
  assert.doesNotMatch(text, /^  (?:push|pull_request|pull_request_target|schedule|workflow_run|release):/m, `${name} must have no automatic trigger`);
  assert.match(text, /^permissions:\n  contents: read\s*$/m, `${name} top-level permissions must be contents: read`);
  assert.doesNotMatch(text, /secrets\./, `${name} must not reference secrets`);
  assert.doesNotMatch(text, /pull_request_target/, `${name} must not use pull_request_target`);
  pinnedActionLines(text, name);
}
// Qualification and promotion use separate parsed contracts with independent mutation sets.
function assertPublishWorkflow(text, name) {
  assert.match(text, /^on:\n  workflow_dispatch:/m, `${name} must be triggered by workflow_dispatch`);
  assertPublishWorkflowContract(text, name);
  const survivors = proveMutations(parseYaml(text));
  assert.deepEqual(survivors, [], `${name}: the contract accepts these violations`);
}
function assertReviewedWorkflow(text, name) {
  assert.match(text, /^on:\n  workflow_dispatch:/m, `${name} must be triggered by workflow_dispatch`);
  assertReviewedDoc(parseYaml(text));
  for (const [label, mutate] of REVIEWED_MUTATIONS) {
    const copy = structuredClone(parseYaml(text));
    mutate(copy);
    assert.throws(() => assertReviewedDoc(copy), undefined, `${name}: ${label}`);
  }
}
/** Direct publication: triggered only by a completed publish.yml run on main, uploads only when that run succeeded from a
 * workflow_dispatch, checks out the qualified commit itself, top-level permissions read-only, no manual or push trigger. */
function assertDirectPublishWorkflow(text, name) {
  assert.match(text, /^on:\n  workflow_run:\n    workflows: \[publish\]\n    types: \[completed\]\n    branches: \[main\]\n/m, `${name} must run only after publish.yml completes on main`);
  assert.doesNotMatch(text, /^  (?:push|pull_request|pull_request_target|schedule|workflow_dispatch|release):/m, `${name} must have no other trigger`);
  assert.match(text, /^permissions:\n  contents: read\n/m, `${name} top-level permissions must be contents: read only`);
  assert.match(text, /if: \$\{\{ github\.event\.workflow_run\.conclusion == 'success' && github\.event\.workflow_run\.event == 'workflow_dispatch' && github\.event\.workflow_run\.head_branch == 'main'/, `${name} must upload only a successful workflow_dispatch qualification of main`);
  assert.match(text, /ref: \$\{\{ github\.event\.workflow_run\.head_sha \}\}/, `${name} must check out the qualified commit, never the default branch`);
  assert.match(text, /persist-credentials: false/, `${name} must not persist checkout credentials`);
  assert.doesNotMatch(text, /pull_request_target/, `${name} must not use pull_request_target`);
  pinnedActionLines(text, name);
}
function assertEvidenceWorkflow(text, name) {
  assert.match(text, /^on:\n  workflow_dispatch:/m, `${name} must be manually dispatched`);
  assert.doesNotMatch(text, /^  (?:push|pull_request|pull_request_target|schedule|workflow_run|release):/m, `${name} must have no automatic trigger`);
  // This strict repository parser intentionally omits flow mappings. Empty permissions are
  // equivalent to an empty sequence for this read-only check: both grant no permission.
  const doc = parseYaml(text.replace(/^(\s*permissions): \{\}$/gm, "$1: []"));
  assert.ok(doc.permissions && typeof doc.permissions === "object", `${name} must declare top-level permissions`);
  const permissionsAreReadOnly = (value) => {
    if (!value || typeof value !== "object") return true;
    if (Array.isArray(value)) return value.every(permissionsAreReadOnly);
    return Object.entries(value).every(([key, nested]) => key === "permissions"
      ? nested && typeof nested === "object" && Object.values(nested).every((scope) => scope === "read")
      : permissionsAreReadOnly(nested));
  };
  assert.ok(permissionsAreReadOnly(doc), `${name} workflow and job permissions must be read-only`);
  assert.doesNotMatch(text, /(?:NODE_AUTH_TOKEN|CARGO_REGISTRY_TOKEN|secrets\.|id-token:\s*write)/, `${name} must not receive publication credentials`);
  assert.doesNotMatch(text, /pull_request_target/, `${name} must not use pull_request_target`);
  pinnedActionLines(text, name);
}
// publish-app: dispatch only; one job in the protected app-publication environment; class A before publication; the staged archive
// unpacked outside the checkout with links and foreign paths refused; release only through the door; secrets only in that job.
function assertAppPublishWorkflow(text, name) {
  const doc = parseYaml(text);
  assert.deepEqual(Object.keys(doc.on ?? {}), ["workflow_dispatch"], `${name}: dispatch only`);
  assert.deepEqual(doc.permissions, { contents: "read" }, `${name}: read-only token`);
  assert.deepEqual(Object.keys(doc.jobs), ["publish"], `${name}: one job`);
  const job = doc.jobs.publish;
  assert.equal(job.environment, "app-publication", `${name}: protected environment`);
  const runs = job.steps.map((step) => step.run ?? "").join("\n");
  assert.ok(runs.indexOf("rightkit-gate check --commit HEAD") >= 0 && runs.indexOf("rightkit-gate check --commit HEAD") < runs.indexOf("rightkit-gate --to release"), `${name}: class A before release`);
  assert.match(runs, /unpack="\$RUNNER_TEMP\/unpack-\$platform"/, `${name}: unpacked outside the checkout`);
  assert.match(runs, /! -type f ! -type d/, `${name}: links and special files refused`);
  assert.match(runs, /RIGHTKIT_PUBLISHER|--to release/, name);
  assert.doesNotMatch(runs, /right-release (upload|build|publish)/, `${name}: nothing reaches right-release around the door`);
  assert.equal(job.steps.find((step) => /--to release/.test(step.run ?? ""))?.env?.RIGHTKIT_PUBLISHER, "github-environment", name);
}

function assertAdminWorkflow(text, name) {
  assert.match(text, /^on:\n  workflow_dispatch:/m, `${name} must be manually dispatched`);
  assert.doesNotMatch(text, /^  (?:push|pull_request|pull_request_target|schedule|workflow_run|release):/m, `${name} must have no automatic trigger`);
  assert.match(text, /^permissions: \{\}$/m, `${name} top-level permissions must be empty`);
  assert.doesNotMatch(text, /id-token:|contents:\s*write|packages:\s*write/, `${name} must not request write or OIDC permissions`);
  assert.match(text, /if: \$\{\{ github\.ref == 'refs\/heads\/main' && github\.repository == 'bogusyogi\/rightkit' \}\}/, `${name} must run only on main of this repository`);
  assert.deepEqual([...new Set(text.match(/secrets\.[A-Z_]+/g) ?? [])].sort(), ["secrets.CARGO_REGISTRY_TOKEN", "secrets.NPM_TOKEN"], `${name} may read only the registry tokens`);
  assert.doesNotMatch(text, PUBLISHING_WORKFLOW_COMMANDS, `${name} must not publish or release`);
  assert.doesNotMatch(text, /pull_request_target/, `${name} must not use pull_request_target`);
  pinnedActionLines(text, name);
}
const PUBLISHING_WORKFLOW_COMMANDS = /right-release\s+(?:build|upload|publish)|gh\s+release|action-gh-release|npm\s+publish|cargo\s+publish/;
test("Right Suite hosts no hosted workflow except rightkit's reviewed workflows", () => {
  for (const root of ["viewright", "scraperight", "heardright", "mailright", "coderight", "genright", "voiceright", "rightkit"]) {
    const workflowDir = path.join(workspace, root, ".github", "workflows");
    const workflows = existsSync(workflowDir) ? readdirSync(workflowDir) : [];
    if (root !== "rightkit") {
      assert.deepEqual(workflows, [], `${root} must not contain hosted workflow files`);
      continue;
    }
    const allowed = [...RIGHTKIT_VERIFICATION_WORKFLOWS, ...RIGHTKIT_EVIDENCE_WORKFLOWS, ...RIGHTKIT_EXCEPTION_WORKFLOWS, ...RIGHTKIT_QUALIFICATION_WORKFLOWS, ...RIGHTKIT_PUBLISHING_WORKFLOWS, ...RIGHTKIT_DIRECT_PUBLISHING_WORKFLOWS, ...RIGHTKIT_SWIFT_PUBLISHING_WORKFLOWS, ...RIGHTKIT_ADMIN_WORKFLOWS, ...RIGHTKIT_APP_PUBLISHING_WORKFLOWS];
    assert.deepEqual(workflows.filter((name) => !allowed.includes(name)), [], `rightkit may host only ${allowed.join(", ")}; any other hosted workflow needs an explicit decision`);
    for (const name of workflows) {
      const text = readFileSync(path.join(workflowDir, name), "utf8").replace(/\r\n/g, "\n");
      if (RIGHTKIT_QUALIFICATION_WORKFLOWS.includes(name)) { assertPublishWorkflow(text, name); continue; }
      if (RIGHTKIT_PUBLISHING_WORKFLOWS.includes(name)) { assertReviewedWorkflow(text, name); continue; }
      if (RIGHTKIT_DIRECT_PUBLISHING_WORKFLOWS.includes(name)) { assertDirectPublishWorkflow(text, name); continue; }
      if (RIGHTKIT_ADMIN_WORKFLOWS.includes(name)) { assertAdminWorkflow(text, name); continue; }
      if (RIGHTKIT_APP_PUBLISHING_WORKFLOWS.includes(name)) { assertAppPublishWorkflow(text, name); continue; }
      if (RIGHTKIT_SWIFT_PUBLISHING_WORKFLOWS.includes(name)) {
        const doc = parseYaml(text);
        assertSwiftDoc(doc);
        assert.deepEqual(proveSwiftMutations(doc), [], `${name}: the contract accepts these violations`);
        continue;
      }
      assert.doesNotMatch(text, PUBLISHING_WORKFLOW, `${name} must not publish or read secrets`);
      if (RIGHTKIT_EXCEPTION_WORKFLOWS.includes(name)) assertManualBuildWorkflow(text, name);
      if (RIGHTKIT_EVIDENCE_WORKFLOWS.includes(name)) assertEvidenceWorkflow(text, name);
    }
  }
});

const publishWorkflowText = () => readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8").replace(/\r\n/g, "\n");

test("publish.yml: every security property is asserted on the parsed workflow and each violation is rejected", { skip: !publishWorkflowPresent && "no publish.yml (#342)" }, () => {
  const doc = parseYaml(publishWorkflowText());
  assertPublishWorkflowContract(publishWorkflowText(), "publish.yml");
  assert.deepEqual(VERIFY_HOSTS, ["windows-2025", "xcode-27"]);
  assert.deepEqual([...doc.jobs.verify.strategy.matrix.os].sort(), VERIFY_HOSTS);
  assert.deepEqual([...doc.jobs.verify.strategy.matrix.os].sort(), ["windows-2025", "xcode-27"], "qualification must prove both native hosts");
  assert.equal(doc.jobs.upload, undefined, "qualification must not upload");
  assert.equal(doc.jobs.qualified.needs.includes("verify"), true, "qualification result gate must require both verify hosts");
  assert.equal(doc.jobs.pack.environment, undefined, "qualification pack must not join release environment");
  assert.ok(MUTATIONS.length >= 30, "the mutation set must stay broad");
  assert.deepEqual(proveMutations(doc), []);
});

test("publish.yml: named violations fail the contract with the property they break", { skip: !publishWorkflowPresent && "no publish.yml (#342)" }, () => {
  const expectations = [
    ["drop the Windows verify host", /verify must run on exactly/],
    ["qualification final gate loses verify dependency", /qualified must always inspect guard, plan, pack, both-host verify & Linux verify results/],
    ["continue-on-error on a step", /continue-on-error is forbidden/],
    ["guard allows another branch", /refs\/heads\/main/],
    ["verify requests id-token", /verify must not request id-token/],
    ["pack reads a secret", /pack must not reference the secrets context in any form/],
    ["pack joins the release environment", /pack must not use an environment/],
    ["pack builds the whole workspace", /pack must not build the whole workspace/],
    ["qualification regains upload", /jobs must be exactly/],
    ["qualification regains approval before pack", /qualification cannot execute publication code/],
    ["verify reads a secret", /verify must not reference the secrets context in any form/],
    ["verify may be skipped by a condition", /verify's condition must be exactly/],
    ["pack condition uses always() instead of !cancelled()", /pack's condition must be exactly/],
    ["pack condition drops !cancelled()", /pack's condition must be exactly/],
    ["verify condition uses always() instead of !cancelled()", /verify's condition must be exactly/],
    ["verify condition drops !cancelled()", /verify's condition must be exactly/],
    ["verify condition drops needs.pack.result == 'success'", /verify's condition must be exactly/],
  ];
  const doc = parseYaml(publishWorkflowText());
  for (const [name, pattern] of expectations) {
    const mutation = MUTATIONS.find(([label]) => label === name);
    assert.ok(mutation, `mutation ${name} must exist`);
    const copy = structuredClone(doc);
    mutation[1](copy);
    assert.throws(() => assertPublishDoc(copy, "publish.yml"), pattern, name);
  }
});

const reviewedWorkflowText = () => readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8").replace(/\r\n/g, "\n");
test("publish-reviewed.yml: promotion alone owns release environment and sparse credentialed uploader", { skip: !reviewedWorkflowPresent && "no publish-reviewed.yml" }, () => {
  const doc = parseYaml(reviewedWorkflowText());
  assertReviewedDoc(doc);
  assert.deepEqual(doc.jobs.upload.steps[0].with["sparse-checkout"].split("\n").filter(Boolean), ["scripts/ci/upload.mjs", "scripts/ci/registry-spec.mjs", "scripts/ci/reviewed-candidate.mjs", "scripts/ci/supplemental-proof.mjs", "ci/approved-artifacts.json", "ci/supplemental-proofs.json", "ci/linux-required.json"]);
  assert.equal(doc.jobs.upload.environment, "release");
  assert.equal(doc.jobs["seal-check"].environment, undefined);
  assert.ok(REVIEWED_MUTATIONS.length >= 25, "promotion mutation set must stay broad");
  for (const [name, mutate] of REVIEWED_MUTATIONS) {
    const copy = structuredClone(doc);
    mutate(copy);
    assert.throws(() => assertReviewedDoc(copy), undefined, name);
  }
});

test("RightApps brand hosts expose app-keyed update manifest proxies", { skip: checkoutSkip([path.join(workspace, siteRepoDir)], "RightApps/RightSites checkout") }, () => {
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
