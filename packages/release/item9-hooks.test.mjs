// Item 9 (docs/GATE-CONTRACT.md:32): commit and push run no gate and no local hook checks. `rightkit-gate install-hook` (and its alias
// `right-release hooks install`) writes no git hook and retires the ones an earlier release wrote, each only by its own header or
// marker. Every hook test here runs real git in a scratch repository; the exact text the retired generators wrote is the frozen
// fixture fixtures/retired-hooks.json.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { doorViolation, claudeDoorDecision } from "./door-policy.mjs";
import { localBuildGranted } from "./execution-policy.mjs";
import { retireHooksIn, hookKind, HOOK_MARKER } from "./hook-retirement.mjs";
import { POLICY_BUNDLE_ENTRYPOINTS, computePolicyBundle, scannerSourceFiles } from "./ownership-policy-bundle.mjs";
import { main as stagedMain, scanStaged } from "./ownership-staged.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(here, "..", "..");
const GATE = path.join(here, "bin", "rightkit-gate.mjs");
const RIGHT_RELEASE = path.join(here, "cli", "right-release.mjs");
const FIXTURE = JSON.parse(fs.readFileSync(path.join(here, "fixtures", "retired-hooks.json"), "utf8"));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const slash = (p) => String(p).replaceAll("\\", "/");

// ---- scratch repositories ------------------------------------------------------------------------------------------

const roots = [];
test.after(() => { for (const dir of roots) { try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ } } });
const scratch = (label) => { const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `rk-item9-${label}-`))); roots.push(dir); return dir; };

/** git and node see no user, system or global configuration: a developer's own core.hooksPath cannot change a result. */
function isolatedEnv(home, extra = {}) {
  const globalConfig = path.join(home, "global.gitconfig");
  if (!fs.existsSync(globalConfig)) fs.writeFileSync(globalConfig, "");
  const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(?:GIT_|RK_GATE_RUN$)/i.test(key)));
  return { ...inherited, HOME: home, USERPROFILE: home, GIT_CONFIG_GLOBAL: globalConfig, GIT_CONFIG_NOSYSTEM: "1", GIT_TERMINAL_PROMPT: "0", CI: "", GITHUB_ACTIONS: "", ...extra };
}

function makeRepo(t, label = "repo") {
  const home = scratch(`${label}-home`);
  const dir = scratch(label);
  const env = isolatedEnv(home);
  const git = (...args) => spawnSync("git", args, { cwd: dir, encoding: "utf8", env });
  assert.equal(git("init", "-q", "-b", "main").status, 0);
  for (const [key, value] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"]]) git("config", key, value);
  fs.writeFileSync(path.join(dir, "README.md"), "# app\n");
  git("add", "-A");
  assert.equal(git("commit", "-q", "-m", "base").status, 0);
  const write = (name, body, mode = 0o755) => { const file = path.join(dir, name); fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); try { fs.chmodSync(file, mode); } catch { /* no modes here */ } return file; };
  const read = (name) => fs.readFileSync(path.join(dir, name), "utf8");
  const exists = (name) => fs.existsSync(path.join(dir, name));
  const hooksPath = () => { const r = git("config", "--local", "--get", "core.hooksPath"); return r.status === 0 ? r.stdout.trim() : null; };
  const install = (args = ["install-hook"]) => spawnSync(process.execPath, [GATE, ...args], { cwd: dir, encoding: "utf8", env });
  const log = path.join(dir, "..", `${path.basename(dir)}.hook.log`);
  roots.push(log);
  const logged = () => (fs.existsSync(log) ? fs.readFileSync(log, "utf8") : "");
  return { dir, home, env, git, write, read, exists, hooksPath, install, log, logged };
}
/** A foreign hook: it records that it ran, and a commit message holding REJECT-ME is refused by it when it is a commit-msg hook. */
const foreignHook = (log, label, { rejects = false } = {}) => `#!/bin/sh\necho "${label} $*" >> "${slash(log)}"\n${rejects ? 'if grep -q REJECT-ME "$1"; then exit 1; fi\n' : ""}exit 0\n`;
const forwarder = (name) => FIXTURE.forwarderTemplate.replaceAll("@NAME@", name);
const treeDigest = (dir) => {
  const out = [];
  const walk = (current) => { for (const entry of fs.readdirSync(current, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) { const file = path.join(current, entry.name); if (entry.isDirectory()) walk(file); else out.push(`${slash(path.relative(dir, file))}:${entry.isSymbolicLink() ? "link" : sha256(fs.readFileSync(file))}`); } };
  if (fs.existsSync(dir)) walk(dir);
  return out;
};
const commit = (r, message) => r.git("commit", "--allow-empty", "-q", "-m", message);

// ---- 9.1: the push hook is gone; install-hook writes none ---------------------------------------------------------------

test("9.1 the RightKit tree has no .githooks/pre-push, no scripts/local-gate.mjs and its template commits no git hook", () => {
  for (const file of [".githooks/pre-push", "scripts/local-gate.mjs", "desktop-app-template/.githooks/pre-push"]) assert.equal(fs.existsSync(path.join(REPO, file)), false, `${file} must not exist`);
  assert.equal(fs.existsSync(path.join(REPO, ".githooks")), false, "RightKit commits no .githooks directory");
  assert.equal(fs.existsSync(path.join(REPO, "desktop-app-template", ".githooks")), false);
});

test("9.1 install-hook writes no pre-push, no forwarder and no pre-commit: .git/hooks is byte-identical and no .githooks appears", (t) => {
  const r = makeRepo(t);
  r.write(".git/hooks/commit-msg", foreignHook(r.log, "foreign-commit-msg"));
  r.write(".git/hooks/pre-push", foreignHook(r.log, "foreign-pre-push"));
  r.write(".git/hooks/pre-commit", foreignHook(r.log, "foreign-pre-commit"));
  const before = treeDigest(path.join(r.dir, ".git", "hooks"));
  const run = r.install();
  assert.equal(run.status, 0, run.stderr);
  assert.deepEqual(treeDigest(path.join(r.dir, ".git", "hooks")), before, "foreign hooks are never touched");
  assert.equal(r.exists(".githooks"), false, "no hook directory is created");
  assert.equal(r.hooksPath(), null, "core.hooksPath is not set");
  assert.match(run.stdout, /no git hook is written/);
  assert.doesNotMatch(run.stdout + run.stderr, /pre-push hook|\.githooks\/pre-push|wrote /);
  // The agent side is the one thing it still registers, exactly as before.
  const settings = JSON.parse(r.read(".claude/settings.json"));
  assert.equal(settings.hooks.PreToolUse.length, 1);
  assert.match(settings.hooks.PreToolUse[0].hooks[0].command, /rightkit-agent-hook/);
});

test("9.1 a push with a red tree runs no gate and prints none: before retirement the installed hook stops it, after it the push reaches the remote", (t) => {
  const r = makeRepo(t, "push");
  const remote = scratch("push-remote");
  assert.equal(spawnSync("git", ["init", "-q", "--bare", "-b", "main", remote], { encoding: "utf8", env: r.env }).status, 0);
  r.git("remote", "add", "origin", remote);
  // The hooks an earlier release installed, over a gate that is always red: it would stop every push and records that it ran.
  r.write(".githooks/pre-push", FIXTURE.gateHook);
  r.write("node_modules/.bin/rightkit-gate", `#!/bin/sh\necho "gate ran $*" >> "${slash(r.log)}"\nexit 1\n`);
  r.write(".gitignore", "node_modules/\n", 0o644);
  r.write("red.txt", "a red tree\n", 0o644);
  r.git("config", "core.hooksPath", ".githooks");
  r.git("add", "-A");
  assert.equal(commit(r, "red").status, 0);
  // Without the sanitised PATH the hook could pick up a developer's rightkit-host-gate: keep the stub the only gate in reach.
  const cleanPath = String(process.env.PATH ?? "").split(path.delimiter).filter((p) => !fs.existsSync(path.join(p, "rightkit-host-gate")) && !fs.existsSync(path.join(p, "rightkit-host-gate.exe"))).join(path.delimiter);
  // Windows spells it Path: drop every spelling before setting the one, or which wins is left to the platform.
  const pushEnv = { ...Object.fromEntries(Object.entries(r.env).filter(([key]) => key.toUpperCase() !== "PATH")), PATH: cleanPath };
  const before = spawnSync("git", ["push", "-q", "origin", "main"], { cwd: r.dir, encoding: "utf8", env: pushEnv });
  assert.notEqual(before.status, 0, "the installed hook is live and stops the push");
  assert.equal(spawnSync("git", ["ls-remote", "origin"], { cwd: r.dir, encoding: "utf8", env: r.env }).stdout.trim(), "", "nothing reached the remote");

  assert.equal(r.install().status, 0);
  assert.equal(r.hooksPath(), null);
  const ranBefore = r.logged();
  for (const args of [["push", "origin", "main"], ["push", "--no-verify", "-u", "origin", "main"]]) {
    r.git("update-ref", "-d", "refs/remotes/origin/main");
    spawnSync("git", ["-C", remote, "update-ref", "-d", "refs/heads/main"], { encoding: "utf8", env: r.env });
    const after = spawnSync("git", args, { cwd: r.dir, encoding: "utf8", env: pushEnv });
    assert.equal(after.status, 0, `${args.join(" ")}: ${after.stderr}`);
    assert.doesNotMatch(after.stdout + after.stderr, /rightkit-gate|gate ran|BLOCKED|preflight/i, "no gate output");
    assert.match(spawnSync("git", ["ls-remote", "origin"], { cwd: r.dir, encoding: "utf8", env: r.env }).stdout, /refs\/heads\/main/, "the ref reached the remote");
  }
  assert.equal(r.logged(), ranBefore, "the gate did not run for any push after retirement");
});

// ---- 9.2: retirement matches by header or marker only ---------------------------------------------------------------------

test("9.2 the gate-written pre-push, its forwarders and the ownership step are removed, core.hooksPath is unset, and a foreign .git/hooks/commit-msg still runs", (t) => {
  const r = makeRepo(t);
  r.write(".git/hooks/commit-msg", foreignHook(r.log, "foreign-commit-msg", { rejects: true }));
  r.write(".git/hooks/pre-commit", foreignHook(r.log, "foreign-pre-commit"));
  r.write(".githooks/pre-push", FIXTURE.gateHook);
  r.write(".githooks/commit-msg", forwarder("commit-msg"));
  r.write(".githooks/pre-commit", FIXTURE.ownershipHook);
  r.write(".githooks/pre-commit.rightkit-foreign", forwarder("pre-commit"));
  r.git("config", "core.hooksPath", ".githooks");
  const run = r.install();
  assert.equal(run.status, 0, run.stderr);
  assert.equal(r.exists(".githooks"), false, "an emptied .githooks directory is removed");
  assert.equal(r.hooksPath(), null, "core.hooksPath is unset");
  assert.match(run.stdout, /unset core\.hooksPath \(was \.githooks\)/);
  // The foreign hooks in .git/hooks run again: the commit-msg hook refuses a message, the pre-commit hook runs on a commit.
  const refused = commit(r, "REJECT-ME now");
  assert.notEqual(refused.status, 0, "the foreign commit-msg hook still runs and refuses");
  assert.equal(commit(r, "fine").status, 0);
  const ran = r.logged();
  assert.match(ran, /foreign-commit-msg/);
  assert.match(ran, /foreign-pre-commit/);
});

test("9.2 a foreign pre-push in .githooks and everything else foreign there is left as is, with core.hooksPath unchanged; only the forwarders RightKit wrote go", (t) => {
  const r = makeRepo(t);
  const verify = "#!/bin/sh\nexec node scripts/verify-public-history.mjs\n";
  r.write(".githooks/pre-push", verify);
  r.write(".githooks/commit-msg", forwarder("commit-msg"));
  r.write(".githooks/post-merge", "#!/bin/sh\necho merged\n");
  r.write(".githooks/README", "notes\n", 0o644);
  r.git("config", "core.hooksPath", ".githooks");
  const before = { pre: sha256(r.read(".githooks/pre-push")), post: sha256(r.read(".githooks/post-merge")), readme: sha256(r.read(".githooks/README")) };
  const run = r.install();
  assert.equal(run.status, 0, run.stderr);
  assert.equal(r.read(".githooks/pre-push"), verify);
  assert.deepEqual([sha256(r.read(".githooks/pre-push")), sha256(r.read(".githooks/post-merge")), sha256(r.read(".githooks/README"))], [before.pre, before.post, before.readme]);
  assert.equal(r.exists(".githooks/commit-msg"), false, "the forwarder RightKit wrote is retired");
  assert.deepEqual(fs.readdirSync(path.join(r.dir, ".githooks")).sort(), ["README", "post-merge", "pre-push"]);
  assert.equal(r.hooksPath(), ".githooks", "foreign files remain: core.hooksPath stays");
  assert.match(run.stdout, /core\.hooksPath stays \.githooks: foreign file\(s\) remain there \(README, post-merge, pre-push\)/);
});

test("9.2 a foreign .githooks directory is left byte for byte, and no file in it is read for change", (t) => {
  const r = makeRepo(t);
  r.write(".githooks/pre-push", "#!/bin/sh\n# Mandatory local gate before every push (Adrian 2026-10-22). Never bypass with --no-verify.\nexec node scripts/local-gate.mjs\n");
  r.write(".githooks/pre-commit", "#!/bin/sh\necho mine\n");
  r.write(".githooks/commit-msg", "#!/bin/sh\n# rightkit-gate install-hook: core.hooksPath=.githooks hides .git/hooks, so run this repository's own pre-push hook from there.\nexit 0\n");
  r.git("config", "core.hooksPath", ".githooks");
  const before = treeDigest(path.join(r.dir, ".githooks"));
  const run = r.install();
  assert.equal(run.status, 0, run.stderr);
  assert.deepEqual(treeDigest(path.join(r.dir, ".githooks")), before, "RightKit's own hand-written header, a user's pre-commit and a forwarder naming another hook are all foreign");
  assert.equal(r.hooksPath(), ".githooks");
});

test("9.2 a hook is RightKit's only by its header on line 2 (CRLF checkouts included) or its marker in the first three lines", () => {
  const kind = (name, text, options) => hookKind(name, text, options);
  for (const text of [FIXTURE.gateHook, FIXTURE.gateHookLegacy, FIXTURE.gateHookMultiRoot, FIXTURE.gateHook.replaceAll("\n", "\r\n")]) assert.equal(kind("pre-push", text), "gate");
  assert.equal(kind("commit-msg", FIXTURE.forwarderOfCommitMsg), "forwarder");
  assert.equal(kind("commit-msg", FIXTURE.forwarderOfCommitMsg.replaceAll("\n", "\r\n")), "forwarder");
  assert.equal(kind("pre-commit.rightkit-foreign", forwarder("pre-commit")), "forwarder");
  assert.equal(kind("pre-commit.rightkit-foreign", forwarder("pre-commit.rightkit-foreign")), "forwarder");
  assert.equal(kind("pre-commit", FIXTURE.ownershipHook), "ownership");
  // Not ours: wrong name, an extra line above the header, no shebang, the header as a quotation further down, a marker deep in a foreign file.
  assert.equal(kind("pre-commit", FIXTURE.gateHook), null, "the gate header only counts for pre-push");
  assert.equal(kind("commit-msg", forwarder("pre-push")), null, "a forwarder is ours only for its own name");
  assert.equal(kind("pre-push", `#!/bin/sh\n# mine\n${FIXTURE.gateHook.split("\n").slice(1).join("\n")}`), null);
  assert.equal(kind("pre-push", FIXTURE.gateHook.split("\n").slice(1).join("\n")), null);
  assert.equal(kind("pre-push", `#!/bin/sh\necho x\n# ${"Mandatory local gate before every push (rightkit-gate install-hook). Never bypass with --no-verify."}\n`), null);
  assert.equal(kind("pre-commit", `#!/bin/sh\necho one\necho two\necho three\n# ${HOOK_MARKER}\n`), null);
  assert.equal(kind("pre-commit", FIXTURE.ownershipHook, { scripts: false }), "ownership", "the ownership marker counts in any directory");
  assert.equal(kind("pre-push", FIXTURE.gateHook, { scripts: false }), null, "the gate hook only counts in the directory the gate wrote into");
});

test("9.2 a symbolic link and a directory are never RightKit's, whatever they point at", (t) => {
  if (process.platform === "win32") { t.skip("symbolic links need a privilege on Windows"); return; }
  const r = makeRepo(t);
  r.write(".githooks/real-gate", FIXTURE.gateHook);
  fs.symlinkSync("real-gate", path.join(r.dir, ".githooks", "pre-push"));
  fs.mkdirSync(path.join(r.dir, ".githooks", "commit-msg"));
  r.git("config", "core.hooksPath", ".githooks");
  const run = r.install();
  assert.equal(run.status, 0, run.stderr);
  assert.ok(fs.lstatSync(path.join(r.dir, ".githooks", "pre-push")).isSymbolicLink(), "the link is untouched");
  assert.ok(fs.statSync(path.join(r.dir, ".githooks", "commit-msg")).isDirectory());
  assert.equal(r.hooksPath(), ".githooks");
  // A .githooks that is itself a link is not entered at all.
  const l = makeRepo(t, "linked");
  const target = scratch("linked-target");
  fs.writeFileSync(path.join(target, "pre-push"), FIXTURE.gateHook);
  fs.symlinkSync(target, path.join(l.dir, ".githooks"));
  l.git("config", "core.hooksPath", ".githooks");
  assert.equal(l.install().status, 0);
  assert.equal(fs.readFileSync(path.join(target, "pre-push"), "utf8"), FIXTURE.gateHook, "a hook behind a linked .githooks is untouched");
  assert.equal(l.hooksPath(), ".githooks");
});

test("9.2 every published form of the gate hook is retired: the root form, the 0.6.5 legacy form and the multi-root form", (t) => {
  for (const [label, text] of [["root", FIXTURE.gateHook], ["legacy", FIXTURE.gateHookLegacy], ["multi-root", FIXTURE.gateHookMultiRoot], ["crlf", FIXTURE.gateHook.replaceAll("\n", "\r\n")]]) {
    const r = makeRepo(t, label);
    r.write(".githooks/pre-push", text);
    r.git("config", "core.hooksPath", ".githooks");
    assert.equal(r.install().status, 0, label);
    assert.equal(r.exists(".githooks"), false, label);
    assert.equal(r.hooksPath(), null, label);
  }
});

test("9.2 core.hooksPath is unset only when it is `.githooks`, only in the repository's own configuration, and a second run changes nothing", (t) => {
  // Another directory: the hooks RightKit wrote in .githooks go, the setting is left.
  const husky = makeRepo(t, "husky");
  husky.write(".husky/pre-push", "#!/bin/sh\necho husky\n");
  husky.write(".githooks/pre-push", FIXTURE.gateHook);
  husky.git("config", "core.hooksPath", ".husky");
  assert.equal(husky.install().status, 0);
  assert.equal(husky.hooksPath(), ".husky");
  assert.equal(husky.read(".husky/pre-push"), "#!/bin/sh\necho husky\n");
  // The global configuration is never written, even when it says `.githooks` and the repository does not.
  const globalRepo = makeRepo(t, "global");
  const globalFile = path.join(globalRepo.home, "global.gitconfig");
  fs.writeFileSync(globalFile, "[core]\n\thooksPath = .githooks\n");
  globalRepo.write(".githooks/pre-push", FIXTURE.gateHook);
  assert.equal(globalRepo.install().status, 0);
  assert.equal(fs.readFileSync(globalFile, "utf8"), "[core]\n\thooksPath = .githooks\n");
  // Idempotent.
  const again = makeRepo(t, "again");
  again.write(".githooks/pre-push", FIXTURE.gateHook);
  again.write(".githooks/commit-msg", forwarder("commit-msg"));
  again.write(".githooks/post-merge", "#!/bin/sh\n");
  again.git("config", "core.hooksPath", ".githooks");
  assert.equal(again.install().status, 0);
  const snapshot = [treeDigest(path.join(again.dir, ".githooks")), again.hooksPath(), again.read(".claude/settings.json")];
  const second = again.install();
  assert.equal(second.status, 0);
  assert.deepEqual([treeDigest(path.join(again.dir, ".githooks")), again.hooksPath(), again.read(".claude/settings.json")], snapshot);
  assert.match(second.stdout, /agent hook already present/);
});

test("9.2 retireHooksIn: nothing in a directory it cannot list, and an unreadable name is foreign", () => {
  assert.deepEqual(retireHooksIn("/no/such/dir", { readdir: () => { throw new Error("ENOENT"); } }), { found: false, removed: [], restored: [], remaining: [], removedDir: false });
  const seen = [];
  const ops = { readdir: () => ["pre-push", "odd"], readFile: (file) => { if (file.endsWith("odd")) throw new Error("EACCES"); return FIXTURE.gateHook; }, lstat: () => ({ isFile: () => true, isSymbolicLink: () => false }), unlink: (file) => seen.push(path.basename(file)), rename: () => assert.fail("no rename"), rmdir: () => assert.fail("not empty") };
  const result = retireHooksIn("/d", ops);
  assert.deepEqual(result.removed, ["pre-push"]);
  assert.deepEqual(result.remaining, ["odd"]);
  assert.deepEqual(seen, ["pre-push"]);
});

// ---- 9.3: the advisory ownership pre-commit hook is retired ---------------------------------------------------------------------

const BLOCK_LINE = "BLOCK oauth.webview.auth-url-load (rightkit-oauth) src/a.rs:1";
/**
 * The ownership step as an earlier release installed it, over a stand-in for the pinned right-release that blocks `ownership --staged` and
 * records that it ran (the real command needs this host's ownership trust roots, which an elevated Windows session refuses: the manual
 * command is proven in process below), and a staged file.
 */
function ownershipRepo(t, { where, foreign }) {
  const r = makeRepo(t, `own-${where}`);
  r.write("node_modules/@rightkit/release/cli/right-release.mjs", `import { appendFileSync } from "node:fs";\nappendFileSync(${JSON.stringify(slash(r.log))}, "ownership-gate ran " + process.argv.slice(2).join(" ") + "\\n");\nconsole.log(${JSON.stringify(BLOCK_LINE)});\nprocess.exit(1);\n`, 0o644);
  const dir = where === "githooks" ? ".githooks" : ".git/hooks";
  if (where === "githooks") r.git("config", "core.hooksPath", ".githooks");
  r.write(`${dir}/pre-commit`, FIXTURE.ownershipHook);
  if (foreign) r.write(`${dir}/pre-commit.rightkit-foreign`, foreignHook(r.log, "foreign-pre-commit"));
  r.write("src/a.rs", "fn main() {}\n", 0o644);
  r.git("add", "src/a.rs");
  return { ...r, place: dir };
}

for (const where of ["git-hooks", "githooks"]) {
  test(`9.3 the ownership step in ${where === "githooks" ? ".githooks" : ".git/hooks"} is removed and the foreign pre-commit it displaced is back: a staged block-tier finding commits and only the foreign hook runs`, (t) => {
    const r = ownershipRepo(t, { where, foreign: true });
    const blocked = commit(r, "blocked");
    assert.notEqual(blocked.status, 0, "while installed, the ownership step stops this commit");
    assert.match(blocked.stdout + blocked.stderr, /BLOCK oauth\.webview\.auth-url-load/);
    assert.match(r.logged(), /ownership-gate ran ownership --staged/);
    const ranBefore = r.logged();
    const run = r.install();
    assert.equal(run.status, 0, run.stderr);
    assert.match(run.stdout, /retired .*pre-commit/);
    assert.match(run.stdout, /restored .*pre-commit from pre-commit\.rightkit-foreign/);
    const place = r.place;
    assert.equal(r.read(`${place}/pre-commit`), foreignHook(r.log, "foreign-pre-commit"), "the foreign hook is back, byte for byte");
    assert.equal(r.exists(`${place}/pre-commit.rightkit-foreign`), false);
    const done = commit(r, "the staged finding commits");
    assert.equal(done.status, 0, done.stdout + done.stderr);
    assert.doesNotMatch(done.stdout + done.stderr, /BLOCK|ownership/i, "no ownership check ran");
    assert.equal(r.logged().replace(ranBefore, "").includes("ownership-gate ran"), false, "the ownership command was not run again");
    assert.match(r.logged(), /foreign-pre-commit/, "the foreign pre-commit still runs");
    // In .git/hooks nothing else is there. In .githooks the restored foreign hook is a foreign file in the directory core.hooksPath names: both stay.
    if (where === "githooks") { assert.equal(r.hooksPath(), ".githooks"); assert.deepEqual(fs.readdirSync(path.join(r.dir, ".githooks")), ["pre-commit"]); }
  });
}

test("9.3 the ownership step with no foreign hook displaced is simply removed, and a foreign pre-commit (even one naming the marker late) is untouched", (t) => {
  const r = ownershipRepo(t, { where: "git-hooks", foreign: false });
  assert.equal(r.install().status, 0);
  assert.equal(r.exists(".git/hooks/pre-commit"), false);
  const done = commit(r, "no hook left");
  assert.equal(done.status, 0, done.stdout + done.stderr);
  const own = makeRepo(t, "own-foreign");
  const mine = `#!/bin/sh\necho one\necho two\necho three\n# ${HOOK_MARKER}\nexit 0\n`;
  own.write(".git/hooks/pre-commit", mine);
  own.write(".git/hooks/pre-commit.rightkit-foreign", "#!/bin/sh\necho user kept this\n");
  assert.equal(own.install().status, 0);
  assert.equal(own.read(".git/hooks/pre-commit"), mine);
  assert.equal(own.read(".git/hooks/pre-commit.rightkit-foreign"), "#!/bin/sh\necho user kept this\n", "nothing is moved unless the ownership step displaced it");
});

test("9.3 `right-release hooks install` is the same retirement, and the direct ownership-hooks entry removes the step too", (t) => {
  const r = ownershipRepo(t, { where: "git-hooks", foreign: true });
  const viaCli = spawnSync(process.execPath, [RIGHT_RELEASE, "hooks", "install"], { cwd: r.dir, encoding: "utf8", env: r.env });
  assert.equal(viaCli.status, 0, viaCli.stderr);
  assert.equal(r.read(".git/hooks/pre-commit"), foreignHook(r.log, "foreign-pre-commit"));
  const d = ownershipRepo(t, { where: "git-hooks", foreign: true });
  const direct = spawnSync(process.execPath, [path.join(here, "ownership-hooks.mjs"), "install", "--app", d.dir], { encoding: "utf8", env: d.env });
  assert.equal(direct.status, 0, direct.stderr);
  assert.match(direct.stdout, /retired/);
  assert.equal(d.read(".git/hooks/pre-commit"), foreignHook(d.log, "foreign-pre-commit"));
  assert.equal(spawnSync(process.execPath, [path.join(here, "ownership-hooks.mjs"), "bogus"], { encoding: "utf8" }).status, 2);
});

test("9.3 `right-release ownership --staged` stays as a manual command: it blocks a staged finding by hand, and git commit has no hook to run it", (t) => {
  const r = ownershipRepo(t, { where: "git-hooks", foreign: false });
  assert.equal(r.install().status, 0);
  // A block-tier finding in the index, the scanner of the command with an injected catalog (the real one needs this host's trust roots).
  r.write("src/b.rs", "fn main() { fakeaudio::open(); }\n", 0o644);
  r.git("add", "src/b.rs");
  const manifest = { schema: 1, version: "t", internalOwners: [], capabilities: [{ package: "rightkit-audio", capability: "audio", fingerprints: [{ id: "audio.src.fake", type: "source", pattern: "fakeaudio::", codeOnly: true, extensions: [".rs"], tier: "block", usageContext: "test", tierReason: "test", policy: "test" }] }] };
  const lines = [];
  const code = stagedMain(["--app", r.dir], { print: (line) => lines.push(line), scanner: (root) => scanStaged(root, { manifest, trust: { trust: null, enrolled: false, pending: true } }) });
  assert.equal(code, 1, lines.join("\n"));
  assert.match(lines.join("\n"), /BLOCK audio\.src\.fake/);
  // The command is still wired into right-release, and nothing runs it on commit.
  const help = spawnSync(process.execPath, [RIGHT_RELEASE, "--help"], { cwd: r.dir, encoding: "utf8", env: r.env });
  assert.match(help.stdout, /ownership --staged \[--app <dir>\] \[--json\]/);
  const done = commit(r, "commits anyway");
  assert.equal(done.status, 0, done.stdout + done.stderr);
  assert.equal(r.logged().includes("ownership-gate ran"), false);
});

test("9.3 the protected policy bundle still computes: every entry point exists, the retired hook step is not one, and computing it does not throw", () => {
  for (const entry of POLICY_BUNDLE_ENTRYPOINTS) assert.ok(fs.existsSync(path.join(here, entry)), `${entry} is listed in the protected bundle and must exist`);
  assert.ok(!POLICY_BUNDLE_ENTRYPOINTS.includes("ownership-hooks.mjs"), "the retired hook step carries no enforcement decision");
  const closure = scannerSourceFiles().map((f) => f.path);
  assert.ok(closure.includes("ownership-staged.mjs") && !closure.includes("ownership-hooks.mjs") && !closure.includes("hook-retirement.mjs"));
  const bundle = computePolicyBundle({ scannerVersion: "1" });
  assert.match(bundle.digest, /^[a-f0-9]{64}$/);
  assert.ok(bundle.files.length > 5);
});

// ---- 9.5: the local-build record carries only the build grant ---------------------------------------------------------------------

test("9.5 .rightkit-local-build.json: the build/test/publish and upload grant unchanged; the pre-push authorization is withdrawn; the grant still admits this repository and no other", (t) => {
  const file = path.join(REPO, ".rightkit-local-build.json");
  const grant = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.deepEqual(Object.keys(grant), ["repository", "allow", "approvedBy", "approved", "reason"]);
  assert.equal(grant.repository, "bogusyogi/rightkit");
  assert.equal(grant.allow, "build-test-publish");
  assert.equal(grant.approvedBy, "Adrian D'souza");
  assert.equal(grant.approved, "2026-10-14", "the grant date of AGENTS.md Commands and execution-policy.mjs, not the date of the disputed pre-push wording");
  const withdrawal = "the 2026-10-22 pre-push authorization (#911) is withdrawn: disputed by Adrian 2026-10-28, GATE-CONTRACT.md:32";
  assert.ok(grant.reason.includes(withdrawal), "the record names the withdrawal");
  const claim = grant.reason.replace(withdrawal, "");
  assert.doesNotMatch(claim, /pre-push|before push|2026-10-22|#911|static class A|focused scripts\/ci|generated-doc|mandatory/i, "no pre-push claim survives");
  assert.match(claim, /local build/i);
  assert.match(claim, /test/i);
  assert.match(claim, /publish/i);
  assert.match(claim, /green sealed qualification artifacts may use direct upload/, "the upload permission is unchanged (signed D9-2 withdraws only the pre-push claim)");
  // The consumers read only `repository` and `allow`: this gate's own reader (mirrors rustright's local_build.rs, whose fixture
  // a_local_build_grant_admits_nothing_past_a_red_check in rustright/crates/rustright-broker/src/job_service/tests.rs covers the broker side).
  const clone = (name, origin, body) => {
    const dir = scratch(name);
    fs.mkdirSync(path.join(dir, ".git"));
    fs.writeFileSync(path.join(dir, ".git", "config"), `[remote "origin"]\n\turl = ${origin}\n`);
    fs.writeFileSync(path.join(dir, ".rightkit-local-build.json"), body);
    return dir;
  };
  assert.equal(localBuildGranted(clone("granted", "https://github.com/bogusyogi/rightkit.git", fs.readFileSync(file, "utf8"))), true, "the committed bytes still grant");
  assert.equal(localBuildGranted(clone("other-origin", "https://github.com/bogusyogi/pulse.git", fs.readFileSync(file, "utf8"))), false, "a different repository is granted nothing");
  assert.equal(localBuildGranted(clone("other-name", "https://github.com/bogusyogi/rightkit.git", JSON.stringify({ ...grant, repository: "bogusyogi/pulse" }))), false, "a grant naming another repository grants nothing");
  assert.equal(localBuildGranted(clone("other-allow", "https://github.com/bogusyogi/rightkit.git", JSON.stringify({ ...grant, allow: "build" }))), false);
  // The wording is irrelevant to the grant: no reader depends on `reason` or `approved`.
  assert.equal(localBuildGranted(clone("reworded", "https://github.com/bogusyogi/rightkit.git", JSON.stringify({ ...grant, reason: "x", approved: "1999-01-01" }))), true);
  // The broker's reader, when the sibling checkout is here (the CI export has none): it reads `allow` and `repository` and nothing else.
  const brokerReader = path.resolve(REPO, "..", "rustright", "crates", "rustright-policy", "src", "local_build.rs");
  if (fs.existsSync(brokerReader)) {
    const source = fs.readFileSync(brokerReader, "utf8");
    assert.doesNotMatch(source, /get\("(?:reason|approved|approvedBy)"\)/, "rustright's local_build.rs does not read the wording or the date");
    assert.match(source, /get\("allow"\)/);
    assert.match(source, /get\("repository"\)/);
  } else t.diagnostic("rustright is not checked out beside this tree: the broker reader was not read");
});

// ---- 9.7: commit and push are allowed by the one-door agent hook -------------------------------------------------------------------

test("9.7 the agent door allows git commit and git push, with -m, --no-verify and -u origin main", () => {
  const allowed = [
    'git commit -m "message"', 'git commit --no-verify -m "message"', 'git commit -am "x"', "git commit --amend --no-edit", 'git commit -m "cargo build fixed"',
    "git push", "git push origin main", "git push -u origin main", "git push --no-verify", "git push --no-verify -u origin main", "git push --force-with-lease origin main",
    'cd /tmp/repo && git add -A && git commit -m "x" && git push -u origin main', "git -C packages/release push -u origin main", 'sh -c "git push --no-verify -u origin main"',
  ];
  for (const command of allowed) {
    assert.equal(doorViolation(command), null, command);
    assert.deepEqual(claudeDoorDecision({ tool_name: "Bash", tool_input: { command } }), {}, command);
  }
  // The door itself is unchanged by item 9: a build outside it is still denied.
  assert.equal(claudeDoorDecision({ tool_name: "Bash", tool_input: { command: "cargo build" } }).hookSpecificOutput.permissionDecision, "deny");
});

// ---- 9.8: the pre-push plan mode and local-gate.mjs are gone -------------------------------------------------------------------------

test("9.8 `rightkit-gate --pre-push` exits 2 as an unknown option, with the usage, and runs nothing", (t) => {
  const r = makeRepo(t);
  for (const args of [["--pre-push"], ["--pre-push", "--plan"], ["--plan", "--pre-push", "--base", "origin/main"]]) {
    const run = spawnSync(process.execPath, [GATE, ...args], { cwd: r.dir, encoding: "utf8", env: r.env });
    assert.equal(run.status, 2, args.join(" "));
    assert.match(run.stderr, /unknown option "--pre-push"/);
    assert.match(run.stderr, /usage: rightkit-gate/);
    assert.doesNotMatch(run.stdout, /plan:|scan\(s\)/);
  }
  const help = spawnSync(process.execPath, [GATE, "--help"], { encoding: "utf8" });
  assert.equal(help.status, 0);
  assert.doesNotMatch(help.stdout, /--pre-push/, "the usage no longer offers it");
  assert.match(help.stdout, /rightkit-gate \[--base <ref>\] \[--plan\] \[--no-cache\]/, "the full gate stays");
});

test("9.8 nothing in the repository calls local-gate.mjs, prePushPlan or the NO_DEFER deferral, and the gate source no longer carries the hook generators", () => {
  const source = fs.readFileSync(GATE, "utf8");
  assert.doesNotMatch(source, /prePushPlan|hookText|trampolineText|hookGatePlan|RIGHTKIT_GATE_NO_DEFER/);
  const listed = spawnSync("git", ["ls-files", "-z"], { cwd: REPO, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  const files = listed.status === 0 ? listed.stdout.split("\0").filter(Boolean) : null;
  if (!files) return; // an export without .git: the source-level assertions above and below still hold
  const callers = [];
  for (const file of files) {
    if (!/\.(?:mjs|js|cjs|json|ya?ml|sh|toml)$/.test(file) || file.startsWith("docs/") || file === "packages/release/item9-hooks.test.mjs") continue;
    let text; try { text = fs.readFileSync(path.join(REPO, file), "utf8"); } catch { continue; }
    if (/local-gate\.mjs|prePushPlan|RIGHTKIT_GATE_NO_DEFER/.test(text)) callers.push(file);
  }
  assert.deepEqual(callers, [], "no repository file calls local-gate.mjs, prePushPlan or the NO_DEFER deferral");
});

// ---- 9.9: the comments no longer name the pre-push hook ------------------------------------------------------------------------------

test("9.9 scripts/preflight.mjs's header and ci.yml's preflight comment do not name a pre-push hook, and say CI runs preflight first on every push", () => {
  const preflight = fs.readFileSync(path.join(REPO, "scripts", "preflight.mjs"), "utf8").split("\n").slice(0, 8).join("\n");
  assert.doesNotMatch(preflight, /pre-push|\.githooks/);
  assert.match(preflight, /ci\.yml runs it as the plan job's\n\/\/ first step on every push/);
  assert.match(preflight, /never a push precondition/);
  const ci = fs.readFileSync(path.join(REPO, ".github", "workflows", "ci.yml"), "utf8");
  const at = ci.indexOf("node scripts/preflight.mjs");
  assert.ok(at > 0);
  assert.doesNotMatch(ci.slice(at - 600, at), /pre-push|hook's first step|skipped the hook/);
  assert.doesNotMatch(ci, /pre-push/);
});
