import { describe, expect, it } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { binary, fail, fakeRun, git, gitRepo, mkRepo, manifest, mockFetch, ok, validToolSource } from "./support.js";
import { runApply, type ApplyDeps } from "../src/apply.js";
import { loadApplyReport } from "../src/reports.js";

const sha = (text: string | Buffer) => crypto.createHash("sha256").update(text).digest("hex");
const noOutdated = async () => ({ stdout: "{}" });

const CARGO_TOML = `[package]
name = "app"
version = "0.1.0"

[dependencies]
serde = "1.0.100"  # keep me
anyhow = { version = "1", default-features = false, features = ["std"] }
tokio = { version = "1.2.3", features = ["rt"] }
rand = "0.8.5"
local = { path = "../local", version = "0.1.0" }
`;

const lockWith = (versions: Record<string, string>) =>
  Object.entries(versions)
    .map(([name, version]) => `[[package]]\nname = "${name}"\nversion = "${version}"\n`)
    .join("\n");

const cratesRoutes = (latest: Record<string, string>) =>
  Object.fromEntries(Object.entries(latest).map(([name, v]) => [`https://crates.io/api/v1/crates/${name}`, { crate: { max_stable_version: v } }]));

/** Simulates `rightkit cargo update -p <name> --precise <ver>` by rewriting Cargo.lock. */
function cargoUpdateFake(repo: string) {
  return (argv: string[], options: { cwd: string }) => {
    if (argv[0] !== "rightkit" || argv[1] !== "cargo") return undefined;
    const name = argv[argv.indexOf("-p") + 1].split("@")[0];
    const version = argv[argv.indexOf("--precise") + 1];
    const lock = path.join(repo, "Cargo.lock");
    const text = fs.readFileSync(lock, "utf8");
    fs.writeFileSync(lock, text.replace(new RegExp(`(name = "${name}"\\nversion = ")[^"]+`), `$1${version}`));
    expect(options.cwd).toBe(repo);
    return ok();
  };
}

async function applyIn(repo: string, deps: ApplyDeps, options: { only?: string[]; dryRun?: boolean } = {}) {
  return runApply({ repo, ...options }, { exec: noOutdated, ...deps });
}

describe("apply: cargo", () => {
  it("rewrites the version in place, refreshes the lock through `rightkit cargo`, and reports before/after", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": CARGO_TOML,
      "Cargo.lock": lockWith({ serde: "1.0.100", anyhow: "1.0.0", tokio: "1.2.3", rand: "0.8.5" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ serde: "1.0.229", anyhow: "1.0.99", tokio: "1.2.3", rand: "0.8.5" }));
    const { run, calls } = fakeRun(cargoUpdateFake(repo));
    const outcome = await applyIn(repo, { fetch, run });

    expect(outcome.errors).toEqual([]);
    expect(outcome.exitCode).toBe(0);
    const report = outcome.report!;
    expect(report.changes).toHaveLength(1);
    expect(report.changes[0]).toMatchObject({ kind: "cargo", name: "serde", source: "Cargo.toml", before: "1.0.100", after: "1.0.229", status: "applied" });
    expect(report.changes[0].files.map((f) => f.path).sort()).toEqual(["Cargo.lock", "Cargo.toml"]);

    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_TOML.replace('serde = "1.0.100"', 'serde = "1.0.229"'));
    expect(fs.readFileSync(path.join(repo, "Cargo.lock"), "utf8")).toContain('name = "serde"\nversion = "1.0.229"');
    expect(calls.filter((c) => c[0] === "rightkit")).toEqual([["rightkit", "cargo", "update", "-p", "serde", "--precise", "1.0.229"]]);
    // Never a bare cargo.
    expect(calls.some((c) => c[0] === "cargo")).toBe(false);

    // The report on disk is the one land will read, with file digests.
    const onDisk = loadApplyReport(path.join(repo, ".rightkit/update/apply-report.json"));
    expect(onDisk.ok).toBe(true);
    const file = report.changes[0].files.find((f) => f.path === "Cargo.toml")!;
    expect(file.sha256After).toBe(sha(fs.readFileSync(path.join(repo, "Cargo.toml"))));
    expect(file.sha256Before).toBe(sha(CARGO_TOML));
    // State is self-ignoring.
    expect(fs.readFileSync(path.join(repo, ".rightkit/update/.gitignore"), "utf8")).toBe("*\n");
  });

  it("restores the manifest and lock when the lock refresh fails", async () => {
    const lock = lockWith({ serde: "1.0.100" });
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": CARGO_TOML,
      "Cargo.lock": lock,
    });
    const { fetch } = mockFetch(cratesRoutes({ serde: "1.0.229", anyhow: "1.0.0", tokio: "1.2.3", rand: "0.8.5" }));
    const { run } = fakeRun((argv) => (argv[0] === "rightkit" ? fail("error: no matching package") : undefined));
    const outcome = await applyIn(repo, { fetch, run });

    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.changes).toEqual([]);
    expect(outcome.report!.failed[0]).toMatchObject({ name: "serde" });
    expect(outcome.report!.failed[0].reason).toMatch(/rightkit cargo update failed/);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_TOML);
    expect(fs.readFileSync(path.join(repo, "Cargo.lock"), "utf8")).toBe(lock);
  });

  it("rolls back when the lock does not end up at the new version", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": CARGO_TOML,
      "Cargo.lock": lockWith({ serde: "1.0.100" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ serde: "1.0.229", anyhow: "1.0.0", tokio: "1.2.3", rand: "0.8.5" }));
    const { run } = fakeRun((argv) => (argv[0] === "rightkit" ? ok() : undefined));
    const outcome = await applyIn(repo, { fetch, run });
    expect(outcome.report!.failed[0].reason).toMatch(/does not contain serde 1.0.229/);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_TOML);
  });

  it("does not apply breaking versions: major bumps and minor bumps at 0.x become needs-migration", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": CARGO_TOML,
      "Cargo.lock": lockWith({ serde: "1.0.100" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ serde: "2.0.0", anyhow: "1.0.0", tokio: "1.2.3", rand: "0.9.1" }));
    const { run, calls } = fakeRun(() => undefined);
    const outcome = await applyIn(repo, { fetch, run });

    expect(outcome.exitCode).toBe(0);
    expect(outcome.report!.changes).toEqual([]);
    expect(outcome.report!.needsMigration.map((r) => `${r.name}:${r.before}->${r.latest}`).sort()).toEqual(["rand:0.8.5->0.9.1", "serde:1.0.100->2.0.0"]);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_TOML);
    expect(calls.some((c) => c[0] === "rightkit")).toBe(false);
    // held.json lists them for an agent lane.
    const held = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update/held.json"), "utf8"));
    expect(held.needsMigration.map((r: { name: string }) => r.name).sort()).toEqual(["rand", "serde"]);
  });

  it("leaves held rows alone and sends agent-pr rows to needs-migration", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest(
        [{ kind: "cargo", manifest: "Cargo.toml" }, { kind: "cargo", manifest: "b/Cargo.toml", policy: "agent-pr" }],
        { default: "auto-land", held: ["serde"] },
      ),
      "Cargo.toml": CARGO_TOML,
      "Cargo.lock": lockWith({ serde: "1.0.100" }),
      "b/Cargo.toml": '[dependencies]\nitoa = "1.0.1"\n',
    });
    const { fetch } = mockFetch(cratesRoutes({ serde: "1.0.229", anyhow: "1.0.0", tokio: "1.2.3", rand: "0.8.5", itoa: "1.0.9" }));
    const { run, calls } = fakeRun(() => undefined);
    const outcome = await applyIn(repo, { fetch, run });
    expect(outcome.report!.changes).toEqual([]);
    expect(outcome.report!.held.map((r) => r.name)).toEqual(["serde"]);
    expect(outcome.report!.needsMigration.map((r) => r.name)).toEqual(["itoa"]);
    expect(calls.some((c) => c[0] === "rightkit")).toBe(false);
  });

  it("--only restricts the run to named dependencies, and unknown names are an error", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": '[dependencies]\na = "1.0.0"\nb = "1.0.0"\n',
      "Cargo.lock": lockWith({ a: "1.0.0", b: "1.0.0" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ a: "1.0.5", b: "1.0.5" }));
    const { run } = fakeRun(cargoUpdateFake(repo));
    const missing = await applyIn(repo, { fetch, run }, { only: ["nope"] });
    expect(missing.exitCode).toBe(2);
    expect(missing.errors[0]).toMatch(/--only names not found/);

    const outcome = await applyIn(repo, { fetch, run }, { only: ["b"] });
    expect(outcome.report!.changes.map((c) => c.name)).toEqual(["b"]);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe('[dependencies]\na = "1.0.0"\nb = "1.0.5"\n');
  });

  it("changes nothing when a lookup failed (fail closed), and --only scopes the failure away", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": '[dependencies]\na = "1.0.0"\nb = "1.0.0"\n',
      "Cargo.lock": lockWith({ a: "1.0.0", b: "1.0.0" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ a: "1.0.5" })); // b -> 404
    const { run } = fakeRun(cargoUpdateFake(repo));
    const blocked = await applyIn(repo, { fetch, run });
    expect(blocked.exitCode).toBe(2);
    expect(blocked.errors.join("\n")).toMatch(/cargo:b/);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe('[dependencies]\na = "1.0.0"\nb = "1.0.0"\n');
    const scoped = await applyIn(repo, { fetch, run }, { only: ["a"] });
    expect(scoped.exitCode).toBe(0);
    expect(scoped.report!.changes.map((c) => c.name)).toEqual(["a"]);
  });

  it("skips the lock command when an earlier row already locked that exact version", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }, { kind: "cargo", manifest: "m/Cargo.toml" }]),
      "Cargo.toml": '[dependencies]\na = "1.0.0"\n',
      "m/Cargo.toml": '[dependencies]\na = "1.0.0"\n',
      "Cargo.lock": lockWith({ a: "1.0.0" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ a: "1.0.5" }));
    const { run, calls } = fakeRun(cargoUpdateFake(repo));
    const outcome = await applyIn(repo, { fetch, run });
    expect(outcome.report!.changes).toHaveLength(2);
    expect(calls.filter((c) => c[0] === "rightkit")).toHaveLength(1);
  });

  it("dry-run changes nothing, runs nothing and writes no report", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": CARGO_TOML,
      "Cargo.lock": lockWith({ serde: "1.0.100" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ serde: "1.0.229", anyhow: "1.0.0", tokio: "1.2.3", rand: "0.8.5" }));
    const { run, calls } = fakeRun(() => undefined);
    const outcome = await applyIn(repo, { fetch, run }, { dryRun: true });
    expect(outcome.report!.dryRun).toBe(true);
    expect(outcome.report!.changes[0]).toMatchObject({ name: "serde", status: "planned", after: "1.0.229" });
    expect(outcome.report!.changes[0].commands[0].slice(0, 3)).toEqual(["rightkit", "cargo", "update"]);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_TOML);
    expect(calls.some((c) => c[0] === "rightkit")).toBe(false);
    expect(fs.existsSync(path.join(repo, ".rightkit/update"))).toBe(false);
  });

  it("refuses to edit a file that already has uncommitted changes", async () => {
    const repo = gitRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": '[dependencies]\na = "1.0.0"\n',
      "Cargo.lock": lockWith({ a: "1.0.0" }),
    });
    fs.appendFileSync(path.join(repo, "Cargo.toml"), "# my own edit\n");
    const { fetch } = mockFetch(cratesRoutes({ a: "1.0.5" }));
    const { run, calls } = fakeRun(cargoUpdateFake(repo));
    const outcome = await applyIn(repo, { fetch, run });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.failed[0].reason).toMatch(/uncommitted changes: Cargo.toml/);
    expect(calls.some((c) => c[0] === "rightkit")).toBe(false);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toMatch(/my own edit/);
  });

  it("refuses a second apply while the first is not landed", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": '[dependencies]\na = "1.0.0"\nb = "1.0.0"\n',
      "Cargo.lock": lockWith({ a: "1.0.0", b: "1.0.0" }),
    });
    const { fetch } = mockFetch(cratesRoutes({ a: "1.0.5", b: "1.0.5" }));
    const { run } = fakeRun(cargoUpdateFake(repo));
    expect((await applyIn(repo, { fetch, run }, { only: ["a"] })).exitCode).toBe(0);
    const again = await applyIn(repo, { fetch, run }, { only: ["b"] });
    expect(again.exitCode).toBe(2);
    expect(again.errors[0]).toMatch(/not landed/);
  });
});

describe("apply: pnpm", () => {
  const pkg = {
    name: "app",
    dependencies: { "left-pad": "1.2.0", "is-odd": "~3.0.0", ms: "^2.0.0" },
    devDependencies: { debug: "^4.1.0" },
    optionalDependencies: { fsevents: "^2.3.0" },
  };

  /** Simulates `pnpm add` by rewriting the right section of package.json. */
  function pnpmFake(dir: string, seen: string[][]) {
    return (argv: string[]) => {
      if (argv[0] !== "pnpm") return undefined;
      seen.push(argv);
      const spec = argv[argv.length - 1];
      const at = spec.lastIndexOf("@");
      const name = spec.slice(0, at);
      const version = spec.slice(at + 1);
      const file = path.join(dir, "package.json");
      const doc = JSON.parse(fs.readFileSync(file, "utf8"));
      const section = argv.includes("-D") ? "devDependencies" : argv.includes("-O") ? "optionalDependencies" : "dependencies";
      const prefix = argv.includes("--save-exact") ? "" : (argv.find((a) => a.startsWith("--save-prefix="))?.split("=")[1] ?? "^");
      doc[section][name] = prefix + version;
      fs.writeFileSync(file, JSON.stringify(doc, null, 2));
      fs.writeFileSync(path.join(dir, "pnpm-lock.yaml"), `lock ${name} ${version}\n`);
      return ok();
    };
  }

  const npmRoutes = (latest: Record<string, string>) =>
    Object.fromEntries(Object.entries(latest).map(([name, version]) => [`https://registry.npmjs.org/${encodeURIComponent(name)}/latest`, { version }]));

  it("uses pnpm add with the right section flag, keeps the range style, and never npm or npx", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "pnpm", manifest: "packages/app/package.json" }]),
      "packages/app/package.json": pkg,
      "pnpm-lock.yaml": "lock\n",
    });
    const seen: string[][] = [];
    const { fetch } = mockFetch(npmRoutes({ "left-pad": "1.3.0", "is-odd": "3.0.1", ms: "2.1.3", debug: "4.3.7", fsevents: "2.3.3" }));
    const { run } = fakeRun(pnpmFake(path.join(repo, "packages/app"), seen));
    const outcome = await applyIn(repo, { fetch, run });

    expect(outcome.errors).toEqual([]);
    expect(outcome.report!.failed).toEqual([]);
    const byName = Object.fromEntries(seen.map((a) => [a[a.length - 1].replace(/@[^@]+$/, ""), a]));
    expect(byName["left-pad"]).toEqual(["pnpm", "add", "--save-exact", "--ignore-scripts", "left-pad@1.3.0"]);
    expect(byName["is-odd"]).toEqual(["pnpm", "add", "--save-prefix=~", "--ignore-scripts", "is-odd@3.0.1"]);
    expect(byName.ms).toEqual(["pnpm", "add", "--save-prefix=^", "--ignore-scripts", "ms@2.1.3"]);
    expect(byName.debug).toEqual(["pnpm", "add", "-D", "--save-prefix=^", "--ignore-scripts", "debug@4.3.7"]);
    expect(byName.fsevents).toEqual(["pnpm", "add", "-O", "--save-prefix=^", "--ignore-scripts", "fsevents@2.3.3"]);
    expect(seen.every((a) => a[0] === "pnpm")).toBe(true);

    const final = JSON.parse(fs.readFileSync(path.join(repo, "packages/app/package.json"), "utf8"));
    expect(final.dependencies).toEqual({ "left-pad": "1.3.0", "is-odd": "~3.0.1", ms: "^2.1.3" });
    expect(final.devDependencies).toEqual({ debug: "^4.3.7" });
    expect(final.optionalDependencies).toEqual({ fsevents: "^2.3.3" });
    const change = outcome.report!.changes.find((c) => c.name === "debug")!;
    expect(change.files.map((f) => f.path).sort()).toEqual(["packages/app/package.json", "packages/app/pnpm-lock.yaml"]);
  });

  it("adds -w when the manifest sits in a pnpm workspace root", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "pnpm", manifest: "package.json" }]),
      "package.json": { name: "root", devDependencies: { debug: "^4.1.0" } },
      "pnpm-workspace.yaml": "packages:\n  - packages/*\n",
    });
    const seen: string[][] = [];
    const { fetch } = mockFetch(npmRoutes({ debug: "4.3.7" }));
    const { run } = fakeRun(pnpmFake(repo, seen));
    const outcome = await applyIn(repo, { fetch, run });
    expect(seen[0]).toEqual(["pnpm", "add", "-w", "-D", "--save-prefix=^", "--ignore-scripts", "debug@4.3.7"]);
    expect(outcome.report!.changes[0].files.map((f) => f.path)).toContain("package.json");
  });

  it("restores package.json and the lock when pnpm fails or writes something unexpected", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "pnpm", manifest: "package.json" }]),
      "package.json": { name: "app", dependencies: { ms: "^2.0.0", "is-odd": "~3.0.0" } },
      "pnpm-lock.yaml": "original\n",
    });
    const original = fs.readFileSync(path.join(repo, "package.json"), "utf8");
    const { fetch } = mockFetch(npmRoutes({ ms: "2.1.3", "is-odd": "3.0.1" }));
    const { run } = fakeRun((argv) => {
      if (argv[0] !== "pnpm") return undefined;
      fs.writeFileSync(path.join(repo, "pnpm-lock.yaml"), "half-written\n");
      // pnpm "succeeds" for ms but records a different spec than requested; fails for is-odd.
      if (argv[argv.length - 1].startsWith("ms@")) {
        fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ name: "app", dependencies: { ms: "^9.9.9", "is-odd": "~3.0.0" } }));
        return ok();
      }
      return fail("ERR_PNPM_NO_MATCHING_VERSION");
    });
    const outcome = await applyIn(repo, { fetch, run });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.failed.map((r) => r.name).sort()).toEqual(["is-odd", "ms"]);
    expect(fs.readFileSync(path.join(repo, "package.json"), "utf8")).toBe(original);
    expect(fs.readFileSync(path.join(repo, "pnpm-lock.yaml"), "utf8")).toBe("original\n");
  });

  it("does not apply a major bump", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "pnpm", manifest: "package.json" }]),
      "package.json": { name: "app", devDependencies: { typescript: "^5.7.0" } },
    });
    const { fetch } = mockFetch(npmRoutes({ typescript: "7.0.2" }));
    const { run, calls } = fakeRun(() => undefined);
    const outcome = await applyIn(repo, { fetch, run });
    expect(outcome.report!.needsMigration.map((r) => r.name)).toEqual(["typescript"]);
    expect(calls.some((c) => c[0] === "pnpm")).toBe(false);
  });

  it("skips range styles pnpm add cannot express", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "pnpm", manifest: "package.json" }]),
      "package.json": { name: "app", dependencies: { ms: ">=2.0.0" } },
    });
    const { fetch } = mockFetch(npmRoutes({ ms: "2.1.3" }));
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run });
    expect(outcome.report!.skipped[0].reason).toMatch(/cannot express/);
  });
});

describe("apply: swiftpm", () => {
  const PACKAGE_SWIFT = `// swift-tools-version: 5.9
import PackageDescription
let package = Package(
  name: "App",
  dependencies: [
    .package(url: "https://github.com/acme/Alpha.git", exact: "1.2.3"),
    .package(url: "https://github.com/acme/Beta", from: "2.0.0"),
    .package(url: "https://github.com/acme/Gamma", branch: "main"),
  ]
)
`;
  const PROJECT_YML = `name: App
packages:
  Alpha:
    url: https://github.com/acme/Alpha.git
    exactVersion: 1.2.3
  Beta:
    url: https://github.com/acme/Beta
    from: 2.0.0
`;
  const tagsRoutes = {
    "https://api.github.com/repos/acme/Alpha/tags?per_page=100": [{ name: "1.4.0" }, { name: "1.2.3" }],
    "https://api.github.com/repos/acme/Beta/tags?per_page=100": [{ name: "v2.0.0" }, { name: "v2.0.0" }],
  };

  it("rewrites exact: in Package.swift and the matching key in project.yml, and notes Package.resolved", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "swiftpm", manifest: "Package.swift" }]),
      "Package.swift": PACKAGE_SWIFT,
      "project.yml": PROJECT_YML,
      "Package.resolved": "{}",
    });
    const { fetch } = mockFetch(tagsRoutes);
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run });
    expect(outcome.report!.failed).toEqual([]);
    expect(outcome.report!.changes).toHaveLength(1);
    expect(outcome.report!.changes[0]).toMatchObject({ kind: "swiftpm", name: "Alpha", before: "1.2.3", after: "1.4.0" });
    expect(outcome.report!.changes[0].notes.join(" ")).toMatch(/Package.resolved was not refreshed/);
    expect(fs.readFileSync(path.join(repo, "Package.swift"), "utf8")).toBe(PACKAGE_SWIFT.replace('exact: "1.2.3"', 'exact: "1.4.0"'));
    expect(fs.readFileSync(path.join(repo, "project.yml"), "utf8")).toBe(PROJECT_YML.replace("exactVersion: 1.2.3", "exactVersion: 1.4.0"));
  });
});

describe("apply: tool", () => {
  const release = (tag: string) => ({
    "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": { tag_name: tag, assets: [{ name: "yt-dlp_macos" }, { name: "yt-dlp.exe" }] },
  });
  const MAC_BYTES = "mac-binary-bytes";
  const WIN_BYTES = "win-binary-bytes";

  function toolRepo(extra: Record<string, unknown> = {}) {
    return mkRepo({ ".rightkit/update.json": manifest([validToolSource(extra)]) });
  }

  it("stages the host asset with an observed digest and hashes other platforms without storing them", async () => {
    const repo = toolRepo();
    const urls: string[] = [];
    const { fetch } = mockFetch({
      ...release("2025.10.01"),
      "https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.01/yt-dlp_macos": (u: string) => (urls.push(u), binary(MAC_BYTES)),
      "https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.01/yt-dlp.exe": (u: string) => (urls.push(u), binary(WIN_BYTES)),
    });
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run, toolPlatform: "macos-arm64" });

    expect(outcome.errors).toEqual([]);
    const change = outcome.report!.changes[0];
    expect(change).toMatchObject({ kind: "tool", name: "yt-dlp", before: "2025.01.12", after: "2025.10.01", status: "staged", files: [] });
    const tool = change.tool!;
    expect(tool).toMatchObject({ platform: "macos-arm64", asset: "yt-dlp_macos", trust: "observed", size: MAC_BYTES.length, sha256: sha(MAC_BYTES), candidateDir: ".rightkit/update/candidates/yt-dlp/2025.10.01", binary: "yt-dlp_macos" });
    expect(tool.otherPlatforms).toEqual([
      expect.objectContaining({ platform: "windows-x86_64", asset: "yt-dlp.exe", sha256: sha(WIN_BYTES), trust: "observed" }),
    ]);
    const staged = path.join(repo, tool.candidateDir, "yt-dlp_macos");
    expect(fs.readFileSync(staged, "utf8")).toBe(MAC_BYTES);
    // Windows has no execute bit to set or read.
    if (process.platform !== "win32") expect(fs.statSync(staged).mode & 0o111).not.toBe(0);
    // The foreign-platform binary is not stored.
    expect(fs.readdirSync(path.join(repo, tool.candidateDir))).toEqual(["yt-dlp_macos"]);
    // The manifest is untouched until land.
    expect(JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update.json"), "utf8")).sources[0].pin).toBe("2025.01.12");
    expect(change.notes.join(" ")).toMatch(/observed/);
  });

  it("enforces the size bound and leaves nothing staged", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([
        validToolSource({
          assets: { "macos-arm64": { asset: "yt-dlp_macos", checksum: `sha256:${"a".repeat(64)}`, maxBytes: 8 } },
        }),
      ]),
    });
    const { fetch } = mockFetch({
      "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": { tag_name: "2025.10.01", assets: [{ name: "yt-dlp_macos" }] },
      "https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.01/yt-dlp_macos": () => binary("this is far more than eight bytes"),
    });
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run, toolPlatform: "macos-arm64" });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.failed[0].reason).toMatch(/exceeded the 8 byte bound/);
    expect(fs.existsSync(path.join(repo, ".rightkit/update/candidates/yt-dlp"))).toBe(false);
  });

  it("rejects a declared content-length over the bound before reading the body", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([
        validToolSource({ assets: { "macos-arm64": { asset: "yt-dlp_macos", checksum: `sha256:${"a".repeat(64)}`, maxBytes: 8 } } }),
      ]),
    });
    const { fetch } = mockFetch({
      "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": { tag_name: "2025.10.01", assets: [{ name: "yt-dlp_macos" }] },
      "https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.01/yt-dlp_macos": () => binary("tiny", { "content-length": "999999" }),
    });
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run, toolPlatform: "macos-arm64" });
    expect(outcome.report!.failed[0].reason).toMatch(/declares 999999 bytes/);
  });

  it("dry-run downloads nothing", async () => {
    const repo = toolRepo();
    const { fetch, calls } = mockFetch(release("2025.10.01"));
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run, toolPlatform: "macos-arm64" }, { dryRun: true });
    expect(outcome.report!.changes[0]).toMatchObject({ status: "planned", after: "2025.10.01" });
    expect(calls.filter((u) => u.includes("/download/"))).toEqual([]);
    expect(fs.existsSync(path.join(repo, ".rightkit/update/candidates"))).toBe(false);
  });

  it("skips a tool with no asset for the host platform", async () => {
    const repo = toolRepo();
    const { fetch } = mockFetch(release("2025.10.01"));
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run, toolPlatform: "linux-x86_64" });
    expect(outcome.report!.changes).toEqual([]);
    expect(outcome.report!.skipped[0].reason).toMatch(/no asset for host platform linux-x86_64/);
  });

  it("extracts the named member from an archive with the system tar", async () => {
    const work = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), "right-update-tar-"));
    fs.mkdirSync(path.join(work, "pkg/bin"), { recursive: true });
    fs.writeFileSync(path.join(work, "pkg/bin/helper"), "helper-binary");
    fs.writeFileSync(path.join(work, "pkg/bin/other"), "other");
    git(work, "init", "-q"); // any command that proves git exists; archive made with tar below
    const archive = path.join(work, "helper.tar.gz");
    const { execFileSync } = await import("node:child_process");
    // Relative names: Git for Windows puts a GNU tar on PATH that reads "C:" in a path as a remote host.
    execFileSync("tar", ["-czf", "helper.tar.gz", "-C", "pkg", "bin"], { cwd: work });
    const archiveBytes = fs.readFileSync(archive);

    const repo = mkRepo({
      ".rightkit/update.json": manifest([
        {
          kind: "tool",
          name: "helper",
          pin: "1.0.0",
          assets: { "macos-arm64": { asset: "helper.tar.gz", checksum: `sha256:${"b".repeat(64)}`, extract: { member: "bin/helper" } } },
          upstream: { type: "url", latest: "https://example.com/helper/latest.txt", download: "https://example.com/helper/{version}/{asset}" },
          journey: { command: ["node", "x.mjs"], report: "{out}/report.json" },
        },
      ]),
    });
    const { fetch } = mockFetch({
      "https://example.com/helper/latest.txt": "1.1.0\n",
      "https://example.com/helper/1.1.0/helper.tar.gz": () => binary(archiveBytes),
    });
    const outcome = await applyIn(repo, { fetch, toolPlatform: "macos-arm64" });
    expect(outcome.report!.failed).toEqual([]);
    const tool = outcome.report!.changes[0].tool!;
    expect(tool.binary).toBe("bin/helper");
    expect(tool.sha256).toBe(sha(archiveBytes));
    expect(tool.binarySha256).toBe(sha("helper-binary"));
    expect(fs.existsSync(path.join(repo, tool.candidateDir, "bin/other"))).toBe(false);
  });
});

describe("apply: manifest problems", () => {
  it("exits 2 for an invalid manifest", async () => {
    const repo = mkRepo({ ".rightkit/update.json": { schemaVersion: 1 } });
    const outcome = await applyIn(repo, {});
    expect(outcome.exitCode).toBe(2);
  });

  it("is a git no-op for repos that are not at a git root", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([{ kind: "cargo", manifest: "Cargo.toml" }]),
      "Cargo.toml": '[dependencies]\na = "1.0.0"\n',
    });
    const { fetch } = mockFetch(cratesRoutes({ a: "1.0.0" }));
    const outcome = await applyIn(repo, { fetch, run: fakeRun(() => undefined).run });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.report!.baseCommit).toBeNull();
  });
});
