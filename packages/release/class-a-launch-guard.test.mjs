import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { REFUSAL_CODE, compileLaunch, shellWords } from "./class-a-launch-guard.mjs";
import { defaultSpawn, launchGuardEnv } from "./deterministic-scans.mjs";

// The probes are child scripts; the module name is joined, not written (background-process.test.mjs).
const CP = ["node:child", "process"].join("_");

test("compileLaunch names compiles and emitting builds; checks, tests and typechecks pass", () => {
  const refused = [
    [["cargo", "build"], "cargo"], [["/usr/local/bin/rustc", "x.rs"], "rustc"], [["rustright", "cargo", "test"], "rustright"],
    [["tsc", "-p", "tsconfig.json"], "tsc (emits)"], [["node", "node_modules/typescript/bin/tsc", "-p", "."], "tsc (emits)"],
    [["esbuild", "src/a.ts"], "esbuild"], [["vite", "build"], "vite build"], [["next", "build"], "next build"],
    [["pnpm", "run", "build"], "pnpm build"], [["pnpm", "--filter", "@rightkit/x", "build"], "pnpm build"], [["npm", "run", "build:types"], "npm build:types"],
    [["pnpm", "exec", "tsc"], "tsc (emits)"], [["npx", "tsup"], "tsup"], [["env", "FOO=1", "cargo", "check"], "cargo"],
    [["sh", "-c", "echo hi && tsc -p ."], "tsc (emits)"], [["cmd", "/c", "pnpm run build"], "pnpm build"],
  ];
  for (const [argv, why] of refused) assert.equal(compileLaunch(argv[0], argv.slice(1)), why, argv.join(" "));
  const allowed = [["tsc", "--noEmit"], ["tsc", "-p", ".", "--noEmit"], ["vitest", "run"], ["node", "--test", "a.test.mjs"], ["pnpm", "run", "lint"], ["vite"], ["git", "status"], ["sh", "-c", "echo build"], ["pnpm", "exec", "vitest", "run"]];
  for (const argv of allowed) assert.equal(compileLaunch(argv[0], argv.slice(1)), null, argv.join(" "));
  assert.deepEqual(shellWords("a 'b c' && d \"e\";f|g"), [["a", "b c"], ["d", "e"], ["f"], ["g"]]);
});

test("in a class A test row every Node process refuses a compile launch, nested children and shells included; fixtures and checks pass", (t) => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-launch-guard-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const env = { ...process.env, ...launchGuardEnv(process.env) };
  assert.match(env.NODE_OPTIONS, /class-a-launch-guard\.mjs/);
  assert.ok(env.NODE_OPTIONS.includes(String(process.env.NODE_OPTIONS ?? "").trim()), "NODE_OPTIONS is appended to, not replaced");
  const probe = (code) => spawnSync(process.execPath, ["--input-type=module", "-e", code], { env, encoding: "utf8", cwd: dir });
  const cases = {
    "spawnSync tsc": `import { spawnSync } from "${CP}"; try { spawnSync("tsc", ["-p", "."]); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`,
    "execSync shell build": `import { execSync } from "${CP}"; try { execSync("pnpm run build"); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`,
    "spawn sh -c cargo": `import { spawn } from "${CP}"; try { spawn("sh", ["-c", "cargo build"]); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`,
    "nested node child": `import { spawnSync } from "${CP}"; const r = spawnSync(process.execPath, ["--input-type=module", "-e", "import { spawnSync } from '${CP}'; try { spawnSync('esbuild', ['a.ts']); console.log('LAUNCHED') } catch (e) { console.log(e.code) }"], { encoding: "utf8" }); console.log(r.stdout.trim())`,
  };
  for (const [label, code] of Object.entries(cases)) {
    const r = probe(code);
    assert.match(r.stdout, new RegExp(REFUSAL_CODE), `${label}: ${r.stdout} ${r.stderr}`);
    assert.doesNotMatch(r.stdout, /LAUNCHED/, label);
  }
  // An executable named like a compiler under a temp directory is refused: no path-based exemption.
  const stub = path.join(dir, "cargo"); fs.writeFileSync(stub, "#!/bin/sh\necho stub\n", { mode: 0o755 });
  const tmpCompiler = probe(`import { spawnSync } from "${CP}"; try { spawnSync(${JSON.stringify(stub)}, []); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`);
  assert.match(tmpCompiler.stdout, new RegExp(REFUSAL_CODE), "a temp-dir 'cargo' without a declaration is refused");
  // No exemption exists: an env switch or a harness temp root does not let a stub or a copied binary through.
  const tryEnv = { ...env, RIGHTKIT_CLASS_A_SCRIPT_STUBS: "1", RIGHTKIT_TEST_TMP: dir };
  const binary = path.join(dir, "rustc"); fs.copyFileSync(process.execPath, binary); fs.chmodSync(binary, 0o755);
  for (const target of [stub, binary]) {
    const r = spawnSync(process.execPath, ["--input-type=module", "-e", `import { spawnSync } from "${CP}"; try { spawnSync(${JSON.stringify(target)}, ["-e", "1"]); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`], { env: tryEnv, encoding: "utf8", cwd: dir });
    assert.match(r.stdout, new RegExp(REFUSAL_CODE), `${target} is refused`);
  }
  // A Node child handed a replacement environment without NODE_OPTIONS still runs under the guard.
  const replaced = probe(`import { spawnSync } from "${CP}"; const r = spawnSync(process.execPath, ["--input-type=module", "-e", "import { spawnSync } from '${CP}'; try { spawnSync('tsc', ['-p', '.']); console.log('LAUNCHED') } catch (e) { console.log(e.code) }"], { encoding: "utf8", env: { PATH: process.env.PATH } }); console.log(r.stdout.trim())`);
  assert.match(replaced.stdout, new RegExp(REFUSAL_CODE), `replacement env: ${replaced.stdout} ${replaced.stderr}`);
  // The caller's options are not modified (code under test may inspect the env it passed).
  const untouched = probe(`import { spawnSync } from "${CP}"; const o = { env: { PATH: process.env.PATH, NODE_OPTIONS: "" }, encoding: "utf8" }; spawnSync(process.execPath, ["-e", "1"], o); console.log("ENV", JSON.stringify(o.env.NODE_OPTIONS), Object.keys(o.env).length)`);
  assert.match(untouched.stdout, /^ENV "" 2$/m, untouched.stderr);
  // Allowed: a typecheck and a test runner.
  const allowed = probe(`import { spawnSync } from "${CP}"; const a = spawnSync("tsc", ["--noEmit"]); const b = spawnSync(process.execPath, ["--test", "--help"], { encoding: "utf8" }); console.log("OK", a.error?.code ?? a.status, b.status)`);
  assert.match(allowed.stdout, /^OK /, allowed.stderr);
  // Without the guard env nothing is refused.
  const free = spawnSync(process.execPath, ["--input-type=module", "-e", `import { spawnSync } from "${CP}"; const r = spawnSync("definitely-not-tsc-xyz", ["-p", "."]); console.log("RAN", r.error?.code)`], { encoding: "utf8" });
  assert.match(free.stdout, /RAN ENOENT/);
});

test("compileLaunch: pnpm's -w is --workspace-root (a flag), npm's -w takes a workspace name (joint plan step 4)", async () => {
  const { compileLaunch } = await import("./class-a-launch-guard.mjs");
  assert.match(compileLaunch("pnpm", ["-w", "build"]) ?? "", /pnpm build/, "pnpm -w build is a root build");
  assert.match(compileLaunch("pnpm", ["-w", "run", "build"]) ?? "", /pnpm build/);
  assert.match(compileLaunch("pnpm", ["--filter", "web", "build:all"]) ?? "", /pnpm build:all/);
  assert.equal(compileLaunch("pnpm", ["-w", "test"]), null, "pnpm -w test is not a build");
  assert.match(compileLaunch("npm", ["-w", "packages/web", "run", "build"]) ?? "", /npm build/, "npm -w <name> takes a value");
  assert.equal(compileLaunch("npm", ["-w", "build", "test"]), null, "npm -w build is the workspace named build, then test");
  assert.match(compileLaunch("sh", ["-c", "pnpm -w build && node --test"]) ?? "", /pnpm build/);
});

// Shell expansion and control syntax (RA-10, R3-X05): the guard reads shell text before any shell starts and fails closed.
const viaSh = (text) => compileLaunch("sh", ["-c", text]);

test("compileLaunch refuses shell text it cannot read safely: substitutions, backticks, control forms, functions, nested wrappers, hidden programs", () => {
  const refused = [
    "echo $(cargo check --locked)", "echo `cargo check`", "echo \"$(cargo check)\"", "echo \"`cargo check`\"", "x=$(cargo build); echo $x", "echo ${x:-$(cargo check)}",
    "cat <(cargo check)", "tee >(cargo check)", "echo $((1+2))", "echo ${!name}", "echo ${x@P}", "echo $'\\x63argo'",
    "f() { cargo check; }; f", "function f { cargo check; }; f", "if true; then cargo check; fi", "for i in 1; do cargo check; done", "while true; do cargo check; break; done",
    "{ cargo check; }", "(cargo check)", "case x in x) cargo check;; esac", "! cargo check", "{cargo,check}", "ca{r,}go check",
    "echo ok\ncargo check", "c\\argo check", "ca\"\"rgo check", "'cargo' check", "CC=cargo; $CC check", "\"$CC\" check", "/usr/bin/car* check", "=cargo check",
    "FOO=1 cargo check", ">/dev/null cargo check", "2>&1 cargo check", "> out.txt cargo check", "echo hi && cargo check", "echo hi || cargo check", "echo hi & cargo check",
    "sh -c 'echo $(cargo check)'", "sh -c \"sh -c 'cargo check'\"", "bash -lc 'cargo check'", "env sh -c 'cargo check'", "env -i cargo check", "nice -n 5 env FOO=1 sh -c \"cargo check\"",
    "timeout 5 cargo check", "command cargo check", "exec cargo check", "nohup cargo check", "sudo -u x cargo check", "busybox sh -c 'cargo check'",
    "eval 'cargo check'", "eval $CMD", "source ./x.sh", ". ./x.sh", "trap 'cargo check' EXIT", "echo check | xargs cargo", "find . -maxdepth 0 -exec cargo check \\;",
    "echo cargo check | sh", "sh <<< 'cargo check'", "sh <<EOF\ncargo check\nEOF", "sh -s", "bash $FLAGS 'cargo check'", "sh -c \"$CMD\"",
    "pnpm run $SCRIPT", "pnpm run build", "tsc -p .", "echo 'unterminated",
  ];
  for (const text of refused) assert.ok(viaSh(text), `refused: ${JSON.stringify(text)}`);
  // Nesting that exceeds the bounded depth is refused, not passed.
  assert.ok(compileLaunch("sh", ["-c", `${Array(30).fill("env").join(" ")} true`]), "a wrapper chain deeper than the bound is refused");
  assert.ok(compileLaunch("cmd", ["/c", "cargo check"]) && compileLaunch("cmd", ["/c", "echo ^c^argo"]) && compileLaunch("cmd", ["/c", "%CC% check"]), "cmd text");
  // cmd.exe delayed expansion builds words after the text is read: refused, with a reason naming it, in every spelling.
  const delayed = [
    ["cmd", ["/v:on", "/c", "set p=cargo&!p! build"]], ["cmd", ["/V:ON", "/C", "set p=cargo&!p! build"]], ["cmd", ["/v", "/c", "set p=cargo&!p! build"]],
    ["cmd", ["/v:on/c", "set p=cargo&!p! build"]], ["cmd", ["/q", "/v:on", "/c", "echo hi"]], ["cmd", ["/v:on", "script.bat"]],
    ["cmd", ["/c", "setlocal enabledelayedexpansion & set p=cargo & !p! build"]], ["cmd", ["/c", "setlocal EnableDelayedExpansion&echo hi"]],
    ["cmd", ["/c", "set p=cargo&!p! build"]], ["cmd", ["/c", "set p=cargo&!p:~0,5! build"]], ["cmd", ["/c", "set p=xcargo&!p:~1,5! build"]],
    ["env", ["cmd", "/v:on", "/c", "set p=cargo&!p! build"]], ["sh", ["-c", "cmd /v:on /c \"set p=cargo&!p! build\""]], ["cmd", ["/c", "cmd /v:on /c \"set p=cargo&!p! build\""]],
    ["cmd", ["/c", "echo hi", "&", "cmd", "/v:on", "/c", "!p!"]],
  ];
  for (const [program, args] of delayed) assert.match(String(compileLaunch(program, args)), /delayed expansion/, `${program} ${args.join(" ")}`);
  // A lone `!` in quoted echo text is not a reference (no `!name!` pair) and /v:off keeps expansion off: allowed, as a lone % is not.
  // Anything with a `!x!` pair is refused even when quoted, because the registry default can leave expansion on.
  assert.equal(compileLaunch("cmd", ["/c", "echo \"wow!\""]), null, "a lone ! in a quoted echo");
  assert.equal(compileLaunch("cmd", ["/v:off", "/c", "echo \"wow!\""]), null, "/v:off with a lone !");
  assert.match(String(compileLaunch("cmd", ["/c", "echo \"hi! there!\""])), /delayed expansion/, "a !..! pair is refused even in quotes (fail closed)");
  assert.ok(compileLaunch("pwsh", ["-Command", "cargo check"]) && compileLaunch("pwsh", ["-Command", "echo $(cargo check)"]) && compileLaunch("pwsh", ["-EncodedCommand", "Y2FyZ28="]), "pwsh text");
  // What is named, not launched, stays allowed; so do ordinary commands with plain, quoted or $VAR arguments.
  const allowed = [
    "echo hello", "echo cargo", "echo build", "echo tsc -p .", "echo 'a $(cargo b)'", "echo 'a `cargo b`'", "echo \"a;b\" && echo c", "echo \"$HOME\" > /dev/null; echo ok", "echo $HOME $USER",
    "printf '%s\\n' x | cat", "FOO=1 sh -c 'echo $FOO'", "echo hi 2>&1", "echo hi # && cargo check", "[ -d . ] && echo yes", "test -d . || echo no", "cd . && pwd", "node --version", "git status",
    "tsc --noEmit", "pnpm run lint", "pnpm exec vitest run", "command -v cargo", "echo a\\ b", "ls -la {}", "echo hi >$OUT",
  ];
  for (const text of allowed) assert.equal(viaSh(text), null, `allowed: ${JSON.stringify(text)}`);
  assert.equal(compileLaunch("sh", ["-c", "echo $(date)"]) !== null, true, "a substitution is refused even when the inner command is not a compiler: text the guard cannot read is not passed");
  assert.deepEqual(shellWords("a\nb"), [["a"], ["b"]]);
  assert.deepEqual(shellWords("echo hi 2>&1"), [["echo", "hi", "2>&1"]]);
  assert.deepEqual(shellWords("c\\argo x"), [["cargo", "x"]]);
  assert.deepEqual(shellWords("echo $HOME '$X'"), [["echo", "$HOME", "$X"]]);
});

// Real process-launch witness: executables named like compilers that record each start. The guarded runner is the production one
// (deterministic-scans defaultSpawn with launchGuardEnv, the pair runScans uses for a class A row). A refused launch starts nothing,
// so the witness stays empty; the same forms without the guard do start it, which proves the witness sees a real start.
const PROBE = `
import cp from ${JSON.stringify(CP)};
const [cmds, modes] = JSON.parse(process.argv[1]);
const out = [];
for (const cmd of cmds) for (const mode of modes) {
  try {
    let stdout;
    if (mode === "execSync") stdout = cp.execSync(cmd, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    else if (mode === "exec") stdout = await new Promise((res, rej) => cp.exec(cmd, { encoding: "utf8" }, (e, so) => (e ? rej(e) : res(so))));
    else if (mode === "spawnSync-sh") { const r = cp.spawnSync("sh", ["-c", cmd], { encoding: "utf8" }); if (r.status !== 0) throw Object.assign(new Error("exit " + r.status), { status: r.status }); stdout = r.stdout; }
    else if (mode === "spawnSync-shell") { const r = cp.spawnSync(cmd, { shell: true, encoding: "utf8" }); if (r.status !== 0) throw Object.assign(new Error("exit " + r.status), { status: r.status }); stdout = r.stdout; }
    else if (mode === "execFileSync-sh") stdout = cp.execFileSync("sh", ["-c", cmd], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    out.push({ cmd, mode, ran: true, stdout: String(stdout).trim() });
  } catch (e) { out.push({ cmd, mode, ran: false, code: e.code ?? null }); }
}
process.stdout.write(JSON.stringify(out));
`;
const MODES = ["execSync", "exec", "spawnSync-sh", "spawnSync-shell", "execFileSync-sh"];

test("through the guarded production runner, shell expansion and control forms start zero compilers; ordinary commands still run", { skip: process.platform === "win32" }, (t) => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-launch-witness-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const witness = path.join(dir, "witness.log");
  const bin = path.join(dir, "bin"); fs.mkdirSync(bin);
  for (const name of ["cargo", "rustc", "tsc", "esbuild"]) fs.writeFileSync(path.join(bin, name), `#!/bin/sh\necho "${name} $*" >> "$RK_WITNESS"\n`, { mode: 0o755 });
  // Only this guard is loaded: the base NODE_OPTIONS is emptied so no other hook decides the outcome.
  const base = { ...process.env, NODE_OPTIONS: "", PATH: `${bin}${path.delimiter}${process.env.PATH}`, RK_WITNESS: witness };
  const guarded = { ...base, ...launchGuardEnv(base) };
  assert.match(guarded.NODE_OPTIONS, /class-a-launch-guard\.mjs/);
  const run = (env, cmds, modes = MODES) => {
    const r = defaultSpawn(process.execPath, ["--input-type=module", "-e", PROBE, JSON.stringify([cmds, modes])], { env, cwd: dir, encoding: "utf8" });
    assert.equal(r.status, 0, `probe: ${r.stderr}`);
    return JSON.parse(r.stdout);
  };
  const starts = () => (fs.existsSync(witness) ? fs.readFileSync(witness, "utf8").split("\n").filter(Boolean) : []);

  // Control: without the guard the same forms start the witness, one start per form per mode that reaches a shell.
  const control = ["echo $(cargo control-1)", "echo `cargo control-2`", "echo \"$(cargo control-3)\"", "f() { cargo control-4; }; f", "if true; then cargo control-5; fi", "sh -c \"sh -c 'cargo control-6'\"", "env sh -c 'cargo control-7'", "echo ok\ncargo control-8"];
  run(base, control, ["execSync"]);
  for (let i = 1; i <= control.length; i++) assert.ok(starts().some((l) => l === `cargo control-${i}`), `the witness sees an unguarded start of control-${i}`);
  fs.rmSync(witness);

  // Guarded: every form is refused before a shell starts, in every launcher mode; the witness stays empty.
  const refused = [
    "echo $(cargo check --locked)", "echo `cargo check`", "echo \"$(cargo check)\"", "echo \"`cargo check`\"", "x=$(cargo build); echo $x", "cat <(cargo check)",
    "f() { cargo check; }; f", "function f { cargo check; }; f", "if true; then cargo check; fi", "for i in 1; do cargo check; done", "while true; do cargo check; break; done", "{ cargo check; }", "(cargo check)", "! cargo check",
    "sh -c 'echo $(cargo check)'", "sh -c \"sh -c 'cargo check'\"", "env sh -c 'cargo check'", "nice -n 5 env FOO=1 sh -c \"cargo check\"", "timeout 5 cargo check", "command cargo check", "nohup cargo check",
    "eval 'cargo check'", "echo check | xargs cargo", "echo cargo check | sh", "sh <<< 'cargo check'", "echo ok\ncargo check", "c\\argo check", "ca\"\"rgo check", "CC=cargo; $CC check",
    `${bin}/car* check`, "$'\\x63argo' check", "{cargo,check}", ">/dev/null cargo check", "FOO=1 cargo check", "echo hi && rustc x.rs", "tsc -p .", "cargo check",
  ];
  for (const r of run(guarded, refused)) {
    assert.equal(r.ran, false, `${r.mode}: ${JSON.stringify(r.cmd)} must not run`);
    assert.equal(r.code, REFUSAL_CODE, `${r.mode}: ${JSON.stringify(r.cmd)} is refused by the guard`);
  }
  assert.deepEqual(starts(), [], "zero compiler starts for every refused form");

  // A Node child the shell starts inherits the guard: a substitution inside its own launch is refused there, and nothing starts.
  const nested = `'${process.execPath}' -e 'require("${CP}").execSync("echo $(cargo nested)")'`;
  const [inner] = run(guarded, [nested], ["execSync"]);
  assert.equal(inner.ran, false, "the nested Node child fails (its launch was refused)");
  assert.deepEqual(starts(), [], "zero compiler starts through a nested Node child");

  // Ordinary permitted commands still work, in every launcher mode.
  const permitted = {
    "echo hello": "hello", "echo cargo": "cargo", "echo 'a $(cargo b)'": "a $(cargo b)", "echo \"a;b\" && echo c": "a;b\nc", "echo \"$HOME\" > /dev/null; echo ok": "ok",
    "FOO=1 sh -c 'echo $FOO'": "1", "[ -d . ] && echo yes": "yes", "printf '%s\\n' x | cat": "x", "echo hi # && cargo check": "hi",
    [`'${process.execPath}' -e 'process.stdout.write("node-ok")'`]: "node-ok",
  };
  for (const r of run(guarded, Object.keys(permitted))) {
    assert.equal(r.ran, true, `${r.mode}: ${JSON.stringify(r.cmd)} still runs (${r.code})`);
    assert.equal(r.stdout, permitted[r.cmd], `${r.mode}: ${JSON.stringify(r.cmd)}`);
  }
  assert.deepEqual(starts(), [], "permitted commands start no compiler");
});
