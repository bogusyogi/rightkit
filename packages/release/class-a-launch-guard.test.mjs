import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { REFUSAL_CODE, compileLaunch, shellWords } from "./class-a-launch-guard.mjs";
import { launchGuardEnv } from "./deterministic-scans.mjs";

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
    "spawnSync tsc": `import { spawnSync } from "node:child_process"; try { spawnSync("tsc", ["-p", "."]); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`,
    "execSync shell build": `import { execSync } from "node:child_process"; try { execSync("pnpm run build"); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`,
    "spawn sh -c cargo": `import { spawn } from "node:child_process"; try { spawn("sh", ["-c", "cargo build"]); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`,
    "nested node child": `import { spawnSync } from "node:child_process"; const r = spawnSync(process.execPath, ["--input-type=module", "-e", "import { spawnSync } from 'node:child_process'; try { spawnSync('esbuild', ['a.ts']); console.log('LAUNCHED') } catch (e) { console.log(e.code) }"], { encoding: "utf8" }); console.log(r.stdout.trim())`,
  };
  for (const [label, code] of Object.entries(cases)) {
    const r = probe(code);
    assert.match(r.stdout, new RegExp(REFUSAL_CODE), `${label}: ${r.stdout} ${r.stderr}`);
    assert.doesNotMatch(r.stdout, /LAUNCHED/, label);
  }
  // An executable named like a compiler under a temp directory is refused: no path-based exemption.
  const stub = path.join(dir, "cargo"); fs.writeFileSync(stub, "#!/bin/sh\necho stub\n", { mode: 0o755 });
  const tmpCompiler = probe(`import { spawnSync } from "node:child_process"; try { spawnSync(${JSON.stringify(stub)}, []); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`);
  assert.match(tmpCompiler.stdout, new RegExp(REFUSAL_CODE), "a temp-dir 'cargo' without a declaration is refused");
  // No exemption exists: an env switch or a harness temp root does not let a stub or a copied binary through.
  const tryEnv = { ...env, RIGHTKIT_CLASS_A_SCRIPT_STUBS: "1", RIGHTKIT_TEST_TMP: dir };
  const binary = path.join(dir, "rustc"); fs.copyFileSync(process.execPath, binary); fs.chmodSync(binary, 0o755);
  for (const target of [stub, binary]) {
    const r = spawnSync(process.execPath, ["--input-type=module", "-e", `import { spawnSync } from "node:child_process"; try { spawnSync(${JSON.stringify(target)}, ["-e", "1"]); console.log("LAUNCHED") } catch (e) { console.log(e.code) }`], { env: tryEnv, encoding: "utf8", cwd: dir });
    assert.match(r.stdout, new RegExp(REFUSAL_CODE), `${target} is refused`);
  }
  // A Node child handed a replacement environment without NODE_OPTIONS still runs under the guard.
  const replaced = probe(`import { spawnSync } from "node:child_process"; const r = spawnSync(process.execPath, ["--input-type=module", "-e", "import { spawnSync } from 'node:child_process'; try { spawnSync('tsc', ['-p', '.']); console.log('LAUNCHED') } catch (e) { console.log(e.code) }"], { encoding: "utf8", env: { PATH: process.env.PATH } }); console.log(r.stdout.trim())`);
  assert.match(replaced.stdout, new RegExp(REFUSAL_CODE), `replacement env: ${replaced.stdout} ${replaced.stderr}`);
  // The caller's options are not modified (code under test may inspect the env it passed).
  const untouched = probe(`import { spawnSync } from "node:child_process"; const o = { env: { PATH: process.env.PATH, NODE_OPTIONS: "" }, encoding: "utf8" }; spawnSync(process.execPath, ["-e", "1"], o); console.log("ENV", JSON.stringify(o.env.NODE_OPTIONS), Object.keys(o.env).length)`);
  assert.match(untouched.stdout, /^ENV "" 2$/m, untouched.stderr);
  // Allowed: a typecheck and a test runner.
  const allowed = probe(`import { spawnSync } from "node:child_process"; const a = spawnSync("tsc", ["--noEmit"]); const b = spawnSync(process.execPath, ["--test", "--help"], { encoding: "utf8" }); console.log("OK", a.error?.code ?? a.status, b.status)`);
  assert.match(allowed.stdout, /^OK /, allowed.stderr);
  // Without the guard env nothing is refused.
  const free = spawnSync(process.execPath, ["--input-type=module", "-e", `import { spawnSync } from "node:child_process"; const r = spawnSync("definitely-not-tsc-xyz", ["-p", "."]); console.log("RAN", r.error?.code)`], { encoding: "utf8" });
  assert.match(free.stdout, /RAN ENOENT/);
});
