import { describe, expect, it } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileChange, fakeRun, git, gitRepo, manifest, mkRepo, SHA, sha256, stageTool, TOOL_BYTES, validToolSource, writeApplyReport, writeGateReport } from "./support.js";
import { commitMessage, rewriteToolInManifest, runDiscard, runLand } from "../src/land.js";
import type { ApplyChange } from "../src/reports.js";

const CARGO_BEFORE = '[dependencies]\nserde = "1.0.100"\n';
const CARGO_AFTER = '[dependencies]\nserde = "1.0.229"\n';
const LOCK_BEFORE = '[[package]]\nname = "serde"\nversion = "1.0.100"\n';
const LOCK_AFTER = '[[package]]\nname = "serde"\nversion = "1.0.229"\n';

function cargoChange(repo: string): ApplyChange {
  return {
    kind: "cargo",
    name: "serde",
    source: "Cargo.toml",
    before: "1.0.100",
    after: "1.0.229",
    latest: "1.0.229",
    status: "applied",
    files: [fileChange(repo, "Cargo.toml", CARGO_BEFORE), fileChange(repo, "Cargo.lock", LOCK_BEFORE)],
    commands: [["rightkit", "cargo", "update", "-p", "serde", "--precise", "1.0.229"]],
    notes: [],
  };
}

/** A git repo where an apply has "happened": files edited in the tree, reports written, gate passed. */
function appliedRepo(options: { policy?: Record<string, unknown>; extraFiles?: Record<string, string | object>; sources?: unknown[] } = {}) {
  const repo = gitRepo({
    ".rightkit/update.json": manifest(options.sources ?? [{ kind: "cargo", manifest: "Cargo.toml" }], { default: "auto-land", ...(options.policy ?? {}) }),
    "Cargo.toml": CARGO_BEFORE,
    "Cargo.lock": LOCK_BEFORE,
    ...(options.extraFiles ?? {}),
  });
  fs.writeFileSync(path.join(repo, "Cargo.toml"), CARGO_AFTER);
  fs.writeFileSync(path.join(repo, "Cargo.lock"), LOCK_AFTER);
  writeApplyReport(repo, [cargoChange(repo)]);
  writeGateReport(repo);
  return repo;
}

const head = (repo: string) => git(repo, "rev-parse", "HEAD");

describe("land: preconditions", () => {
  it("commits exactly the touched files with the required subject, body and trailer", async () => {
    const repo = appliedRepo();
    const before = head(repo);
    const outcome = await runLand({ repo });

    expect(outcome.errors).toEqual([]);
    expect(outcome.refusals).toEqual([]);
    expect(outcome.exitCode).toBe(0);
    expect(outcome.committed).toBe(true);
    expect(outcome.pushed).toBe(false);
    expect(outcome.commit).toBe(head(repo));
    expect(head(repo)).not.toBe(before);
    expect(git(repo, "log", "-1", "--format=%B")).toBe(
      "deps: 1 updates via right-update\n\n- cargo serde 1.0.100 -> 1.0.229 (Cargo.toml)\n\nCo-Authored-By: right-update <noreply@rightkit.dev>",
    );
    expect(git(repo, "show", "--name-only", "--format=", "HEAD").split("\n").sort()).toEqual(["Cargo.lock", "Cargo.toml"]);
    expect(git(repo, "status", "--porcelain")).toBe("");
    // The report records the landing; a second land refuses.
    const again = await runLand({ repo });
    expect(again.exitCode).toBe(1);
    expect(again.refusals[0]).toMatch(/already landed/);
  });

  it("refuses when the working tree has changes outside the apply-report, and commits nothing", async () => {
    const repo = appliedRepo();
    fs.writeFileSync(path.join(repo, "notes.txt"), "someone else's work\n");
    const before = head(repo);
    const outcome = await runLand({ repo });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.refusals[0]).toMatch(/outside the apply-report: notes\.txt/);
    expect(outcome.committed).toBe(false);
    expect(head(repo)).toBe(before);
    expect(git(repo, "status", "--porcelain")).toContain("notes.txt");
  });

  it("refuses a modified tracked file and a staged file outside the report", async () => {
    const repo = appliedRepo({ extraFiles: { "src.rs": "fn main() {}\n" } });
    fs.appendFileSync(path.join(repo, "src.rs"), "// edit\n");
    git(repo, "add", "src.rs");
    const outcome = await runLand({ repo });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.refusals[0]).toMatch(/src\.rs/);
  });

  it("refuses without an apply-report, without a gate-report, and with a failed gate", async () => {
    const bare = gitRepo({ ".rightkit/update.json": manifest([]) });
    expect((await runLand({ repo: bare })).refusals[0]).toMatch(/no usable apply-report/);

    const noGate = appliedRepo();
    fs.rmSync(path.join(noGate, ".rightkit/update/gate-report.json"));
    expect((await runLand({ repo: noGate })).refusals[0]).toMatch(/no usable gate-report/);

    const failed = appliedRepo();
    writeGateReport(failed, { passed: false });
    const outcome = await runLand({ repo: failed });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.refusals[0]).toMatch(/gate did not pass/);
    expect(git(failed, "log", "--oneline").split("\n")).toHaveLength(1);
  });

  it("refuses a gate-report that was produced for a different apply-report", async () => {
    const repo = appliedRepo();
    writeGateReport(repo, { boundTo: "0".repeat(64) });
    const outcome = await runLand({ repo });
    expect(outcome.refusals[0]).toMatch(/different apply-report/);
  });

  it("refuses when a touched file changed after apply", async () => {
    const repo = appliedRepo();
    fs.appendFileSync(path.join(repo, "Cargo.toml"), "# tweaked after apply\n");
    const outcome = await runLand({ repo });
    expect(outcome.refusals[0]).toMatch(/Cargo\.toml changed since apply/);
  });

  it("refuses off the default branch and with a detached HEAD", async () => {
    const repo = appliedRepo();
    git(repo, "checkout", "-q", "-b", "feature");
    expect((await runLand({ repo })).refusals[0]).toMatch(/current branch feature is not the default branch \(main\)/);
    git(repo, "checkout", "-q", "--detach");
    expect((await runLand({ repo })).refusals[0]).toMatch(/detached/);
  });

  it("refuses when HEAD moved since apply", async () => {
    const repo = appliedRepo();
    fs.writeFileSync(path.join(repo, "other.txt"), "x");
    git(repo, "add", "other.txt");
    git(repo, "commit", "-q", "-m", "someone committed");
    const outcome = await runLand({ repo });
    expect(outcome.refusals[0]).toMatch(/HEAD moved since apply/);
  });

  it("refuses a dry-run report", async () => {
    const repo = appliedRepo();
    writeApplyReport(repo, [], { dryRun: true });
    writeGateReport(repo);
    expect((await runLand({ repo })).refusals[0]).toMatch(/dry-run/);
  });

  it("refuses a repo path that is not a git top level", async () => {
    const repo = appliedRepo();
    const nested = path.join(repo, "nested");
    fs.mkdirSync(path.join(nested, ".rightkit"), { recursive: true });
    fs.copyFileSync(path.join(repo, ".rightkit/update.json"), path.join(nested, ".rightkit/update.json"));
    fs.cpSync(path.join(repo, ".rightkit/update"), path.join(nested, ".rightkit/update"), { recursive: true });
    const outcome = await runLand({ repo: nested });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.refusals[0]).toMatch(/not the top level/);
  });
});

describe("land: held and needs-migration rows", () => {
  it("writes them to held.json, never commits them, and lands the rest", async () => {
    const repo = appliedRepo();
    const report = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update/apply-report.json"), "utf8"));
    report.needsMigration = [{ kind: "cargo", name: "rand", source: "Cargo.toml", before: "0.8.5", latest: "0.9.1", reason: "breaking" }];
    report.held = [{ kind: "cargo", name: "rustls", source: "Cargo.toml", before: "0.23.0", latest: "0.23.5", reason: "policy: held" }];
    fs.writeFileSync(path.join(repo, ".rightkit/update/apply-report.json"), JSON.stringify(report, null, 2));
    writeGateReport(repo);
    const outcome = await runLand({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.held).toBe(2);
    const held = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update/held.json"), "utf8"));
    expect(held.needsMigration[0].name).toBe("rand");
    expect(held.held[0].name).toBe("rustls");
    const body = git(repo, "log", "-1", "--format=%B");
    expect(body).not.toMatch(/rand|rustls/);
    expect(git(repo, "ls-files")).not.toMatch(/held\.json/);
  });

  it("with nothing applied it lands nothing, still records held rows, and exits 0", async () => {
    const repo = gitRepo({ ".rightkit/update.json": manifest([]) });
    writeApplyReport(repo, [], { held: [{ kind: "pnpm", name: "@rightkit/legal", source: "package.json", before: "1.0.0", latest: "1.0.1", reason: "policy: held" }] });
    const before = head(repo);
    const outcome = await runLand({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.committed).toBe(false);
    expect(outcome.notes[0]).toMatch(/nothing to land/);
    expect(head(repo)).toBe(before);
    expect(JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update/held.json"), "utf8")).held[0].name).toBe("@rightkit/legal");
  });
});

describe("land: push policy", () => {
  function withOrigin(repo: string): string {
    const bare = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), "right-update-origin-"));
    git(bare, "init", "-q", "--bare", "-b", "main");
    git(repo, "remote", "add", "origin", bare);
    git(repo, "push", "-q", "-u", "origin", "main");
    return bare;
  }

  it("does not push by default (land: commit)", async () => {
    const repo = appliedRepo();
    const bare = withOrigin(repo);
    const remoteBefore = git(bare, "rev-parse", "main");
    const outcome = await runLand({ repo });
    expect(outcome.committed).toBe(true);
    expect(outcome.pushed).toBe(false);
    expect(git(bare, "rev-parse", "main")).toBe(remoteBefore);
  });

  it("pushes with plain git push when policy.land is push, and says why right-git was not used", async () => {
    const repo = appliedRepo({ policy: { land: "push" }, extraFiles: { ".rightgit.json": { profile: "node" } } });
    const bare = withOrigin(repo);
    const outcome = await runLand({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.pushed).toBe(true);
    expect(outcome.pushVia).toBe("git");
    expect(git(bare, "rev-parse", "main")).toBe(head(repo));
    expect(outcome.notes.join(" ")).toMatch(/no lane-trigger API/);
  });

  it("keeps the commit and exits 2 when the push fails", async () => {
    const repo = appliedRepo({ policy: { land: "push" } });
    const outcome = await runLand({ repo }); // no remote configured
    expect(outcome.exitCode).toBe(2);
    expect(outcome.committed).toBe(true);
    expect(outcome.pushed).toBe(false);
    expect(outcome.errors[0]).toMatch(/git push failed \(the commit [0-9a-f]{40} is local\)/);
  });
});

describe("land: commit failure", () => {
  it("rolls back the index when a hook rejects the commit, leaving the apply state intact", async () => {
    const repo = appliedRepo();
    const hook = path.join(repo, ".git/hooks/pre-commit");
    fs.writeFileSync(hook, "#!/bin/sh\necho rejected >&2\nexit 1\n", { mode: 0o755 });
    const before = head(repo);
    const outcome = await runLand({ repo });
    expect(outcome.exitCode).toBe(2);
    expect(outcome.committed).toBe(false);
    expect(outcome.errors[0]).toMatch(/git commit failed/);
    expect(head(repo)).toBe(before);
    // Edits stay in the working tree but nothing is left staged.
    expect(git(repo, "diff", "--cached", "--name-only")).toBe("");
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_AFTER);
    // After the hook goes away the same land succeeds.
    fs.rmSync(hook);
    expect((await runLand({ repo })).committed).toBe(true);
  });
});

describe("land: tool delivery", () => {
  const WIN_SHA = "c".repeat(64);
  const toolManifest = (pin = "2025.01.12") =>
    manifest([
      validToolSource({
        pin,
        assets: {
          "macos-arm64": { asset: "yt-dlp_macos", checksum: `sha256:${"a".repeat(64)}` },
          "windows-x86_64": { asset: "yt-dlp.exe", checksum: `sha256:${"b".repeat(64)}` },
        },
      }),
    ]);

  function toolRepo(manifestText?: string) {
    const files: Record<string, string | object> = { ".rightkit/update.json": manifestText ?? toolManifest() };
    const repo = gitRepo(files);
    const change = stageTool(repo);
    change.tool!.previousChecksum = `sha256:${"a".repeat(64)}`;
    change.tool!.otherPlatforms[0].previousChecksum = `sha256:${"b".repeat(64)}`;
    change.tool!.otherPlatforms[0].sha256 = WIN_SHA;
    writeApplyReport(repo, [change]);
    writeGateReport(repo);
    return { repo, change };
  }

  const deliveryRoot = () => fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), "right-update-artifacts-"));

  it("moves the verified candidate into the delivery dir, updates pin and checksums, and commits the manifest", async () => {
    const { repo } = toolRepo();
    const root = deliveryRoot();
    const outcome = await runLand({ repo }, { host: { env: { RIGHTKIT_ARTIFACT_DIR: root } } });

    expect(outcome.errors).toEqual([]);
    expect(outcome.exitCode).toBe(0);
    const delivery = path.join(root, "yt-dlp", "2025.10.01");
    expect(outcome.delivered).toEqual([delivery]);
    expect(fs.readFileSync(path.join(delivery, "yt-dlp_macos"), "utf8")).toBe(TOOL_BYTES);
    const receipt = JSON.parse(fs.readFileSync(path.join(delivery, "receipt.json"), "utf8"));
    expect(receipt).toMatchObject({ name: "yt-dlp", version: "2025.10.01", platform: "macos-arm64", sha256: sha256(TOOL_BYTES), deliveredBy: "right-update" });
    // The candidate was moved, not copied.
    expect(fs.existsSync(path.join(repo, ".rightkit/update/candidates/yt-dlp"))).toBe(false);

    const m = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update.json"), "utf8"));
    expect(m.sources[0].pin).toBe("2025.10.01");
    expect(m.sources[0].assets["macos-arm64"].checksum).toBe(`sha256:${sha256(TOOL_BYTES)}`);
    expect(m.sources[0].assets["windows-x86_64"].checksum).toBe(`sha256:${WIN_SHA}`);
    expect(git(repo, "show", "--name-only", "--format=", "HEAD")).toBe(".rightkit/update.json");
    expect(git(repo, "log", "-1", "--format=%s")).toBe("deps: 1 updates via right-update");
    expect(git(repo, "log", "-1", "--format=%b")).toMatch(/- tool yt-dlp 2025\.01\.12 -> 2025\.10\.01 \(sha256:[0-9a-f]{12}, macos-arm64\)/);
    expect(git(repo, "status", "--porcelain")).toBe("");
  });

  it("re-verifies the staged digest and refuses a candidate that changed after apply", async () => {
    const { repo } = toolRepo();
    const root = deliveryRoot();
    fs.writeFileSync(path.join(repo, ".rightkit/update/candidates/yt-dlp/2025.10.01/yt-dlp_macos"), "tampered");
    const before = head(repo);
    const outcome = await runLand({ repo }, { host: { env: { RIGHTKIT_ARTIFACT_DIR: root } } });
    expect(outcome.exitCode).toBe(1);
    expect(outcome.refusals[0]).toMatch(/tool yt-dlp: staged yt-dlp_macos digest .* differs/);
    expect(fs.existsSync(path.join(root, "yt-dlp"))).toBe(false);
    expect(head(repo)).toBe(before);
    expect(JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update.json"), "utf8")).sources[0].pin).toBe("2025.01.12");
  });

  it("refuses when the manifest pin or checksum changed since apply", async () => {
    const { repo } = toolRepo();
    const m = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update.json"), "utf8"));
    m.sources[0].pin = "2025.05.05";
    fs.writeFileSync(path.join(repo, ".rightkit/update.json"), JSON.stringify(m, null, 2));
    git(repo, "commit", "-q", "-am", "someone moved the pin");
    // HEAD also moved, so the base-commit check fires first; either way nothing lands.
    const outcome = await runLand({ repo }, { host: { env: { RIGHTKIT_ARTIFACT_DIR: deliveryRoot() } } });
    expect(outcome.exitCode).toBe(1);
  });

  it("is repeatable after a commit failure: the delivered artifact is recognised and the land completes", async () => {
    const { repo } = toolRepo();
    const root = deliveryRoot();
    const hook = path.join(repo, ".git/hooks/pre-commit");
    fs.writeFileSync(hook, "#!/bin/sh\nexit 1\n", { mode: 0o755 });
    const first = await runLand({ repo }, { host: { env: { RIGHTKIT_ARTIFACT_DIR: root } } });
    expect(first.exitCode).toBe(2);
    // The manifest edit was rolled back; the artifact stays delivered (a versioned directory is harmless).
    expect(JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update.json"), "utf8")).sources[0].pin).toBe("2025.01.12");
    expect(fs.existsSync(path.join(root, "yt-dlp", "2025.10.01", "yt-dlp_macos"))).toBe(true);
    fs.rmSync(hook);
    const second = await runLand({ repo }, { host: { env: { RIGHTKIT_ARTIFACT_DIR: root } } });
    expect(second.exitCode).toBe(0);
    expect(second.notes.join(" ")).toMatch(/already delivered/);
    expect(JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update.json"), "utf8")).sources[0].pin).toBe("2025.10.01");
  });

  it("rewriteToolInManifest keeps hand formatting when the literals are unique", () => {
    const text = `{
  "schemaVersion": 1,
    "policy": { "default": "auto-land" },
  "sources": [
    { "kind": "tool", "name": "yt-dlp", "pin": "2025.01.12",
      "assets": { "macos-arm64": { "asset": "yt-dlp_macos", "checksum": "sha256:${"a".repeat(64)}" } },
      "upstream": { "type": "github-release", "repo": "yt-dlp/yt-dlp" },
      "journey": { "command": ["x"], "report": "{out}/r.json" } }
  ]
}
`;
    const change = stageTool(mkRepo({}));
    change.tool!.previousChecksum = `sha256:${"a".repeat(64)}`;
    change.tool!.otherPlatforms = [];
    const out = rewriteToolInManifest(text, change);
    expect(out).toBe(text.replace('"pin": "2025.01.12"', '"pin": "2025.10.01"').replace("a".repeat(64), sha256(TOOL_BYTES)));
  });
});

describe("commitMessage", () => {
  it("lists one line per change under the fixed subject and trailer", () => {
    const repo = mkRepo({});
    const msg = commitMessage([
      { ...cargoChange(repo) },
      { ...cargoChange(repo), kind: "pnpm", name: "ms", source: "package.json", before: "^2.0.0", after: "^2.1.3" },
    ]);
    expect(msg).toBe(
      "deps: 2 updates via right-update\n\n- cargo serde 1.0.100 -> 1.0.229 (Cargo.toml)\n- pnpm ms ^2.0.0 -> ^2.1.3 (package.json)\n\nCo-Authored-By: right-update <noreply@rightkit.dev>\n",
    );
  });
});

describe("discard", () => {
  it("restores the touched files and drops the reports", async () => {
    const repo = appliedRepo();
    const outcome = await runDiscard({ repo });
    expect(outcome.exitCode).toBe(0);
    expect(outcome.restored.sort()).toEqual(["Cargo.lock", "Cargo.toml"]);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toBe(CARGO_BEFORE);
    expect(git(repo, "status", "--porcelain")).toBe("");
    expect(fs.existsSync(path.join(repo, ".rightkit/update/apply-report.json"))).toBe(false);
  });

  it("refuses to overwrite edits made after apply", async () => {
    const repo = appliedRepo();
    fs.appendFileSync(path.join(repo, "Cargo.toml"), "# mine\n");
    const outcome = await runDiscard({ repo });
    expect(outcome.exitCode).toBe(2);
    expect(outcome.errors[0]).toMatch(/changed after apply/);
    expect(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8")).toMatch(/# mine/);
  });
});

describe("apply-report hardening", () => {
  it("refuses a report that names paths outside the repo, inside .git, or a malformed tool stage", async () => {
    const bad: Array<[string, (c: ApplyChange) => void]> = [
      ["parent traversal", (c) => (c.files[0].path = "../outside.txt")],
      ["absolute path", (c) => (c.files[0].path = "/etc/passwd")],
      ["inside .git", (c) => (c.files[0].path = ".git/hooks/pre-commit")],
    ];
    for (const [label, mutate] of bad) {
      const repo = appliedRepo();
      const report = JSON.parse(fs.readFileSync(path.join(repo, ".rightkit/update/apply-report.json"), "utf8"));
      mutate(report.changes[0]);
      fs.writeFileSync(path.join(repo, ".rightkit/update/apply-report.json"), JSON.stringify(report));
      const outcome = await runLand({ repo });
      expect(outcome.exitCode, label).toBe(1);
      expect(outcome.refusals[0], label).toMatch(/unsafe path/);
    }

    const repo = gitRepo({ ".rightkit/update.json": manifest([]) });
    const change = stageTool(repo);
    change.name = "../../evil";
    writeApplyReport(repo, [change]);
    expect((await runLand({ repo })).refusals[0]).toMatch(/malformed tool stage/);

    const repo2 = gitRepo({ ".rightkit/update.json": manifest([]) });
    const change2 = stageTool(repo2);
    change2.tool!.candidateDir = "src";
    writeApplyReport(repo2, [change2]);
    expect((await runLand({ repo: repo2 })).refusals[0]).toMatch(/malformed tool stage/);
  });
});
