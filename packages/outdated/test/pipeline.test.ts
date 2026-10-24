import { describe, expect, it } from "vitest";
import fs from "node:fs";
import path from "node:path";
import { fakeRun, git, gitRepo, manifest, mockFetch, ok } from "./support.js";
import { runCli, type CliIo } from "../src/run.js";

const NODE = process.execPath;
const LOCK = '[[package]]\nname = "a"\nversion = "1.0.0"\n';

function cargoFake(repo: string) {
  return (argv: string[]) => {
    if (argv[0] !== "rustright") return undefined;
    const version = argv[argv.indexOf("--precise") + 1];
    fs.writeFileSync(path.join(repo, "Cargo.lock"), LOCK.replace("1.0.0", version));
    return ok();
  };
}

function pipelineRepo(gates: unknown[] = [{ name: "ok", command: [NODE, "-e", "0"] }]) {
  return gitRepo({
    ".rightkit/update.json": { ...manifest([{ kind: "cargo", manifest: "Cargo.toml" }]), gates },
    "Cargo.toml": '[dependencies]\na = "1.0.0"\n',
    "Cargo.lock": LOCK,
  });
}

function drive(repo: string, args: string[], latest = "1.0.5") {
  const out: string[] = [];
  const err: string[] = [];
  const { fetch } = mockFetch({ "https://crates.io/api/v1/crates/a": { crate: { max_stable_version: latest } } });
  const { run, calls } = fakeRun(cargoFake(repo));
  const io: CliIo = {
    stdout: (t) => out.push(t),
    stderr: (t) => err.push(t),
    deps: { fetch, run, exec: async () => ({ stdout: "{}" }) },
    now: () => new Date("2026-10-11T00:00:00Z"),
  };
  return {
    go: () => runCli(args, io),
    events: () => out.join("").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l) as Record<string, unknown>),
    text: () => out.join(""),
    err: () => err.join(""),
    calls,
  };
}

describe("run: the scheduled pipeline", () => {
  it("checks, applies, gates and lands one commit, and logs machine-readable events", async () => {
    const repo = pipelineRepo();
    const d = drive(repo, ["run", "--repo", repo, "--quiet"]);
    const code = await d.go();
    expect(code).toBe(0);
    const events = d.events();
    expect(events.map((e) => e.event)).toEqual(["update.run.start", "update.gate.result", "update.land.result", "update.run.end"]);
    expect(events[0]).toMatchObject({ event: "update.run.start", command: "run", repo, ts: "2026-10-11T00:00:00.000Z" });
    expect(events[1]).toMatchObject({ event: "update.gate.result", ok: true, exitCode: 0, gates: [{ name: "ok", ok: true }] });
    expect(events[2]).toMatchObject({ event: "update.land.result", ok: true, committed: true, pushed: false, changes: 1 });
    expect(events[3]).toMatchObject({ event: "update.run.end", command: "run", exitCode: 0 });
    expect(String(events[3].outcome)).toMatch(/^landed [0-9a-f]{40}$/);
    expect(git(repo, "log", "-1", "--format=%s")).toBe("deps: 1 updates via right-update");
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe('[dependencies]\na = "1.0.5"\n');
    expect(git(repo, "status", "--porcelain")).toBe("");
    // Every stdout line is JSON; nothing else leaks into the log.
    expect(d.text().trim().split("\n").every((l) => l.startsWith("{"))).toBe(true);
  });

  it("does nothing and exits 0 when everything is current", async () => {
    const repo = pipelineRepo();
    const d = drive(repo, ["run", "--repo", repo, "--quiet"], "1.0.0");
    expect(await d.go()).toBe(0);
    expect(d.events().at(-1)).toMatchObject({ event: "update.run.end", exitCode: 0, outcome: "all current" });
    expect(git(repo, "log", "--oneline").split("\n")).toHaveLength(1);
  });

  it("discards the applied changes when a gate fails, leaving a clean tree and exit 1", async () => {
    const repo = pipelineRepo([{ name: "bad", command: [NODE, "-e", "process.exit(1)"] }]);
    const d = drive(repo, ["run", "--repo", repo, "--quiet"]);
    expect(await d.go()).toBe(1);
    const events = d.events();
    expect(events.map((e) => e.event)).toEqual(["update.run.start", "update.gate.result", "update.run.end"]);
    expect(events[1]).toMatchObject({ ok: false, exitCode: 1 });
    expect(String(events[2].outcome)).toMatch(/gate did not pass \(exit 1\); applied changes were discarded/);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe('[dependencies]\na = "1.0.0"\n');
    expect(git(repo, "status", "--porcelain")).toBe("");
    expect(git(repo, "log", "--oneline").split("\n")).toHaveLength(1);
  });

  it("refuses up front when the tree is dirty, before applying or running any gate", async () => {
    const repo = pipelineRepo();
    fs.writeFileSync(path.join(repo, "wip.txt"), "work in progress");
    const d = drive(repo, ["run", "--repo", repo, "--quiet"]);
    expect(await d.go()).toBe(1);
    expect(String(d.events().at(-1)!.outcome)).toMatch(/refused: working tree is not clean \(wip\.txt\)/);
    expect(d.calls.some((c) => c[0] === "rustright")).toBe(false);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe('[dependencies]\na = "1.0.0"\n');
  });

  it("exits 2 when check cannot look something up", async () => {
    const repo = pipelineRepo();
    const out: string[] = [];
    const io: CliIo = { stdout: (t) => out.push(t), stderr: () => undefined, deps: { fetch: mockFetch({}).fetch, exec: async () => ({ stdout: "{}" }) } };
    expect(await runCli(["run", "--repo", repo, "--quiet"], io)).toBe(2);
    expect(String(JSON.parse(out.join("").trim().split("\n").at(-1)!).outcome)).toMatch(/check failed/);
  });

  it("--dry-run plans without changing anything", async () => {
    const repo = pipelineRepo();
    const d = drive(repo, ["run", "--repo", repo, "--dry-run", "--quiet"]);
    expect(await d.go()).toBe(0);
    expect(String(d.events().at(-1)!.outcome)).toMatch(/dry run: 1 change\(s\) planned/);
    expect(git(repo, "status", "--porcelain")).toBe("");
  });
});

describe("apply, gate and land through the CLI", () => {
  it("apply --json prints the report, and `--only` takes several names", async () => {
    const repo = pipelineRepo();
    const d = drive(repo, ["apply", "--repo", repo, "--only", "a,zzz", "--dry-run", "--json"]);
    expect(await d.go()).toBe(2); // zzz does not exist
    const missing = drive(repo, ["apply", "--repo", repo, "--only", "a", "--dry-run", "--json"]);
    expect(await missing.go()).toBe(0);
    const parsed = JSON.parse(missing.text());
    expect(parsed.report.changes[0]).toMatchObject({ name: "a", before: "1.0.0", after: "1.0.5", status: "planned" });
  });

  it("apply then gate then land, one command at a time", async () => {
    const repo = pipelineRepo();
    expect(await drive(repo, ["apply", "--repo", repo]).go()).toBe(0);
    expect(await drive(repo, ["gate", "--repo", repo]).go()).toBe(0);
    const landing = drive(repo, ["land", "--repo", repo]);
    expect(await landing.go()).toBe(0);
    expect(landing.text()).toMatch(/committed [0-9a-f]{40} \(not pushed\)/);
    expect(git(repo, "log", "-1", "--format=%s")).toBe("deps: 1 updates via right-update");
  });

  it("land without a gate exits 1 with the refusal", async () => {
    const repo = pipelineRepo();
    await drive(repo, ["apply", "--repo", repo]).go();
    const landing = drive(repo, ["land", "--repo", repo]);
    expect(await landing.go()).toBe(1);
    expect(landing.text()).toMatch(/refused: no usable gate-report/);
  });

  it("discard undoes an unlanded apply", async () => {
    const repo = pipelineRepo();
    await drive(repo, ["apply", "--repo", repo]).go();
    expect(await drive(repo, ["discard", "--repo", repo]).go()).toBe(0);
    expect(git(repo, "status", "--porcelain")).toBe("");
  });
});
