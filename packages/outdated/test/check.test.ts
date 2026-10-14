import { describe, expect, it } from "vitest";
import { mkRepo, manifest, mockFetch, validToolSource, SHA } from "./support.js";
import { checkSources, exitCodeFor, cargoDependencies, parseSwiftPackage, parseSwiftProjectYml } from "../src/check.js";
import { runCheck } from "../src/run.js";
import { fetchText, MAX_RESPONSE_BYTES } from "../src/http.js";
import { loadManifest } from "../src/manifest.js";
import fs from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";

const noExec = async () => ({ stdout: "{}" });

describe("cargo source", () => {
  const cargoToml = `[package]
name = "app"

[dependencies]
serde = "1.0.100"
anyhow = "1"
local-crate = { path = "../local" }
renamed = { package = "real-name", version = "0.3" }

[dev-dependencies]
tempfile = { version = "3.10" }
`;

  it("extracts registry, renamed, and non-registry dependencies", () => {
    const doc = parseToml(cargoToml) as Record<string, unknown>;
    const deps = cargoDependencies(doc);
    expect(deps.map((d) => d.name)).toEqual(["serde", "anyhow", "local-crate", "real-name", "tempfile"]);
    expect(deps.find((d) => d.name === "local-crate")?.reason).toBe("path or git dependency");
  });

  it("reports outdated, current, and unknown entries against crates.io", async () => {
    const repo = mkRepo({ "Cargo.toml": cargoToml });
    const { fetch, calls } = mockFetch({
      "https://crates.io/api/v1/crates/serde": { crate: { max_stable_version: "1.0.210" } },
      "https://crates.io/api/v1/crates/anyhow": { crate: { max_stable_version: "1.0.99" } },
      "https://crates.io/api/v1/crates/real-name": { crate: { max_stable_version: "0.3.4" } },
      "https://crates.io/api/v1/crates/tempfile": { crate: { max_stable_version: "3.20.0" } },
    });
    const m = manifest([{ kind: "cargo", manifest: "Cargo.toml" }]) as never;
    const entries = await checkSources(repo, m, { fetch, exec: noExec });
    const byName = Object.fromEntries(entries.map((e) => [e.name, e]));
    expect(byName.serde).toMatchObject({ status: "outdated", pinned: "1.0.100", latest: "1.0.210", kind: "cargo", policy: "auto-land" });
    expect(byName.anyhow).toMatchObject({ status: "current", pinned: "1" });
    expect(byName["real-name"]).toMatchObject({ status: "current", pinned: "0.3" });
    expect(byName["local-crate"]).toMatchObject({ status: "unknown", reason: "path or git dependency" });
    expect(byName.tempfile).toMatchObject({ status: "outdated", latest: "3.20.0" });
    expect(calls.every((u) => u.startsWith("https://crates.io/api/v1/crates/"))).toBe(true);
    expect(exitCodeFor(entries)).toBe(1);
  });

  it("records a lookup failure as an error entry", async () => {
    const repo = mkRepo({ "Cargo.toml": '[dependencies]\nserde = "1.0.0"\n' });
    const entries = await checkSources(repo, manifest([{ kind: "cargo", manifest: "Cargo.toml" }]) as never, { fetch: mockFetch({}).fetch, exec: noExec });
    expect(entries[0].error).toMatch(/crates\.io lookup failed: GET .* HTTP 404/);
    expect(exitCodeFor(entries)).toBe(2);
  });
});

describe("pnpm source", () => {
  it("uses pnpm outdated for latest where it reports one and the registry otherwise", async () => {
    const repo = mkRepo({
      "app/package.json": { name: "app", dependencies: { "left-pad": "^1.0.0", "is-odd": "3.0.1", local: "workspace:*" } },
    });
    const execCalls: Array<{ file: string; args: string[]; cwd: string }> = [];
    const exec = async (file: string, args: string[], options: { cwd: string }) => {
      execCalls.push({ file, args, cwd: options.cwd });
      return { stdout: JSON.stringify({ "left-pad": { current: "1.0.0", latest: "1.3.0" } }), error: "exit 1" };
    };
    const { fetch } = mockFetch({
      "https://registry.npmjs.org/is-odd/latest": { version: "3.0.1" },
    });
    const entries = await checkSources(repo, manifest([{ kind: "pnpm", manifest: "app/package.json" }]) as never, { fetch, exec });
    const byName = Object.fromEntries(entries.map((e) => [e.name, e]));
    expect(execCalls[0]).toMatchObject({ file: "pnpm", args: ["outdated", "--json"] });
    expect(byName["left-pad"]).toMatchObject({ status: "outdated", latest: "1.3.0", pinned: "^1.0.0" });
    expect(byName["is-odd"]).toMatchObject({ status: "current", latest: "3.0.1" });
    expect(byName.local).toMatchObject({ status: "unknown", reason: "non-registry version requirement" });
  });

  it("falls back to the npm registry when pnpm outdated is not JSON", async () => {
    const repo = mkRepo({ "package.json": { dependencies: { "is-odd": "^2.0.0" } } });
    const { fetch, calls } = mockFetch({ "https://registry.npmjs.org/is-odd/latest": { version: "3.0.1" } });
    const entries = await checkSources(repo, manifest([{ kind: "pnpm", manifest: "package.json" }]) as never, {
      fetch,
      exec: async () => ({ stdout: "not json" }),
    });
    expect(entries[0]).toMatchObject({ status: "outdated", latest: "3.0.1" });
    expect(calls).toContain("https://registry.npmjs.org/is-odd/latest");
  });
});

describe("swiftpm source", () => {
  const packageSwift = `// swift-tools-version:5.9
let package = Package(
  name: "App",
  dependencies: [
    .package(url: "https://github.com/apple/swift-argument-parser.git", exact: "1.2.0"),
    .package(url: "https://github.com/vapor/vapor", from: "4.80.0"),
    .package(url: "https://github.com/acme/tool", branch: "main"),
    .package(url: "https://gitlab.example.com/acme/other", from: "1.0.0"),
  ]
)`;

  it("parses Package.swift requirements", () => {
    const deps = parseSwiftPackage(packageSwift);
    expect(deps.map((d) => [d.name, d.requirement, d.reason ?? null])).toEqual([
      ["swift-argument-parser", "1.2.0", null],
      ["vapor", "4.80.0", null],
      ["tool", null, "branch or revision pin"],
      ["other", "1.0.0", null],
    ]);
  });

  it("parses project.yml requirements", () => {
    const yml = `packages:\n  Yams:\n    url: https://github.com/jpsim/Yams\n    from: 5.0.0\n  Local:\n    url: https://github.com/acme/local\n    branch: main\n`;
    const deps = parseSwiftProjectYml(yml);
    expect(deps.map((d) => [d.name, d.requirement])).toEqual([["Yams", "5.0.0"], ["local", null]]);
  });

  it("compares against GitHub tags and marks unsupported pins unknown", async () => {
    const repo = mkRepo({ "Package.swift": packageSwift });
    const { fetch } = mockFetch({
      "https://api.github.com/repos/apple/swift-argument-parser/tags?per_page=100": [{ name: "1.2.0" }, { name: "1.3.0" }, { name: "nightly" }],
      "https://api.github.com/repos/vapor/vapor/tags?per_page=100": [{ name: "v4.80.0" }, { name: "v4.81.1" }],
    });
    const entries = await checkSources(repo, manifest([{ kind: "swiftpm", manifest: "Package.swift" }]) as never, { fetch, exec: noExec });
    const byName = Object.fromEntries(entries.map((e) => [e.name, e]));
    expect(byName["swift-argument-parser"]).toMatchObject({ status: "outdated", latest: "1.3.0" });
    expect(byName.vapor).toMatchObject({ status: "outdated", latest: "4.81.1" });
    expect(byName.tool).toMatchObject({ status: "unknown", reason: "branch or revision pin" });
    expect(byName.other).toMatchObject({ status: "unknown", reason: "non-GitHub package URL" });
  });
});

describe("rightkit source", () => {
  it("compares the registry section of the versions file and reads drift only for right-git repos", async () => {
    const repo = mkRepo({
      "versions/pins.json": { npm: { "@rightkit/ci": "0.2.0", "@rightkit/release": "0.2.151" }, stagedNpm: {} },
    });
    const { fetch, calls } = mockFetch({
      "https://registry.npmjs.org/%40rightkit%2Fgit/latest": { version: "0.2.37" },
      "https://registry.npmjs.org/%40rightkit%2Frelease/latest": { version: "0.2.151" },
    });
    const drift = async () => ({ ok: true, driftDetected: true, visibility: { allowed: true }, errors: [] });
    const m = manifest([{ kind: "rightkit", versions: "versions/pins.json" }]) as never;

    const without = await checkSources(repo, m, { fetch, drift });
    expect(without.map((e) => [e.name, e.status])).toEqual([
      ["@rightkit/ci", "outdated"],
      ["@rightkit/release", "current"],
    ]);
    expect(calls).toHaveLength(2);

    fs.writeFileSync(path.join(repo, ".rightgit.json"), "{}");
    const withDrift = await checkSources(repo, m, { fetch, drift });
    const workflows = withDrift.find((e) => e.name === "rightgit-workflows");
    expect(workflows).toMatchObject({ status: "outdated", kind: "rightkit" });
  });

  it("reports a missing registry section as a source error", async () => {
    const repo = mkRepo({ "packages/release/rightkit-versions.json": { schema: 2 } });
    const entries = await checkSources(repo, manifest([{ kind: "rightkit" }]) as never, { fetch: mockFetch({}).fetch });
    expect(entries[0].error).toMatch(/no npm registry section/);
  });
});

describe("tool source", () => {
  it("compares a github-release tag and checks every platform asset is published", async () => {
    const repo = mkRepo({});
    const { fetch } = mockFetch({
      "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": {
        tag_name: "2025.10.01",
        assets: [{ name: "yt-dlp_macos" }, { name: "yt-dlp.exe" }],
      },
    });
    const entries = await checkSources(repo, manifest([validToolSource()]) as never, { fetch, exec: noExec });
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ name: "yt-dlp", kind: "tool", pinned: "2025.01.12", latest: "2025.10.01", status: "outdated", policy: "auto-land" });
    expect(entries[0].platforms).toEqual([
      { platform: "macos-arm64", asset: "yt-dlp_macos", checksum: SHA },
      { platform: "windows-x86_64", asset: "yt-dlp.exe", checksum: SHA },
    ]);
  });

  it("errors when a platform asset is missing upstream", async () => {
    const repo = mkRepo({});
    const { fetch } = mockFetch({
      "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": { tag_name: "2025.10.01", assets: [{ name: "yt-dlp_macos" }] },
    });
    const entries = await checkSources(repo, manifest([validToolSource()]) as never, { fetch, exec: noExec });
    expect(entries[0].error).toMatch(/assets missing from upstream release: windows-x86_64:yt-dlp\.exe/);
  });

  it("reads a url-type latest endpoint and holds sign-named tools by policy", async () => {
    const repo = mkRepo({});
    const { fetch } = mockFetch({ "https://example.com/latest.txt": "1.4.2\n" });
    const source = validToolSource({
      name: "signing-helper",
      pin: "1.4.2",
      upstream: { type: "url", latest: "https://example.com/latest.txt", download: "https://example.com/{version}/x" },
    });
    const entries = await checkSources(repo, manifest([source]) as never, { fetch, exec: noExec });
    expect(entries[0]).toMatchObject({ status: "current", latest: "1.4.2", policy: "held" });
  });

  it("rejects a latest URL that does not return a version", async () => {
    const repo = mkRepo({});
    const { fetch } = mockFetch({ "https://example.com/latest.txt": "<html>oops</html>" });
    const source = validToolSource({ upstream: { type: "url", latest: "https://example.com/latest.txt", download: "https://example.com/{version}" } });
    const entries = await checkSources(repo, manifest([source]) as never, { fetch, exec: noExec });
    expect(entries[0].error).toMatch(/did not return a version string/);
  });
});

describe("http bounds", () => {
  it("refuses a response larger than the cap", async () => {
    const big = "x".repeat(MAX_RESPONSE_BYTES + 10);
    const { fetch } = mockFetch({ "https://example.com/big": big });
    await expect(fetchText("https://example.com/big", { fetch })).rejects.toThrow(/exceeds/);
  });
});

describe("runCheck and exit codes", () => {
  it("exits 0 when everything is current and 2 when the manifest is missing", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": manifest([validToolSource()]),
    });
    const { fetch } = mockFetch({
      "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": { tag_name: "2025.01.12", assets: [{ name: "yt-dlp_macos" }, { name: "yt-dlp.exe" }] },
    });
    const report = await runCheck(repo, { fetch, exec: noExec });
    expect(report.exitCode).toBe(0);
    expect(report.entries[0].status).toBe("current");

    const missing = await runCheck(mkRepo({}), { fetch, exec: noExec });
    expect(missing.exitCode).toBe(2);
    expect(missing.errors[0]).toMatch(/cannot read/);
    expect(missing.entries).toEqual([]);
  });

  it("exits 2 for an invalid manifest without touching the network", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([{ kind: "gem" }]) });
    const { fetch, calls } = mockFetch({});
    const report = await runCheck(repo, { fetch });
    expect(report.exitCode).toBe(2);
    expect(calls).toEqual([]);
  });

  it("validates the manifest loader result independently", () => {
    expect(loadManifest(mkRepo({ ".rightkit/update.json": manifest([]) })).valid).toBe(true);
  });
});


describe("glob sources and shared lookups", () => {
  const crate = (deps: string) => `[package]\nname = "x"\n\n[dependencies]\n${deps}`;

  it("expands a cargo glob, skips hidden directories, and looks each crate up once", async () => {
    const repo = mkRepo({
      "crates/a/Cargo.toml": crate('serde = "1.0.100"\nanyhow = "1"\n'),
      "crates/b/Cargo.toml": crate('serde = "1.0.100"\ninternal = { path = "../a", version = "0.1.0" }\n'),
      "crates/.audit/Cargo.toml": crate('hidden = "1.0.0"\n'),
      "crates/c/not-a-manifest.txt": "x",
    });
    const { fetch, calls } = mockFetch({
      "https://crates.io/api/v1/crates/serde": { crate: { max_stable_version: "1.0.200" } },
      "https://crates.io/api/v1/crates/anyhow": { crate: { max_stable_version: "1.0.5" } },
    });
    const m = manifest([{ kind: "cargo", glob: "crates/*/Cargo.toml" }]) as never;
    const entries = await checkSources(repo, m, { fetch, exec: noExec });
    expect(entries.map((e) => `${e.source}:${e.name}:${e.status}`)).toEqual([
      "crates/a/Cargo.toml:serde:outdated",
      "crates/a/Cargo.toml:anyhow:current",
      "crates/b/Cargo.toml:serde:outdated",
      "crates/b/Cargo.toml:internal:unknown",
    ]);
    // A path dependency with a version is the release tooling's business, not a registry lookup.
    expect(entries.find((e) => e.name === "internal")?.reason).toBe("path or git dependency");
    expect(calls.filter((u) => u.endsWith("/serde"))).toHaveLength(1);
    expect(calls.some((u) => u.includes("hidden") || u.includes("internal"))).toBe(false);
  });

  it("expands a pnpm glob and shares npm registry answers across manifests", async () => {
    const repo = mkRepo({
      "packages/one/package.json": { name: "one", dependencies: { ms: "^2.0.0" } },
      "packages/two/package.json": { name: "two", devDependencies: { ms: "^2.0.0" } },
    });
    const { fetch, calls } = mockFetch({ "https://registry.npmjs.org/ms/latest": { version: "2.1.3" } });
    const entries = await checkSources(repo, manifest([{ kind: "pnpm", glob: "packages/*/package.json" }]) as never, { fetch, exec: noExec });
    expect(entries.map((e) => `${e.source}:${e.name}:${e.status}`)).toEqual(["packages/one/package.json:ms:outdated", "packages/two/package.json:ms:outdated"]);
    expect(calls.filter((u) => u.endsWith("/ms/latest"))).toHaveLength(1);
  });

  it("a glob that matches nothing is an error entry, not silence", async () => {
    const repo = mkRepo({ "README.md": "x" });
    const entries = await checkSources(repo, manifest([{ kind: "cargo", glob: "crates/*/Cargo.toml" }]) as never, { fetch: mockFetch({}).fetch, exec: noExec });
    expect(entries[0].error).toMatch(/glob crates\/\*\/Cargo\.toml matched no files/);
    expect(exitCodeFor(entries)).toBe(2);
  });

  it("one unreadable manifest in a glob does not hide the others", async () => {
    const repo = mkRepo({
      "crates/a/Cargo.toml": crate('serde = "1.0.100"\n'),
      "crates/b/Cargo.toml": "[dependencies\nbroken",
    });
    const { fetch } = mockFetch({ "https://crates.io/api/v1/crates/serde": { crate: { max_stable_version: "1.0.200" } } });
    const entries = await checkSources(repo, manifest([{ kind: "cargo", glob: "crates/*/Cargo.toml" }]) as never, { fetch, exec: noExec });
    expect(entries.find((e) => e.name === "serde")?.status).toBe("outdated");
    expect(entries.find((e) => e.source === "crates/b/Cargo.toml")?.error).toBeTruthy();
  });
});

describe("http retries", () => {
  it("retries a 429 and then succeeds, and gives up after four attempts", async () => {
    let n = 0;
    const flaky = async () => (++n < 3 ? new Response("slow down", { status: 429 }) : new Response("ok", { status: 200 }));
    const slept: number[] = [];
    const text = await fetchText("https://example.com/x", { fetch: flaky as never, sleep: async (ms) => void slept.push(ms) });
    expect(text).toBe("ok");
    expect(n).toBe(3);
    expect(slept).toEqual([1000, 2000]);

    let m = 0;
    const down = async () => (m++, new Response("down", { status: 503 }));
    await expect(fetchText("https://example.com/x", { fetch: down as never, sleep: async () => undefined })).rejects.toThrow(/HTTP 503/);
    expect(m).toBe(4);
  });

  it("does not retry a 404", async () => {
    let n = 0;
    const missing = async () => (n++, new Response("no", { status: 404 }));
    await expect(fetchText("https://example.com/x", { fetch: missing as never, sleep: async () => undefined })).rejects.toThrow(/HTTP 404/);
    expect(n).toBe(1);
  });
});
