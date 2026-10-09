import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { appleScriptString, buildDesktopWrapper, buildMacInvocation, buildMacShellCommand, buildSchtasksCreate, decodeLog, delegateToDesktop, DESKTOP_DELEGATED_ENV, needsDesktopDelegation, shellQuote } from "./ownership-key-elevation.mjs";
import { __testOnly } from "./ownership-key-command.mjs";

const NODE = "/usr/local/bin/node";
const ARGS = ["/r/ownership-key-authority.mjs", "--trust-script", "/r/ownership-key-trust.mjs", "--request", "/t/req.json", "--sha256", "a".repeat(64)];

test("macOS with a terminal keeps the unchanged sudo command", () => {
  const i = buildMacInvocation({ tty: true, nodeExe: NODE, args: ARGS, env: { PATH: "/bin" } });
  assert.equal(i.mode, "tty");
  assert.deepEqual(i.args, ["-k", NODE, ...ARGS]);
  assert.equal(i.options.stdio, "inherit");
});

test("macOS without a terminal uses the system admin dialog via osascript argv", () => {
  const i = buildMacInvocation({ tty: false, nodeExe: NODE, args: ARGS, label: "RightKit ownership trust-sync" });
  assert.equal(i.mode, "dialog");
  assert.equal(i.command, "/usr/bin/osascript");
  assert.equal(i.args[0], "-e");
  assert.equal(i.args.length, 2);
  assert.match(i.args[1], /^do shell script ".*" with administrator privileges with prompt "RightKit ownership trust-sync"$/);
  assert.ok(i.args[1].includes(`'${NODE}'`));
  assert.equal(i.options.env.SUDO_ASKPASS, undefined);
});

const NASTY = ["plain", "with space", "it's", 'say "hi"', "back\\slash", "mix ' \" \\ $HOME `x` ; &", "''", ""];

test("shell quoting round-trips spaces, quotes and backslashes exactly through /bin/sh", () => {
  const cmd = `printf '%s\\n' ${NASTY.map(shellQuote).join(" ")}`;
  const r = spawnSync("/bin/sh", ["-c", cmd], { encoding: "utf8" });
  assert.equal(r.stdout, NASTY.map((v) => `${v}\n`).join(""));
});

test("AppleScript literal escaping and full command survive both layers", () => {
  assert.equal(appleScriptString('a"b\\c'), '"a\\"b\\\\c"');
  assert.throws(() => appleScriptString("a\nb"), /line break/);
  assert.throws(() => shellQuote("a\0b"), /NUL/);
  // Undo the AppleScript literal layer, then run the shell string without privileges.
  const cmd = buildMacShellCommand("/bin/echo", NASTY.slice(0, 7));
  const literal = appleScriptString(cmd);
  const unescaped = literal.slice(1, -1).replace(/\\(["\\])/g, "$1");
  assert.equal(unescaped, cmd);
  const r = spawnSync("/bin/sh", ["-c", unescaped.replace("/usr/bin/env '-u' 'NODE_OPTIONS' '-u' 'NODE_PATH' ", "")], { encoding: "utf8" });
  assert.equal(r.stdout, `${NASTY.slice(0, 7).join(" ")}\n`);
});

function macFixture() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-elev-mac-"));
  for (const n of ["ownership-key-command.mjs", "ownership-key-trust.mjs", "ownership-key-authority.mjs", "ownership-repositories.json"]) writeFileSync(path.join(dir, n), "// f\n");
  const request = path.join(dir, "request.json"); writeFileSync(request, "{}\n");
  return { dir, request, source: path.join(dir, "ownership-key-command.mjs"), digest: createHash("sha256").update("{}\n").digest("hex") };
}

test("elevate on macOS picks the dialog when there is no tty and sudo when there is", () => {
  const f = macFixture();
  try {
    for (const tty of [false, true]) {
      const calls = [];
      __testOnly.elevate((...a) => { calls.push(a); return { status: 0 }; }, f.request, f.digest, { modulePath: f.source, hostPlatform: "darwin", audit: () => ({ ok: true }), tty, label: "RightKit ownership trust-sync" });
      assert.equal(calls.some(([c]) => c === "/usr/bin/osascript"), !tty);
      assert.equal(calls.filter(([c, a]) => c === "/usr/bin/sudo" && a[0] === "-k").length, tty ? 1 : 0);
      assert.equal(calls.filter(([c, a]) => c === "/usr/bin/sudo" && a[0] === "-K").length, tty ? 1 : 0);
    }
    assert.throws(() => __testOnly.elevate(() => ({ status: 1 }), f.request, f.digest, { modulePath: f.source, hostPlatform: "darwin", audit: () => ({ ok: true }), tty: false }), /failed or was cancelled/);
  } finally { rmSync(f.dir, { recursive: true, force: true }); }
});

test("desktop delegation is chosen for elevated or SSH Windows runs only, never recursively", () => {
  const run = () => ({ status: 0 });
  assert.equal(needsDesktopDelegation({ hostPlatform: "darwin", env: {}, run }), false);
  assert.equal(needsDesktopDelegation({ hostPlatform: "win32", env: {}, run, elevatedCheck: () => false }), false);
  assert.equal(needsDesktopDelegation({ hostPlatform: "win32", env: {}, run, elevatedCheck: () => true }), true);
  assert.equal(needsDesktopDelegation({ hostPlatform: "win32", env: { SSH_CONNECTION: "1 2 3 4" }, run, elevatedCheck: () => false }), true);
  assert.equal(needsDesktopDelegation({ hostPlatform: "win32", env: { SSH_CONNECTION: "x", [DESKTOP_DELEGATED_ENV]: "1" }, run, elevatedCheck: () => true }), false);
});

test("desktop task is limited, interactive, for the logged-on user, with quoted wrapper", () => {
  const [, args] = buildSchtasksCreate("T1", "C:\\t\\run.ps1", "PC\\adrds");
  assert.deepEqual([args[args.indexOf("/RU") + 1], args.includes("/IT"), args[args.indexOf("/RL") + 1]], ["PC\\adrds", true, "LIMITED"]);
  const w = buildDesktopWrapper(["C:\\n'ode.exe", "cli.mjs", "ownership", "trust-sync"], "L", "C");
  assert.match(w, /'C:\\n''ode\.exe' 'cli\.mjs' 'ownership' 'trust-sync'/);
  assert.match(w, new RegExp(`${DESKTOP_DELEGATED_ENV}='1'`));
});

function fakeSchtasks({ user = "PC\\adrds", code = "0", log = "synced", never = false } = {}) {
  const calls = [];
  let wrapperDir;
  const run = (cmd, args) => {
    calls.push([cmd, args]);
    if (args.includes("-Command")) return { status: 0, stdout: `${user}\r\n` };
    if (args[0] === "/Create") { wrapperDir = path.dirname(args[args.indexOf("/TR") + 1].match(/-File "(.+)"$/)[1]); return { status: 0 }; }
    if (args[0] === "/Run") { if (!never) { writeFileSync(path.join(wrapperDir, "output.log"), log); writeFileSync(path.join(wrapperDir, "exit-code.txt"), `${code}\r\n`); } return { status: 0 }; }
    if (args[0] === "/Query") return { status: 0, stdout: "Status:   Ready\r\n" };
    return { status: 0 };
  };
  return { run, calls };
}

test("delegateToDesktop reports exit code and output, then deletes the task and temp dir", () => {
  const f = fakeSchtasks({ code: "3", log: "boom" });
  const r = delegateToDesktop({ run: f.run, command: ["node", "cli"], sleep() {}, suffix: "abc" });
  assert.deepEqual([r.status, r.output], [3, "boom"]);
  assert.ok(f.calls.some(([, a]) => a[0] === "/Delete" && a.includes("RightKitOwnershipTrustSync-abc")));
});

test("delegateToDesktop fails clearly with no interactive user or a stuck task", () => {
  assert.throws(() => delegateToDesktop({ run: () => ({ status: 0, stdout: "" }), command: ["n"], sleep() {} }), /no interactive user is logged on/);
  const f = fakeSchtasks({ never: true });
  let t = 0;
  assert.throws(() => delegateToDesktop({ run: f.run, command: ["n"], sleep() {}, now: () => (t += 20000) }), /without reporting a result/);
  assert.ok(f.calls.some(([, a]) => a[0] === "/Delete"));
});

test("desktop wrapper sets the working directory first and logs cwd and argv", () => {
  const w = buildDesktopWrapper(["node.exe", "C:\\r\\cli.mjs", "ownership", "trust-sync"], "L", "C", "D:\\Claude");
  assert.ok(w.indexOf("Set-Location -LiteralPath 'D:\\Claude'") > 0);
  assert.ok(w.indexOf("Set-Location") < w.indexOf("& 'node.exe'"));
  assert.match(w, /desktop task cwd:/);
  assert.match(w, /desktop task argv: /);
  assert.match(w, /Out-File -Append -Encoding utf8/);
});

test("delegateToDesktop passes the parent cwd to the task and decodes UTF-16LE logs", () => {
  const f = fakeSchtasks({ log: Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from("NativeCommandError: not a Git repository", "utf16le")]) });
  const seen = [];
  const run = (cmd, args, o) => { if (args[0] === "/Create") { const wrapper = args[args.indexOf("/TR") + 1].match(/-File "(.+)"$/)[1]; seen.push(readFileSync(wrapper, "utf8")); } return f.run(cmd, args, o); };
  const r = delegateToDesktop({ run, command: ["node", "cli"], sleep() {}, suffix: "cwd", cwd: "D:\\Claude" });
  assert.match(seen[0], /Set-Location -LiteralPath '.*Claude'/);
  assert.equal(r.output, "NativeCommandError: not a Git repository");
});

test("decodeLog handles BOM-less UTF-16LE, UTF-8 and BOM variants", () => {
  assert.equal(decodeLog(Buffer.from("hello world", "utf16le")), "hello world");
  assert.equal(decodeLog(Buffer.from("\uFEFFplain")), "plain");
  assert.equal(decodeLog(Buffer.from("caf\u00e9")), "caf\u00e9");
});
