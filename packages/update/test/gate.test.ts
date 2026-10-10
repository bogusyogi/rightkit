import { describe, expect, it } from "vitest";
import fs from "node:fs";
import path from "node:path";
import { manifest, mkRepo, stageTool, validToolSource, writeApplyReport, sha256 } from "./support.js";
import { fillPlaceholders, runGateCommand } from "../src/gate.js";
import { runCli } from "../src/run.js";

const NODE = process.execPath;
const node = (code: string, ...args: string[]) => [NODE, "-e", code, ...args];

function journeySource(script: string, extra: Record<string, unknown> = {}) {
  return validToolSource({
    journey: {
      command: [NODE, "journey.mjs", "{candidate}", "{out}", "{delivery}", "{model}"],
      report: "{out}/report.json",
      inputs: { case: "youtube" },
      ...extra,
    },
  });
}

function repoWithJourney(script: string, extra: Record<string, unknown> = {}, gates?: unknown[]) {
  const repo = mkRepo({
    ".rightkit/update.json": { ...manifest([journeySource(script, extra)]), ...(gates ? { gates } : {}) },
    "journey.mjs": script,
  });
  const change = stageTool(repo);
  writeApplyReport(repo, [change]);
  return repo;
}

const WRITE_REPORT = (body: string) => `
import fs from "node:fs";
import path from "node:path";
const [candidate, out, delivery, model] = process.argv.slice(2);
const stdin = fs.readFileSync(0, "utf8");
fs.writeFileSync(path.join(out, "seen.json"), JSON.stringify({ candidate, out, delivery, model, stdin, files: fs.readdirSync(candidate) }));
${body}
`;

describe("fillPlaceholders", () => {
  it("fills the four placeholders and nothing else", () => {
    const values = { candidate: "/c", out: "/o", delivery: "/d", model: "m1" };
    expect(fillPlaceholders("{candidate}|{out}|{delivery}|{model}|{other}", values)).toBe("/c|/o|/d|m1|{other}");
    expect(fillPlaceholders("{model}", { ...values, model: "" })).toBe("");
  });
});

describe("gate: repository gates", () => {
  it("exits 0 when every gate passes and saves stdout and stderr under the run dir", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": { ...manifest([]), gates: [{ name: "one", command: node("console.log('hello out'); console.error('hello err')") }, { name: "two", command: node("0") }] },
    });
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.report!.passed).toBe(true);
    const one = outcome.report!.gates[0];
    expect(fs.readFileSync(path.join(repo, one.stdoutPath), "utf8")).toBe("hello out\n");
    expect(fs.readFileSync(path.join(repo, one.stderrPath), "utf8")).toBe("hello err\n");
    expect(one.stdoutPath).toMatch(/^\.rightkit\/update\/gate\/.+\/gates\/one\.stdout\.log$/);
    // The on-disk report is what land reads.
    const onDisk = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update/gate-report.json"), "utf8"));
    expect(onDisk).toMatchObject({ passed: true, exitCode: 0, applyReportSha256: null });
    expect(onDisk.warnings).toEqual([]);
  });

  it("exits 1 when a gate fails, and still runs the remaining gates in order", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": { ...manifest([]), gates: [{ name: "bad", command: node("process.exit(7)") }, { name: "after", command: node("0") }] },
    });
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.gates.map((g) => [g.name, g.ok, g.exitCode])).toEqual([["bad", false, 7], ["after", true, 0]]);
    expect(outcome.report!.passed).toBe(false);
  });

  it("kills a gate that exceeds its timeout and reports it as failed", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": { ...manifest([]), gates: [{ name: "slow", command: node("setTimeout(()=>{}, 60000)"), timeoutSec: 1 }] },
    });
    const started = Date.now();
    const outcome = await runGateCommand({ repo });
    expect(Date.now() - started).toBeLessThan(15_000);
    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.gates[0]).toMatchObject({ ok: false, timedOut: true });
    expect(outcome.report!.gates[0].detail).toMatch(/timed out after 1s/);
  });

  it("spawns argv directly: shell metacharacters are literal text", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": { ...manifest([]), gates: [{ name: "literal", command: node("process.stdout.write(process.argv[1])", "$(echo pwned); `id` && touch hacked") }] },
    });
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(fs.readFileSync(path.join(repo, outcome.report!.gates[0].stdoutPath), "utf8")).toBe("$(echo pwned); `id` && touch hacked");
    expect(fs.existsSync(path.join(repo, "hacked"))).toBe(false);
  });

  it("runs a gate in its cwd and fails cleanly for a missing executable", async () => {
    const repo = mkRepo({
      "sub/marker.txt": "x",
      ".rightkit/update.json": {
        ...manifest([]),
        gates: [
          { name: "cwd", command: node("require('fs').accessSync('marker.txt')"), cwd: "sub" },
          { name: "missing", command: ["definitely-not-a-real-binary-xyz"] },
        ],
      },
    });
    const outcome = await runGateCommand({ repo });
    expect(outcome.report!.gates[0].ok).toBe(true);
    expect(outcome.report!.gates[1]).toMatchObject({ ok: false });
    expect(outcome.report!.gates[1].detail).toMatch(/ENOENT/);
    expect(outcome.exitCode).toBe(1);
  });

  it("warns when the manifest has no gates", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([]) });
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.report!.warnings[0]).toMatch(/no gates/);
  });
});

describe("gate: tool journeys", () => {
  it("substitutes {candidate}, {out}, {delivery} and {model}, passes inputs on stdin, and passes on ok:true", async () => {
    const script = WRITE_REPORT(`fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ ok: true, extra: 1 }));`);
    const repo = repoWithJourney(script, { model: "whisper-large-v3" });
    const delivery = path.join(repo, "artifact-root");
    const outcome = await runGateCommand({ repo }, { host: { env: { RIGHTKIT_ARTIFACT_DIR: delivery } } });

    expect(outcome.errors).toEqual([]);
    expect(outcome.exitCode).toBe(0);
    const journey = outcome.report!.journeys[0];
    expect(journey).toMatchObject({ name: "yt-dlp", ok: true, inconclusive: false });
    const seen = JSON.parse(fs.readFileSync(path.join(repo, journey.outDir, "seen.json"), "utf8"));
    expect(seen.candidate).toBe(path.join(repo, ".rightkit/update/candidates/yt-dlp/2025.10.01"));
    expect(seen.files).toEqual(["yt-dlp_macos"]);
    expect(seen.out).toBe(path.join(repo, journey.outDir));
    expect(seen.delivery).toBe(path.join(delivery, "yt-dlp", "2025.10.01"));
    expect(seen.model).toBe("whisper-large-v3");
    expect(JSON.parse(seen.stdin)).toEqual({ case: "youtube" });
    // Bound to the apply-report it ran against.
    const applySha = sha256(fs.readFileSync(path.join(repo, ".rightkit/update/apply-report.json")));
    expect(outcome.report!.applyReportSha256).toBe(applySha);
  });

  it("gives {model} as an empty string when none is declared, and uses a fresh out dir per run", async () => {
    const script = WRITE_REPORT(`fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ ok: true }));`);
    const repo = repoWithJourney(script);
    const first = await runGateCommand({ repo }, { now: () => new Date("2026-10-11T01:00:00Z") });
    const second = await runGateCommand({ repo }, { now: () => new Date("2026-10-11T02:00:00Z") });
    const seen = JSON.parse(fs.readFileSync(path.join(repo, first.report!.journeys[0].outDir, "seen.json"), "utf8"));
    expect(seen.model).toBe("");
    expect(first.report!.journeys[0].outDir).not.toBe(second.report!.journeys[0].outDir);
  });

  const cases: Array<[string, string, number, { ok: boolean; inconclusive: boolean }]> = [
    ["ok:false report", `fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ ok: false }));`, 1, { ok: false, inconclusive: false }],
    ["missing report", ``, 3, { ok: false, inconclusive: true }],
    ["report without ok", `fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ passed: true }));`, 3, { ok: false, inconclusive: true }],
    ["non-boolean ok", `fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ ok: "yes" }));`, 3, { ok: false, inconclusive: true }],
    ["invalid JSON report", `fs.writeFileSync(path.join(out, "report.json"), "{not json");`, 3, { ok: false, inconclusive: true }],
    ["array report", `fs.writeFileSync(path.join(out, "report.json"), "[true]");`, 3, { ok: false, inconclusive: true }],
    ["journey exits 3", `process.exit(3);`, 3, { ok: false, inconclusive: true }],
    ["journey exits 1 even with an ok report", `fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ ok: true })); process.exit(1);`, 1, { ok: false, inconclusive: false }],
  ];
  for (const [label, body, code, expected] of cases) {
    it(`exit ${code} for ${label}`, async () => {
      const repo = repoWithJourney(WRITE_REPORT(body));
      const outcome = await runGateCommand({ repo });
      expect(outcome.exitCode).toBe(code);
      expect(outcome.report!.journeys[0]).toMatchObject(expected);
      expect(outcome.report!.passed).toBe(false);
    });
  }

  it("kills a journey that exceeds its timeout", async () => {
    const repo = repoWithJourney(`setTimeout(() => {}, 60000);`, { timeoutSec: 1 });
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.report!.journeys[0]).toMatchObject({ timedOut: true, ok: false });
  });

  it("a failed gate outranks an inconclusive journey", async () => {
    const repo = repoWithJourney(WRITE_REPORT(``), {}, [{ name: "bad", command: node("process.exit(1)") }]);
    const outcome = await runGateCommand({ repo });
    expect(outcome.report!.journeys[0].inconclusive).toBe(true);
    expect(outcome.exitCode).toBe(1);
  });

  it("runs journeys alongside the gate lane (their time windows overlap)", async () => {
    const script = WRITE_REPORT(
      `fs.writeFileSync(path.join(out, "start"), String(Date.now())); await new Promise((r) => setTimeout(r, 600)); fs.writeFileSync(path.join(out, "end"), String(Date.now())); fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ ok: true }));`,
    );
    const gate = node(
      "const fs = require('fs'); fs.writeFileSync('gate-start', String(Date.now())); setTimeout(() => fs.writeFileSync('gate-end', String(Date.now())), 600)",
    );
    const repo = repoWithJourney(script, {}, [{ name: "wait", command: gate }]);
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(0);
    const num = (file: string) => Number(fs.readFileSync(file, "utf8"));
    const out = path.join(repo, outcome.report!.journeys[0].outDir);
    const [gs, ge] = [num(path.join(repo, "gate-start")), num(path.join(repo, "gate-end"))];
    const [js, je] = [num(path.join(out, "start")), num(path.join(out, "end"))];
    expect(gs).toBeLessThan(je);
    expect(js).toBeLessThan(ge);
  });

  it("exits 2 when the staged candidate is missing", async () => {
    const repo = repoWithJourney(WRITE_REPORT(``));
    fs.rmSync(path.join(repo, ".rightkit/update/candidates"), { recursive: true });
    const outcome = await runGateCommand({ repo });
    expect(outcome.exitCode).toBe(2);
    expect(outcome.errors[0]).toMatch(/staged candidate for yt-dlp is missing/);
  });
});

describe("gate: CLI", () => {
  it("returns the gate exit code and prints events in --quiet mode", async () => {
    const repo = mkRepo({
      ".rightkit/update.json": { ...manifest([]), gates: [{ name: "bad", command: node("process.exit(1)") }] },
    });
    const out: string[] = [];
    const code = await runCli(["gate", "--repo", repo, "--quiet"], { stdout: (t) => out.push(t), stderr: () => undefined, now: () => new Date("2026-10-11T00:00:00Z") });
    expect(code).toBe(1);
    const events = out.join("").trim().split("\n").map((l) => JSON.parse(l));
    expect(events.map((e) => e.event)).toEqual(["update.run.start", "update.gate.result", "update.run.end"]);
    expect(events[1]).toMatchObject({ ok: false, exitCode: 1, gates: [{ name: "bad", ok: false, exitCode: 1 }] });
    expect(events[2]).toMatchObject({ command: "gate", exitCode: 1 });
    expect(events[0].ts).toBe("2026-10-11T00:00:00.000Z");
  });
});

describe("gate: retention", () => {
  it("keeps only the newest 20 run directories", async () => {
    const repo = mkRepo({ ".rightkit/update.json": manifest([]) });
    for (let i = 0; i < 23; i++) {
      await runGateCommand({ repo }, { now: () => new Date(Date.UTC(2026, 9, 11, 0, 0, i)) });
    }
    const runs = fs.readdirSync(path.join(repo, ".rightkit/update/gate")).sort();
    expect(runs).toHaveLength(20);
    expect(runs[0]).toBe("20261011T000003Z");
    expect(runs.at(-1)).toBe("20261011T000022Z");
  });
});
