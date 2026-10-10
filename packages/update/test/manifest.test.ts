import { describe, expect, it } from "vitest";
import { mkRepo, manifest, validToolSource, SHA } from "./support.js";
import { validateManifest, loadManifest } from "../src/manifest.js";
import { resolvePolicy, compareVersions } from "../src/compare.js";

const fullManifest = () =>
  manifest(
    [
      { kind: "cargo", manifest: "Cargo.toml" },
      { kind: "pnpm", manifest: "packages/app/package.json" },
      { kind: "swiftpm", manifest: "Package.swift" },
      { kind: "rightkit" },
      validToolSource({ policy: "agent-pr" }),
    ],
    { default: "auto-land", held: ["rightkit-license"], tier: { held: ["/^ffmpeg/"] } },
  );

describe("manifest validation", () => {
  it("accepts a valid manifest with every source kind", () => {
    const result = validateManifest(fullManifest());
    expect(result.errors).toEqual([]);
    expect(result.valid).toBe(true);
  });

  it("rejects an unknown source kind", () => {
    const result = validateManifest(manifest([{ kind: "npmx", manifest: "package.json" }]));
    expect(result.valid).toBe(false);
    expect(result.errors.join("\n")).toMatch(/unknown kind "npmx"/);
  });

  it("rejects an unknown policy value", () => {
    const result = validateManifest(manifest([], { default: "yolo" }));
    expect(result.valid).toBe(false);
    expect(result.errors.join("\n")).toMatch(/policy\.default/);
  });

  it("rejects a tool missing required fields", () => {
    const source: Record<string, unknown> = validToolSource();
    delete source.journey;
    delete source.assets;
    const result = validateManifest(manifest([source]));
    expect(result.valid).toBe(false);
    expect(result.errors.join("\n")).toMatch(/journey must be an object/);
    expect(result.errors.join("\n")).toMatch(/assets must be a non-empty object/);
  });

  it("rejects a tool with a malformed checksum or unknown platform", () => {
    const bad = validToolSource({
      assets: { "solaris-sparc": { asset: "x", checksum: "md5:abc" } },
    });
    const result = validateManifest(manifest([bad]));
    expect(result.valid).toBe(false);
    expect(result.errors.join("\n")).toMatch(/unknown platform/);
  });

  it("requires checksum sha256 format on each platform asset", () => {
    const bad = validToolSource({ assets: { "linux-x86_64": { asset: "yt-dlp", checksum: "sha256:short" } } });
    const result = validateManifest(manifest([bad]));
    expect(result.errors.join("\n")).toMatch(/checksum must be sha256/);
  });

  it("accepts an extract member and rejects a traversing one", () => {
    const ok = validToolSource({ assets: { "linux-x86_64": { asset: "a.tar.gz", checksum: SHA, extract: { member: "bin/tool" } } } });
    expect(validateManifest(manifest([ok])).valid).toBe(true);
    const bad = validToolSource({ assets: { "linux-x86_64": { asset: "a.tar.gz", checksum: SHA, extract: { member: "../etc" } } } });
    expect(validateManifest(manifest([bad])).valid).toBe(false);
  });

  it("rejects journey placeholders other than the four allowed ones", () => {
    const bad = validToolSource({
      journey: { command: ["node", "x.mjs", "--dir", "{secrets}"], report: "{out}/report.json" },
    });
    const result = validateManifest(manifest([bad]));
    expect(result.valid).toBe(false);
    expect(result.errors.join("\n")).toMatch(/unknown placeholder \{secrets\}/);
  });

  it("accepts all four journey placeholders", () => {
    const ok = validToolSource({
      journey: {
        command: ["node", "x.mjs", "{candidate}", "{out}", "{delivery}", "{model}"],
        report: "{out}/report.json",
        inputs: { case: "youtube" },
      },
    });
    expect(validateManifest(manifest([ok])).valid).toBe(true);
  });

  it("rejects a non-https upstream URL and a download template without {version}", () => {
    const http = validToolSource({ upstream: { type: "url", latest: "http://example.com/v", download: "https://example.com/{version}" } });
    expect(validateManifest(manifest([http])).errors.join("\n")).toMatch(/https URL/);
    const noTemplate = validToolSource({ upstream: { type: "url", latest: "https://example.com/v", download: "https://example.com/x" } });
    expect(validateManifest(manifest([noTemplate])).errors.join("\n")).toMatch(/must contain \{version\}/);
  });

  it("rejects a swiftpm manifest that is not Package.swift or project.yml", () => {
    const result = validateManifest(manifest([{ kind: "swiftpm", manifest: "Podfile" }]));
    expect(result.errors.join("\n")).toMatch(/Package\.swift or project\.yml/);
  });

  it("rejects absolute and parent-relative source paths", () => {
    const result = validateManifest(manifest([{ kind: "cargo", manifest: "/etc/Cargo.toml" }, { kind: "pnpm", manifest: "../package.json" }]));
    expect(result.valid).toBe(false);
    expect(result.errors.length).toBe(2);
  });

  it("loadManifest reads .rightkit/update.json and reports invalid JSON", () => {
    const good = mkRepo({ ".rightkit/update.json": manifest([]) });
    expect(loadManifest(good).valid).toBe(true);
    const broken = mkRepo({ ".rightkit/update.json": "{ nope" });
    expect(loadManifest(broken).errors[0]).toMatch(/not valid JSON/);
    expect(loadManifest(mkRepo({})).errors[0]).toMatch(/cannot read/);
  });
});

describe("policy resolution", () => {
  const config = { default: "auto-land" as const, held: ["rightkit-license"], tier: { held: ["/^ffmpeg/"] } };

  it("holds named dependencies regardless of an explicit policy", () => {
    expect(resolvePolicy("rightkit-license", "auto-land", config)).toBe("held");
  });

  it("defaults sign, secur, licen and credential names to held", () => {
    expect(resolvePolicy("signing-agent", undefined, config)).toBe("held");
    expect(resolvePolicy("security-core", undefined, config)).toBe("held");
    expect(resolvePolicy("rightkit-licensing", undefined, config)).toBe("held");
    expect(resolvePolicy("credential-store", undefined, config)).toBe("held");
  });

  it("lets an explicit entry policy override the regex default", () => {
    expect(resolvePolicy("signing-agent", "agent-pr", config)).toBe("agent-pr");
  });

  it("applies tier patterns and falls back to the default", () => {
    expect(resolvePolicy("ffmpeg-static", undefined, config)).toBe("held");
    expect(resolvePolicy("yt-dlp", undefined, config)).toBe("auto-land");
  });
});

describe("version comparison", () => {
  it("compares only the components a pin names", () => {
    expect(compareVersions("1.2", "1.2.5").status).toBe("current");
    expect(compareVersions("1.2", "1.3.0").status).toBe("outdated");
    expect(compareVersions("^1.2.3", "1.2.4").status).toBe("outdated");
    expect(compareVersions("=2.0.0", "2.0.0").status).toBe("current");
  });

  it("compares date-style tool pins numerically", () => {
    expect(compareVersions("2025.01.12", "2025.10.01").status).toBe("outdated");
    expect(compareVersions("v2025.10.01", "2025.10.01").status).toBe("current");
  });

  it("marks non-comparable pins unknown", () => {
    expect(compareVersions("beta-build", "1.0.0").status).toBe("unknown");
  });
});

describe("manifest: 0.2 fields", () => {
  it("accepts glob on cargo and pnpm sources, and rejects both, neither, or an unsafe glob", () => {
    const good = validateManifest(manifest([{ kind: "cargo", glob: "crates/*/Cargo.toml" }, { kind: "pnpm", glob: "packages/*/package.json" }]));
    expect(good.errors).toEqual([]);
    const both = validateManifest(manifest([{ kind: "cargo", manifest: "Cargo.toml", glob: "crates/*/Cargo.toml" }]));
    expect(both.errors.join("\n")).toMatch(/exactly one of manifest and glob/);
    const neither = validateManifest(manifest([{ kind: "pnpm" }]));
    expect(neither.errors.join("\n")).toMatch(/exactly one of manifest and glob/);
    for (const glob of ["**/Cargo.toml", "crates/*/lib.rs", "../x/Cargo.toml", "/abs/*/Cargo.toml", "crates/?/Cargo.toml", "crates/{a,b}/Cargo.toml"]) {
      expect(validateManifest(manifest([{ kind: "cargo", glob }])).valid, glob).toBe(false);
    }
    // A pnpm glob must end in package.json, not Cargo.toml.
    expect(validateManifest(manifest([{ kind: "pnpm", glob: "crates/*/Cargo.toml" }])).valid).toBe(false);
  });

  it("validates gates", () => {
    const ok = validateManifest({ ...manifest([]), gates: [{ name: "tests", command: ["pnpm", "test"], cwd: "packages/x", timeoutSec: 60 }] });
    expect(ok.errors).toEqual([]);
    const cases: Array<[unknown, RegExp]> = [
      [[{ name: "x", command: [] }], /command must be a non-empty array/],
      [[{ name: "x", command: ["a"], timeoutSec: 0 }], /timeoutSec/],
      [[{ name: "x", command: ["a"], cwd: "../up" }], /cwd must be a relative path/],
      [[{ name: "bad name", command: ["a"] }], /name must match/],
      [[{ name: "x", command: ["a"] }, { name: "x", command: ["b"] }], /duplicates x/],
      [[{ name: "x", command: ["a"], shell: true }], /unknown field shell/],
      ["nope", /gates must be an array/],
    ];
    for (const [gates, pattern] of cases) {
      const result = validateManifest({ ...manifest([]), gates });
      expect(result.valid).toBe(false);
      expect(result.errors.join("\n")).toMatch(pattern);
    }
  });

  it("validates policy.land, journey.model, journey.timeoutSec and asset maxBytes", () => {
    expect(validateManifest(manifest([], { default: "auto-land", land: "push" })).valid).toBe(true);
    expect(validateManifest(manifest([], { default: "auto-land", land: "pr" })).errors.join("\n")).toMatch(/policy\.land/);
    const withJourney = (journey: Record<string, unknown>) =>
      validateManifest(manifest([validToolSource({ journey: { command: ["x"], report: "{out}/r.json", ...journey } })]));
    expect(withJourney({ model: "whisper-large-v3", timeoutSec: 600 }).errors).toEqual([]);
    expect(withJourney({ model: "bad model!" }).errors.join("\n")).toMatch(/journey\.model/);
    expect(withJourney({ timeoutSec: 99999999 }).errors.join("\n")).toMatch(/journey\.timeoutSec/);
    const asset = (maxBytes: unknown) =>
      validateManifest(manifest([validToolSource({ assets: { "macos-arm64": { asset: "a", checksum: SHA, maxBytes } } })]));
    expect(asset(1024).errors).toEqual([]);
    expect(asset(0).errors.join("\n")).toMatch(/maxBytes/);
  });
});
