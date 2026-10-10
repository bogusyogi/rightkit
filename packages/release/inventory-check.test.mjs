import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  ACTION,
  DEFAULT_REPO_ROOT,
  INFO,
  assessParity,
  baseVersion,
  behindKind,
  cargoDeclarations,
  createFetcher,
  crateIndexPath,
  decideExit,
  duplicateCargoVersions,
  latestStable,
  mapLimit,
  npmDeclarations,
  pnpmPackageVersions,
  renderMarkdown,
  runInventoryCheck,
  satisfies,
} from "./inventory-check.mjs";

const json = (body, status = 200) => new Response(JSON.stringify(body), { status });
const text = (body, status = 200) => new Response(body, { status });

test("satisfies follows Cargo caret, tilde, exact, and wildcard rules", () => {
  assert.equal(satisfies("0.2.0", "0.2.2", "cargo"), true);
  assert.equal(satisfies("=0.2.0", "0.2.2", "cargo"), false);
  assert.equal(satisfies("1.2", "2.0.0", "cargo"), false);
  assert.equal(satisfies("^1.2.3", "1.9.0", "cargo"), true);
  assert.equal(satisfies("0.1", "0.2.0", "cargo"), false);
  assert.equal(satisfies("~1.2.3", "1.3.0", "cargo"), false);
  assert.equal(satisfies("*", "9.9.9", "cargo"), true);
});

test("satisfies follows npm exact, x-range, OR and hyphen forms", () => {
  assert.equal(satisfies("1.2.3", "1.2.4", "npm"), false);
  assert.equal(satisfies("1.x", "1.9.0", "npm"), true);
  assert.equal(satisfies("^1.0.0 || ^2.0.0", "2.1.0", "npm"), true);
  assert.equal(satisfies(">= 1.0.0 < 2.0.0", "1.5.0", "npm"), true);
  assert.equal(satisfies("1.2.3 - 2.0.0", "2.0.0", "npm"), true);
  assert.equal(satisfies("1.2.3 - 2.0.0", "2.0.1", "npm"), false);
});

test("satisfies returns null for syntax it does not model", () => {
  assert.equal(satisfies("latest", "1.0.0", "npm"), null);
  assert.equal(satisfies("file:../x", "1.0.0", "npm"), null);
});

test("baseVersion and behindKind classify the lower bound", () => {
  assert.equal(baseVersion("^0.2.0", "cargo"), "0.2.0");
  assert.equal(baseVersion("=1.0.0", "cargo"), "1.0.0");
  assert.equal(behindKind("1.2", "2.0.0", "cargo"), "major");
  assert.equal(behindKind("0.1", "0.2.0", "cargo"), "minor");
  assert.equal(behindKind("1.0.0", "1.0.3", "npm"), "patch");
});

test("latestStable skips yanked, deprecated, and prerelease versions", () => {
  assert.equal(latestStable([
    { version: "1.0.0" },
    { version: "1.1.0", yanked: true },
    { version: "1.2.0", deprecated: "use 2" },
    { version: "2.0.0-rc.1" },
  ]), "1.0.0");
});

test("crateIndexPath uses the sparse index prefix layout", () => {
  assert.equal(crateIndexPath("a"), "1/a");
  assert.equal(crateIndexPath("ab"), "2/ab");
  assert.equal(crateIndexPath("abc"), "3/a/abc");
  assert.equal(crateIndexPath("rightkit-fs"), "ri/gh/rightkit-fs");
});

test("mapLimit never runs more than the limit at once", async () => {
  let inFlight = 0;
  let peak = 0;
  const out = await mapLimit(Array.from({ length: 20 }, (_, i) => i), 8, async (i) => {
    inFlight++;
    peak = Math.max(peak, inFlight);
    await new Promise((r) => setTimeout(r, 2));
    inFlight--;
    return i * 2;
  });
  assert.equal(peak, 8);
  assert.deepEqual(out, Array.from({ length: 20 }, (_, i) => i * 2));
});

test("createFetcher serves a repeat request from the cache and never caches 5xx", async () => {
  const dir = await mkdtemp(join(tmpdir(), "inventory-cache-test-"));
  try {
    let calls = 0;
    const fetchImpl = async (url) => {
      calls++;
      if (url.includes("broken")) return text("boom", 500);
      return text("ok", 200);
    };
    const get = createFetcher({ fetchImpl, cacheDir: dir, crateApiIntervalMs: 0 });
    assert.equal((await get("https://registry.npmjs.org/a")).text, "ok");
    assert.equal((await get("https://registry.npmjs.org/a")).text, "ok");
    assert.equal(calls, 1);
    await assert.rejects(get("https://registry.npmjs.org/broken"), /HTTP 500/);
    await assert.rejects(get("https://registry.npmjs.org/broken"), /HTTP 500/);
    assert.equal(calls, 3);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("assessParity flags unpublished, stale-feed, newer-registry and yanked cases", () => {
  const base = { ecosystem: "cargo", name: "rightkit-x", isPublic: true, version: "0.3.0" };
  const never = assessParity({ ...base, live: { versions: [], latest: null } });
  assert.deepEqual(never.findings.map((f) => f.kind), ["never-published"]);

  const unpublished = assessParity({
    ...base, staged: "0.3.0", recorded: "0.2.0",
    live: { versions: [{ version: "0.2.0" }], latest: "0.2.0" },
  });
  assert.ok(unpublished.findings.some((f) => f.kind === "source-unpublished" && f.severity === ACTION));

  const newer = assessParity({
    ...base, version: "0.2.0", recorded: "0.2.0",
    live: { versions: [{ version: "0.2.0" }, { version: "0.2.1" }], latest: "0.2.1" },
  });
  const kinds = newer.findings.map((f) => f.kind);
  assert.ok(kinds.includes("registry-newer-than-source"));
  assert.ok(kinds.includes("feed-out-of-date"));

  const yanked = assessParity({
    ...base, version: "0.2.0", recorded: "0.2.0",
    live: { versions: [{ version: "0.2.0", yanked: true }, { version: "0.1.0" }], latest: "0.1.0" },
  });
  assert.ok(yanked.findings.some((f) => f.kind === "yanked"));

  const privateButLive = assessParity({ ...base, isPublic: false, live: { versions: [{ version: "1.0.0" }], latest: "1.0.0" } });
  assert.ok(privateButLive.findings.some((f) => f.kind === "published-but-private"));

  const privateOnly = assessParity({ ...base, isPublic: false, live: { versions: [], latest: null } });
  assert.deepEqual(privateOnly.findings.map((f) => [f.kind, f.severity]), [["private-crate", INFO]]);
});

test("assessParity treats a Swift lookup failure as informational", () => {
  const { findings } = assessParity({
    ecosystem: "swift", name: "RightKitSwift", isPublic: true, version: "0.3.8", liveError: "GitHub tags unavailable",
  });
  assert.equal(findings[0].kind, "unverified");
  assert.equal(findings[0].severity, INFO);
});

test("cargo and npm declarations skip local, git, and workspace-inherited entries", () => {
  const manifest = {
    dependencies: {
      serde: "1.0",
      local: { path: "../x" },
      inherited: { workspace: true },
      renamed: { package: "real-name", version: "2" },
    },
    "dev-dependencies": { tokio: { version: "1.40" } },
  };
  assert.deepEqual(cargoDeclarations(manifest).map((d) => d.name), ["serde", "real-name", "tokio"]);
  const pkg = { dependencies: { a: "^1.0.0", b: "workspace:*", c: "github:x/y", d: "file:../z" }, devDependencies: { e: "~2.0.0" } };
  assert.deepEqual(npmDeclarations(pkg).map((d) => d.name), ["a", "e"]);
});

test("lock parsers report multiple versions of one name", () => {
  const cargoLock = [
    '[[package]]\nname = "serde"\nversion = "1.0.1"\n',
    '[[package]]\nname = "serde"\nversion = "1.0.2"\n',
    '[[package]]\nname = "rightkit-fs"\nversion = "0.2.0"\n',
  ].join("\n");
  assert.deepEqual(duplicateCargoVersions(cargoLock), [{ name: "serde", versions: ["1.0.1", "1.0.2"] }]);
  const pnpm = [
    "lockfileVersion: '9.0'", "", "importers:", "  .:", "    dependencies:", "      '@rightkit/git':",
    "packages:", "", "  '@rightkit/git@0.2.26':", "    resolution: {}", "  '@rightkit/git@0.2.20':", "    resolution: {}",
    "  '@rightkit/fs@0.1.0(react@18.3.1)':", "    resolution: {}", "snapshots:", "",
  ].join("\n");
  assert.deepEqual(pnpmPackageVersions(pnpm, (n) => n.startsWith("@rightkit/")), [
    { name: "@rightkit/git", versions: ["0.2.20", "0.2.26"] },
  ]);
});

test("decideExit fails on action items unless warn-only", () => {
  const report = { counts: { action: 2, info: 0 } };
  assert.equal(decideExit(report), 1);
  assert.equal(decideExit(report, true), 0);
  assert.equal(decideExit({ counts: { action: 0, info: 4 } }), 0);
});

test("end-to-end run on the real repo with a fully mocked registry", async () => {
  const appsRoot = await mkdtemp(join(tmpdir(), "inventory-apps-test-"));
  try {
    const appDir = join(appsRoot, "demo-app");
    await mkdir(appDir, { recursive: true });
    await writeFile(join(appDir, "Cargo.lock"), [
      '[[package]]\nname = "rightkit-fs"\nversion = "0.1.0"\n',
      '[[package]]\nname = "rightkit-fs"\nversion = "0.2.0"\n',
    ].join("\n"));
    await writeFile(join(appDir, "package.json"), JSON.stringify({ name: "demo-app", dependencies: { "@rightkit/git": "0.1.0" } }));

    const requested = [];
    const get = async (url) => {
      requested.push(url);
      if (url.startsWith("https://index.crates.io/") || url.startsWith("https://registry.npmjs.org/")) {
        return { status: 404, text: "" };
      }
      return { status: 404, text: "" };
    };

    const report = await runInventoryCheck({ repoRoot: DEFAULT_REPO_ROOT, appsRoot, apps: ["demo-app"], get });
    assert.equal(report.schema, 1);
    assert.ok(report.counts.action > 0, "a mocked empty registry must produce action items");
    assert.ok(requested.length > 0);
    assert.ok(requested.every((url) => !url.includes("github.com") || url.includes("api.github.com")));

    const kinds = new Set(report.findings.map((f) => f.kind));
    assert.ok(kinds.has("never-published"));
    assert.ok(kinds.has("duplicate-app-copies"), "lock-file duplicate in demo-app");
    // rightkit-review gained a README (6a171cb9); the missing-readme kind is covered by the fixture test below.
    const swift = report.findings.find((f) => f.subject === "swift RightKitSwift" && f.kind === "unverified");
    assert.equal(swift?.severity, INFO);

    const markdown = renderMarkdown(report);
    assert.match(markdown, /# RightKit inventory status/);
    assert.match(markdown, /## 1\. Registry parity/);
  } finally {
    await rm(appsRoot, { recursive: true, force: true });
  }
});
