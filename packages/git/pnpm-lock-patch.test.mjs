import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { renderTemplate } from "./render.mjs";
import { buildRenderContext, lockfilePatchStep } from "./manifest.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const templatesDir = path.join(packageRoot, "templates");

const TEMPLATES = { node: "ci.yml", rust: "profiles/rust-hybrid/ci.yml" };
const INSTALL_LINE = "      - run: pnpm install --frozen-lockfile";
const STEP_NAME = "      - name: Print pnpm-lock.yaml patch when the lockfile is stale";

const NODE_BASE = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: ["ci"],
  matrix: { os: ["ubuntu-latest"], node: ["24"] },
  templateVersion: "1.0.0",
};
const RUST_BASE = {
  ...NODE_BASE,
  profile: "rust-hybrid",
  gate: { path: "scripts/gate.sh" },
  matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
  ciAllOs: true,
};
const manifestFor = (profile, packageManager) => ({ ...(profile === "rust" ? RUST_BASE : NODE_BASE), packageManager });

function render(profile, manifest, override = {}) {
  const context = { ...buildRenderContext(structuredClone(manifest), "/repos/widgets"), ...override };
  return renderTemplate(fs.readFileSync(path.join(templatesDir, TEMPLATES[profile]), "utf8"), context);
}

// The step's run body with the YAML block indent removed, as GitHub Actions runs it.
function runBody(step) {
  const head = "        run: |\n";
  const body = step.slice(step.indexOf(head) + head.length);
  return body.split("\n").map((line) => line.slice(10)).join("\n");
}

test("pnpm: both ci templates print the patch step directly after the frozen install", () => {
  for (const profile of ["node", "rust"]) {
    const out = render(profile, manifestFor(profile, "pnpm@11.24.0"));
    assert.equal(out.split(STEP_NAME).length - 1, 1, `${profile}: exactly one patch step`);
    assert.equal(out.split(INSTALL_LINE).length - 1, 1, `${profile}: exactly one frozen install`);
    assert.ok(out.includes(`${INSTALL_LINE}\n${STEP_NAME}`), `${profile}: step follows the install`);
    assert.match(out, /        if: failure\(\)\n/);
  }
});

test("pnpm: the patch step is the only difference from a render without it", () => {
  for (const profile of ["node", "rust"]) {
    const manifest = manifestFor(profile, "pnpm@11.24.0");
    const step = lockfilePatchStep(manifest);
    const withStep = render(profile, manifest);
    const withoutStep = render(profile, manifest, { PNPM_LOCK_PATCH_STEP: "" });
    assert.equal(withoutStep.split(INSTALL_LINE).length - 1, 1, `${profile}: install line is unique`);
    assert.equal(withStep, withoutStep.replace(INSTALL_LINE, INSTALL_LINE + step), `${profile}: only the new step differs`);
  }
});

test("npm and yarn: no patch step and no pnpm lockfile reference in either ci template", () => {
  for (const pm of ["npm@10.9.2", "yarn@4.5.0"]) {
    for (const profile of ["node", "rust"]) {
      const manifest = manifestFor(profile, pm);
      assert.equal(lockfilePatchStep(manifest), "", `${pm} ${profile}: no step generated`);
      const out = render(profile, manifest);
      assert.doesNotMatch(out, /lockfile-only|pnpm-lock\.yaml/, `${pm} ${profile}: no pnpm lockfile text`);
    }
  }
});

test("patch step runs only after a failed install, never masks it, and prints the lockfile diff", () => {
  const step = lockfilePatchStep(manifestFor("node", "pnpm@11.24.0"));
  assert.match(step, /^\n {6}- name: /);
  assert.match(step, /\n {8}if: failure\(\)\n/);
  const body = runBody(step);
  assert.match(body, /pnpm install --lockfile-only/);
  assert.match(body, /git diff -- pnpm-lock\.yaml/);
  assert.doesNotMatch(body, /\bexit\b/, "the step must not choose the job result");
});

test("patch step behaves on a real stale, fresh and failing lockfile", (t) => {
  if (process.platform === "win32") return t.skip("POSIX shell behaviour");
  const git = spawnSync("git", ["--version"], { encoding: "utf8" });
  if (git.error) return t.skip("git not installed");
  const body = runBody(lockfilePatchStep(manifestFor("node", "pnpm@11.24.0")));
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-pnpm-lock-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const repo = path.join(root, "repo");
  const bin = path.join(root, "bin");
  fs.mkdirSync(repo);
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(root, "step.sh"), `${body}\n`);
  // Stand-in for pnpm: `install --lockfile-only` rewrites the lockfile to $FAKE_PNPM_LOCK and exits $FAKE_PNPM_STATUS.
  fs.writeFileSync(path.join(bin, "pnpm"), [
    "#!/bin/sh",
    'if [ "$1" = "install" ] && [ "$2" = "--lockfile-only" ]; then',
    '  printf \'%s\\n\' "$FAKE_PNPM_LOCK" > pnpm-lock.yaml',
    '  exit "${FAKE_PNPM_STATUS:-0}"',
    "fi",
    'echo "unexpected pnpm arguments: $*" >&2',
    "exit 2",
    "",
  ].join("\n"));
  fs.chmodSync(path.join(bin, "pnpm"), 0o755);
  const gitRun = (args) => spawnSync("git", args, { cwd: repo, encoding: "utf8" });
  gitRun(["init", "-q"]);
  fs.writeFileSync(path.join(repo, "pnpm-lock.yaml"), "lockfileVersion: v1\n");
  gitRun(["add", "pnpm-lock.yaml"]);
  gitRun(["-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "-qm", "init"]);

  const runStep = (env) => spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", path.join(root, "step.sh")], {
    cwd: repo,
    encoding: "utf8",
    env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`, ...env },
  });

  const stale = runStep({ FAKE_PNPM_LOCK: "lockfileVersion: v2", FAKE_PNPM_STATUS: "0" });
  assert.equal(stale.status, 0, stale.stderr);
  assert.match(stale.stdout, /::group::pnpm-lock\.yaml patch/);
  assert.match(stale.stdout, /^-lockfileVersion: v1$/m);
  assert.match(stale.stdout, /^\+lockfileVersion: v2$/m);
  assert.match(stale.stdout, /::error file=pnpm-lock\.yaml::/);
  assert.doesNotMatch(stale.stdout, /::notice::/);

  gitRun(["checkout", "--", "pnpm-lock.yaml"]);
  const fresh = runStep({ FAKE_PNPM_LOCK: "lockfileVersion: v1", FAKE_PNPM_STATUS: "0" });
  assert.equal(fresh.status, 0, fresh.stderr);
  assert.match(fresh.stdout, /::notice::pnpm-lock\.yaml already matches package\.json/);
  assert.doesNotMatch(fresh.stdout, /::group::/);

  const failing = runStep({ FAKE_PNPM_LOCK: "lockfileVersion: v1", FAKE_PNPM_STATUS: "1" });
  assert.equal(failing.status, 0, "the step itself succeeds; the failed install already failed the job");
  assert.match(failing.stdout, /::error::pnpm install --lockfile-only failed/);
  assert.doesNotMatch(failing.stdout, /::group::/);
});

test("rendered pnpm ci workflows pass actionlint with the patch step", (t) => {
  const probe = spawnSync("actionlint", ["-version"], { encoding: "utf8" });
  if (probe.error) return t.skip("actionlint not installed");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-pnpm-patch-actionlint-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  fs.mkdirSync(path.join(dir, ".github", "workflows"), { recursive: true });
  fs.writeFileSync(path.join(dir, ".github", "actionlint.yaml"), "self-hosted-runner:\n  labels:\n    - xcode-*\n");
  const files = [
    ["node-pnpm.yml", render("node", manifestFor("node", "pnpm@11.24.0"))],
    ["rust-pnpm.yml", render("rust", manifestFor("rust", "pnpm@11.24.0"))],
  ].map(([name, text]) => {
    const file = path.join(dir, ".github", "workflows", name);
    fs.writeFileSync(file, text);
    return file;
  });
  const result = spawnSync("actionlint", ["-shellcheck=", "-pyflakes=", "-config-file", path.join(dir, ".github", "actionlint.yaml"), ...files], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stdout + result.stderr);
});
