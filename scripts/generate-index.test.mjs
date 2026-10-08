// node --test tools/rightkit/scripts/generate-index.test.mjs
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  DEFAULT_ROOT, OUTPUTS, cargoDependencies, generate, isExactPin, npmDependencies, parseToml, pinStatus, satisfies,
} from "./generate-index.mjs";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "generate-index.mjs");
const tmpDirs = [];
after(() => { for (const dir of tmpDirs) rmSync(dir, { recursive: true, force: true }); });

const write = (file, content) => {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, typeof content === "string" ? content : JSON.stringify(content, null, 2));
};

// A RightKit root with one crate, one private crate, one npm package and a version and ownership catalog,
// plus a workspace with three app repos (one missing), a vendored copy and a node_modules copy to skip.
function fixture() {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-generate-"));
  tmpDirs.push(base);
  const root = path.join(base, "rightkit");
  const workspace = path.join(base, "apps");
  write(path.join(root, "crates/alpha-crate/Cargo.toml"), '[package]\nname = "alpha-crate"\nversion = "0.2.0"\ndescription = "Alpha."\n');
  write(path.join(root, "crates/beta-private/Cargo.toml"), '[package]\nname = "beta-private"\nversion = "0.1.0"\npublish = false\n');
  write(path.join(root, "packages/gamma/package.json"), { name: "@fx/gamma", version: "1.3.0", description: "Gamma." });
  write(path.join(root, "packages/release/rightkit-versions.json"), {
    schema: 2,
    npm: { "@fx/gamma": "1.0.0" },
    stagedNpm: { "@fx/gamma": "1.3.0" },
    cargo: { "alpha-crate": "0.1.0" },
    registry: { cargo: { "alpha-crate": "0.2.0" } },
    stagedCargo: {},
  });
  write(path.join(root, "packages/release/rightkit-ownership.json"), {
    schema: 1,
    version: "3.0.0",
    capabilities: [
      { package: "alpha-crate", capability: "alpha", summary: "Alpha", detection: "declaration-only", fingerprints: [] },
      {
        package: "@fx/gamma", capability: "gamma-ui", summary: "Gamma", detection: "fingerprinted",
        fingerprints: [{ id: "gamma.left-pad", type: "dependency", ecosystem: "npm", name: "left-pad", tier: "advise", usageContext: {}, tierReason: "test" }],
      },
    ],
    internalOwners: [],
  });
  write(path.join(workspace, "app1/Cargo.toml"), '[package]\nname = "app1"\nversion = "1.0.0"\n\n[workspace.dependencies]\nalpha-crate = "0.1.0"\n\n[dependencies]\nbeta-private = { path = "../rightkit/crates/beta-private" }\n');
  write(path.join(workspace, "app1/crates/member/Cargo.toml"), '[package]\nname = "member"\nversion = "1.0.0"\n\n[dependencies]\nalpha-crate = { workspace = true }\n');
  write(path.join(workspace, "app1/crates/other/Cargo.toml"), '[package]\nname = "other"\nversion = "1.0.0"\n\n[dependencies]\nalpha-crate = "=0.1.0"\n');
  write(path.join(workspace, "app1/vendor/alpha-crate/Cargo.toml"), '[package]\nname = "alpha-crate"\nversion = "0.2.0"\n');
  write(path.join(workspace, "app1/package.json"), { name: "app1", dependencies: { "@fx/gamma": "^1.0.0" } });
  write(path.join(workspace, "app1/node_modules/@fx/gamma/package.json"), { name: "@fx/gamma", version: "9.9.9", dependencies: { "@fx/gamma": "=9.9.9" } });
  write(path.join(workspace, "app2/package.json"), {
    name: "app2",
    dependencies: { "@fx/gamma": "1.3.0" },
    devDependencies: { "gamma-alias": "npm:@fx/gamma@^1.0.0" },
  });
  return { root, workspace, apps: ["app1", "app2", "app3-missing"] };
}

test("generate writes all three documents with flags, pins and capabilities", () => {
  const { root, workspace, apps } = fixture();
  const out = generate({ root, workspace, apps });
  assert.deepEqual(Object.keys(out).sort(), [...OUTPUTS].sort());

  const index = out["INDEX.md"];
  assert.ok(index.includes("| `alpha-crate` | crates.io | 0.2.0 | 0.2.0 |  | ok | alpha | Alpha. |"), "published equals source");
  assert.ok(index.includes("| `beta-private` | private | 0.1.0 | none recorded |  | private |  |"), "private crate without a record");
  assert.ok(index.includes("| `@fx/gamma` | npm | 1.3.0 | 1.0.0 | 1.3.0 | staged, not yet published |  | gamma-ui | Gamma. |"), "staged ahead of published");
  assert.match(index, /Flagged mismatches: 0\./);

  const adoption = out["docs/ADOPTION.md"];
  assert.ok(adoption.includes("| `alpha-crate` | app1 | `app1/Cargo.toml` | workspace.dependencies | `0.1.0` | 0.2.0 | stale |"), "workspace pin 0.1.0 is stale against 0.2.0");
  assert.ok(adoption.includes("| `alpha-crate` | app1 | `app1/crates/other/Cargo.toml` | dependencies | `=0.1.0` | 0.2.0 | stale |"), "exact old pin is stale");
  assert.ok(adoption.includes("| `beta-private` | app1 | `app1/Cargo.toml` | dependencies | `*` | none recorded | local |"), "path dependency is local");
  assert.ok(adoption.includes("| `@fx/gamma` | app1 | `app1/package.json` | dependencies | `^1.0.0` | 1.0.0 | range ok |"));
  assert.ok(adoption.includes("| `@fx/gamma` | app2 | `app2/package.json` | dependencies | `1.3.0` | 1.0.0 | ahead |"));
  assert.ok(adoption.includes("| `@fx/gamma` | app2 | `app2/package.json` | devDependencies | `^1.0.0` | 1.0.0 | range ok |"));
  assert.ok(adoption.includes("| `alpha-crate` | app1 | `app1/vendor/alpha-crate/Cargo.toml` | 0.2.0 | 0.2.0 |"), "vendored copy listed");
  assert.ok(adoption.includes("| app3-missing | **missing** | 0 | 0 | 0 |"), "missing app is reported");
  assert.ok(!adoption.includes("9.9.9"), "node_modules copies are skipped");
  assert.match(adoption, /Split pins:[\s\S]*`@fx\/gamma` \| npm \| app2 \| 1\.3\.0; \^1\.0\.0/);

  const capabilities = out["docs/CAPABILITY-MAP.md"];
  assert.ok(capabilities.includes("| gamma-ui | `@fx/gamma` | advise | fingerprinted | 1 (0 block, 1 advise) | @fx/gamma: published 1.0.0 (npm) |"));
  assert.ok(capabilities.includes("| alpha | `alpha-crate` | none | declaration-only | 0 | alpha-crate: published 0.2.0 (crates.io) |"));
});

test("generation is deterministic", () => {
  const fx = fixture();
  assert.deepEqual(generate(fx), generate(fx));
});

test("--check exits 0 for current repo-local files, 1 after drift, and names the drifted file", () => {
  const { root, workspace } = fixture();
  const args = ["--root", root, "--workspace", workspace];
  const written = spawnSync(process.execPath, [script, ...args], { encoding: "utf8" });
  assert.equal(written.status, 0, written.stderr);
  for (const rel of OUTPUTS) assert.ok(readFileSync(path.join(root, rel), "utf8").length > 0);

  const current = spawnSync(process.execPath, [script, "--check", ...args], { encoding: "utf8" });
  assert.equal(current.status, 0, current.stderr);

  // --check ignores the app repos entirely: a missing workspace does not matter.
  const noApps = spawnSync(process.execPath, [script, "--check", "--root", root, "--workspace", path.join(root, "no-such-apps")], { encoding: "utf8" });
  assert.equal(noApps.status, 0, noApps.stderr);

  const indexFile = path.join(root, "INDEX.md");
  writeFileSync(indexFile, readFileSync(indexFile, "utf8") + "\nhand edit\n");
  const drifted = spawnSync(process.execPath, [script, "--check", ...args], { encoding: "utf8" });
  assert.equal(drifted.status, 1);
  assert.match(drifted.stderr, /DRIFT INDEX\.md/);
  assert.ok(!drifted.stderr.includes("DRIFT docs/ADOPTION.md"), "--check does not cover ADOPTION.md");

  rmSync(path.join(root, "docs/CAPABILITY-MAP.md"));
  const absent = spawnSync(process.execPath, [script, "--check", ...args], { encoding: "utf8" });
  assert.equal(absent.status, 1);
  assert.match(absent.stderr, /DRIFT docs\/CAPABILITY-MAP\.md/);
});

test("--check-adoption compares only ADOPTION.md against the app repos", () => {
  const { root, workspace } = fixture();
  const args = ["--root", root, "--workspace", workspace];
  assert.equal(spawnSync(process.execPath, [script, ...args], { encoding: "utf8" }).status, 0);
  const current = spawnSync(process.execPath, [script, "--check-adoption", ...args], { encoding: "utf8" });
  assert.equal(current.status, 0, current.stderr);
  writeFileSync(path.join(root, "docs/ADOPTION.md"), "stale\n");
  const drifted = spawnSync(process.execPath, [script, "--check-adoption", ...args], { encoding: "utf8" });
  assert.equal(drifted.status, 1);
  assert.match(drifted.stderr, /DRIFT docs\/ADOPTION\.md/);
});

test("app scan skips dot-directories, node_modules, target and .cache", () => {
  const { root, workspace, apps } = fixture();
  write(path.join(workspace, "app1/.worktrees/copy/package.json"), { name: "app1-copy", dependencies: { "@fx/gamma": "=9.0.0" } });
  write(path.join(workspace, "app1/.claude/package.json"), { name: "app1-claude", dependencies: { "@fx/gamma": "=8.0.0" } });
  write(path.join(workspace, "app1/target/package.json"), { name: "app1-target", dependencies: { "@fx/gamma": "=7.0.0" } });
  write(path.join(workspace, "app1/.cache/package.json"), { name: "app1-cache", dependencies: { "@fx/gamma": "=6.0.0" } });
  const adoption = generate({ root, workspace, apps })["docs/ADOPTION.md"];
  for (const hidden of ["9.0.0", "8.0.0", "7.0.0", "6.0.0", "9.9.9"]) assert.ok(!adoption.includes(hidden), `skipped: ${hidden}`);
  assert.ok(adoption.includes("| app1 | yes | 5 | 0 |"), "app1 reads only its visible manifests");
});

test("semver subset matches cargo and npm requirement meaning", () => {
  assert.equal(satisfies("0.2.5", "^0.2.0"), true);
  assert.equal(satisfies("0.3.0", "^0.2.0"), false);
  assert.equal(satisfies("0.0.3", "^0.0.3"), true);
  assert.equal(satisfies("0.0.4", "^0.0.3"), false);
  assert.equal(satisfies("1.2.9", "~1.2.3"), true);
  assert.equal(satisfies("1.3.0", "~1.2.3"), false);
  assert.equal(satisfies("0.2.5", "0.2.0", "cargo"), true, "cargo bare version is a caret requirement");
  assert.equal(satisfies("0.2.5", "0.2.0", "npm"), false, "npm bare version is exact");
  assert.equal(satisfies("0.2.5", "0.2"), true);
  assert.equal(satisfies("0.3.0", "0.2.*"), false);
  assert.equal(satisfies("1.0.0", "*"), true);
  assert.equal(satisfies("0.2.0", "=0.1.0"), false);
  assert.equal(satisfies("1.5.0", ">=1.0.0 <2"), true);
  assert.equal(satisfies("2.0.0", ">=1.0.0 <2"), false);
  assert.equal(isExactPin("=0.1.0", "cargo"), true);
  assert.equal(isExactPin("0.1.0", "cargo"), false);
  assert.equal(isExactPin("0.1.0", "npm"), true);
});

test("pin status separates current, stale, ahead, local and unparsed", () => {
  const pin = (req, ecosystem = "cargo", source = "registry") => ({ ecosystem, req, source });
  assert.equal(pinStatus(pin("=0.2.0"), "0.2.0"), "current");
  assert.equal(pinStatus(pin("=0.1.0"), "0.2.0"), "stale");
  assert.equal(pinStatus(pin("=0.3.0"), "0.2.0"), "ahead");
  assert.equal(pinStatus(pin("0.2"), "0.2.7"), "range ok");
  assert.equal(pinStatus(pin("0.1"), "0.2.7"), "stale");
  assert.equal(pinStatus(pin("0.2.0", "cargo", "local"), "0.2.0"), "local");
  assert.equal(pinStatus(pin("catalog:"), "npm"), "unparsed");
  assert.equal(pinStatus(pin("^1.0.0", "npm"), null), "unpublished");
});

test("TOML and npm readers keep sections, dotted keys, inherited entries and aliases", () => {
  const toml = [
    "[package]",
    'name = "demo"',
    'version = "0.1.0"',
    "",
    "[dependencies]",
    'alpha = { version = "0.1", features = ["a", "b"] }',
    "beta.workspace = true",
    'gamma = { path = "../gamma" }',
    "",
    "[dev-dependencies.delta]",
    'version = "2"',
    "",
    "[target.'cfg(unix)'.dependencies]",
    'nix = "=0.30.1"',
    "",
    "[workspace.dependencies]",
    'epsilon = "1"',
  ].join("\n");
  const entries = cargoDependencies(toml).map(({ section, name, req, source }) => [section, name, req, source]);
  assert.deepEqual(entries, [
    ["dependencies", "alpha", "0.1", "registry"],
    ["dependencies", "gamma", "", "local"],
    ["dev-dependencies", "delta", "2", "registry"],
    ["dependencies", "nix", "=0.30.1", "registry"],
    ["workspace.dependencies", "epsilon", "1", "registry"],
  ]);
  assert.equal(parseToml(toml).package.version, "0.1.0");

  const npm = npmDependencies({ dependencies: { "@fx/x": "npm:@fx/x@^2.0.0", local: "workspace:*", repo: "github:a/b" } });
  assert.deepEqual(npm.map(({ name, req, source }) => [name, req, source]), [
    ["@fx/x", "^2.0.0", "registry"],
    ["local", "workspace:*", "local"],
    ["repo", "github:a/b", "git"],
  ]);
});

test("the committed repo-local RightKit docs match the generator (drift test)", () => {
  // Repo-local only: INDEX.md and CAPABILITY-MAP.md. ADOPTION.md reads app repos and has its own --check-adoption.
  const result = spawnSync(process.execPath, [script, "--check", "--root", DEFAULT_ROOT], { encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stderr}${result.stdout}`);
});
