import { describe, expect, it } from "vitest";
import { mkRepo, manifest, mockFetch, validToolSource } from "./support.js";
import { runCli, type CliIo } from "../src/run.js";

function io(deps: CliIo["deps"] = {}) {
  const out: string[] = [];
  const err: string[] = [];
  const cli: CliIo = { stdout: (t) => out.push(t), stderr: (t) => err.push(t), deps };
  return { cli, out: () => out.join(""), err: () => err.join("") };
}

const release = (tag: string) => ({
  "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest": { tag_name: tag, assets: [{ name: "yt-dlp_macos" }, { name: "yt-dlp.exe" }] },
});

describe("right-update CLI", () => {
  it("check --json prints the report shape and exits 1 when something is outdated", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([validToolSource()]) });
    const { fetch } = mockFetch(release("2025.10.01"));
    const { cli, out } = io({ fetch, exec: async () => ({ stdout: "{}" }) });
    const code = await runCli(["check", "--repo", repo, "--json"], cli);
    expect(code).toBe(1);
    const report = JSON.parse(out());
    expect(report).toMatchObject({ schemaVersion: 1, repo, exitCode: 1, errors: [] });
    expect(report.entries[0]).toMatchObject({
      name: "yt-dlp",
      kind: "tool",
      source: "tool:yt-dlp",
      pinned: "2025.01.12",
      latest: "2025.10.01",
      status: "outdated",
      policy: "auto-land",
    });
  });

  it("check exits 0 when all current", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([validToolSource()]) });
    const { fetch } = mockFetch(release("2025.01.12"));
    const { cli, out } = io({ fetch, exec: async () => ({ stdout: "{}" }) });
    expect(await runCli(["check", "--repo", repo], cli)).toBe(0);
    expect(out()).toMatch(/1 current, 0 outdated, 0 unknown, exit 0/);
  });

  it("check exits 2 on a lookup error", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([validToolSource()]) });
    const { fetch } = mockFetch({});
    const { cli, out } = io({ fetch, exec: async () => ({ stdout: "{}" }) });
    expect(await runCli(["check", "--repo", repo, "--json"], cli)).toBe(2);
    expect(JSON.parse(out()).entries[0].error).toMatch(/upstream lookup failed/);
  });

  it("check exits 2 with validation errors for an invalid manifest", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([{ kind: "gem" }]) });
    const { cli, out } = io({ fetch: mockFetch({}).fetch });
    expect(await runCli(["check", "--repo", repo, "--json"], cli)).toBe(2);
    const report = JSON.parse(out());
    expect(report.exitCode).toBe(2);
    expect(report.errors[0]).toMatch(/unknown kind "gem"/);
  });

  it("rejects unknown commands and flags with exit 2", async () => {
    expect(await runCli(["publish"], io().cli)).toBe(2);
    expect(await runCli(["check", "--bogus"], io().cli)).toBe(2);
    expect(await runCli(["check", "--repo"], io().cli)).toBe(2);
    for (const command of ["apply", "gate", "land", "discard", "run"]) {
      const { cli, err } = io();
      expect(await runCli([command, "--bogus"], cli)).toBe(2);
      expect(err()).toMatch(/unknown argument: --bogus/);
    }
    // check does not take apply's flags.
    expect(await runCli(["check", "--dry-run"], io().cli)).toBe(2);
    expect(await runCli(["apply", "--only"], io().cli)).toBe(2);
    expect(await runCli(["schedule", "frobnicate"], io().cli)).toBe(2);
    expect(await runCli(["schedule", "install", "--interval", "soon"], io().cli)).toBe(2);
  });

  it("prints version and help", async () => {
    const version = io();
    expect(await runCli(["--version"], version.cli)).toBe(0);
    expect(version.out()).toMatch(/^0\.3\.0\n$/);
    const help = io();
    expect(await runCli(["--help"], help.cli)).toBe(0);
    expect(help.out()).toMatch(/right-update check/);
    expect(help.out()).toMatch(/right-update schedule install\|remove\|status/);
  });
});
