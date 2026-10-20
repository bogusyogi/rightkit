#!/usr/bin/env node
// Targeted AppArmor userns permission for the verified CI Chrome install only.
// Chromium documents this profile route in docs/security/apparmor-userns-restrictions.md.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { pinFor, shellPinFor } from "./provision-chrome.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const REPOSITORY = "bogusyogi/rightkit";
export const PROFILE_PATH = "/etc/apparmor.d/rightkit-ci-chrome";
const WORKFLOW_ROOTS = new Map([
  ["publish.yml", "chrome-for-testing"],
  ["chrome-native-diagnostic.yml", "rightkit-chrome-managed"],
]);
const need = (ok, message) => { if (!ok) throw new Error(`Chrome Linux sandbox refused: ${message}`); };

export function validateContext({ env, event, platform = process.platform, arch = process.arch }) {
  need(platform === "linux" && arch === "x64" && env.RUNNER_OS === "Linux" && env.RUNNER_ARCH === "X64", "Linux x64 runner required");
  need(env.GITHUB_ACTIONS === "true" && env.RUNNER_ENVIRONMENT === "github-hosted", "GitHub-hosted Actions required");
  need(env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_REF === "refs/heads/main" && env.GITHUB_EVENT_NAME === "workflow_dispatch", "public repository main workflow_dispatch required");
  const workflow = [...WORKFLOW_ROOTS.keys()].find((name) => env.GITHUB_WORKFLOW_REF === `${REPOSITORY}/.github/workflows/${name}@refs/heads/main`);
  need(workflow, "unapproved workflow identity");
  need(/^[0-9a-f]{40}$/.test(env.GITHUB_SHA ?? ""), "source SHA required");
  need(event?.repository?.full_name === REPOSITORY && event.repository.private === false && event.repository.default_branch === "main", "public repository event identity required");
  need(typeof env.RUNNER_TEMP === "string" && path.posix.isAbsolute(env.RUNNER_TEMP) && path.posix.normalize(env.RUNNER_TEMP) === env.RUNNER_TEMP && env.RUNNER_TEMP !== "/", "canonical runner temp required");
  need(!/[\x00-\x1f\x7f]/.test(env.RUNNER_TEMP), "control characters in runner temp");
  const root = path.posix.join(env.RUNNER_TEMP, WORKFLOW_ROOTS.get(workflow));
  const executable = path.posix.join(root, "current/chrome-linux64/chrome");
  need(env.RIGHTKIT_CHROME_FOR_TESTING_ROOT === root && env.RIGHTKIT_CHROME_FOR_TESTING === executable, "exact managed Chrome path required");
  return { root, executable, marker: path.posix.join(root, "current/install.json") };
}

export function validateInstall(pins, recorded) {
  const entry = pinFor(pins, "linux64");
  need(entry.sha256 !== "", "nonempty pinned Linux archive hash required");
  const shell = shellPinFor(pins, "linux64");
  const expected = { version: pins.version, platform: "linux64", files: [{ kind: "chrome", ...entry }, ...(shell ? [{ kind: "headless_shell", ...shell }] : [])] };
  need(JSON.stringify(recorded) === JSON.stringify(expected), "install.json does not match provisioner version, URL & archive hash");
}

export function profileText(executable) {
  need(typeof executable === "string" && path.posix.isAbsolute(executable) && !/[\x00-\x1f\x7f]/.test(executable), "absolute profile attachment without control characters required");
  // Quoting protects whitespace; escape AARE metacharacters so attachment never expands.
  const escaped = executable.replace(/[\\"*?\[\]{}]/g, "\\$&");
  return `# Managed by RightKit CI; grants userns only to pinned Chrome's exact path.\nprofile rightkit-ci-chrome "${escaped}" flags=(unconfined) {\n  userns,\n}\n`;
}

export function profileCommands(source) {
  return [
    ["/usr/bin/sudo", ["-n", "/usr/bin/install", "-T", "-o", "root", "-g", "root", "-m", "0644", "--", source, PROFILE_PATH]],
    ["/usr/bin/sudo", ["-n", "/usr/sbin/apparmor_parser", "-r", PROFILE_PATH]],
  ];
}

export function provisionLinuxSandbox({ env = process.env, platform = process.platform, arch = process.arch, run = spawnSync, log = console.log } = {}) {
  // No filesystem write or privileged operation precedes identity & pin validation.
  const event = JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, "utf8"));
  const context = validateContext({ env, event, platform, arch });
  const pins = JSON.parse(fs.readFileSync(path.join(ROOT, "ci/chrome-for-testing.json"), "utf8"));
  validateInstall(pins, JSON.parse(fs.readFileSync(context.marker, "utf8")));
  for (const target of [env.RUNNER_TEMP, context.root, path.posix.join(context.root, "current"), path.posix.dirname(context.executable), context.executable, context.marker]) {
    need(fs.realpathSync(target) === target, `symlink or redirected managed path: ${target}`);
  }
  need(fs.statSync(context.executable).isFile() && (fs.statSync(context.executable).mode & 0o111) !== 0, "managed Chrome must be executable file");
  const text = profileText(context.executable);
  const temp = fs.mkdtempSync(path.posix.join(env.RUNNER_TEMP, "rightkit-chrome-apparmor-"));
  try {
    const source = path.posix.join(temp, "profile");
    fs.writeFileSync(source, text, { flag: "wx", mode: 0o600 });
    const commands = profileCommands(source);
    for (let index = 0; index < commands.length; index++) {
      const [command, args] = commands[index];
      const result = run(command, args, { stdio: "inherit", shell: false, timeout: 30_000 });
      need(!result.error && result.status === 0, `profile ${index === 0 ? "installation" : "load"} failed (exit ${result.status})`);
      if (index === 0) {
        const installed = fs.lstatSync(PROFILE_PATH);
        need(installed.isFile() && installed.uid === 0 && installed.gid === 0 && (installed.mode & 0o777) === 0o644 && fs.readFileSync(PROFILE_PATH, "utf8") === text, "installed profile ownership, mode or content mismatch");
      }
    }
    log(`Chrome Linux sandbox profile loaded: ${context.executable}`);
    return { executable: context.executable, profile: PROFILE_PATH };
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    need(process.argv.length === 2, "no arguments accepted");
    provisionLinuxSandbox();
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
