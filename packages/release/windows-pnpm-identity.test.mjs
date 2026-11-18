import { test } from "node:test";
import assert from "node:assert/strict";
import { windowsPnpmIdentity } from "./snapshot-dependency-reuse.mjs";

const files = (map) => ({ exists: (f) => Object.hasOwn(map, f), readFile: (f) => { if (!Object.hasOwn(map, f)) throw new Error(`ENOENT ${f}`); return Buffer.from(map[f], "latin1"); }, execPath: "C:\\node\\node.exe" });

test("NN12: Windows pnpm is named by the shim and the program it starts", () => {
  const bin = "C:\\Users\\u\\AppData\\Local\\pnpm\\bin";
  const env = { Path: `relative;${bin};C:\\other`, PATHEXT: ".COM;.EXE;.BAT;.CMD" };
  // The Dell's pnpm 11+ global install (2026-11-18): pnpm.CMD runs an extensionless path, which cmd completes to pnpm.exe beside it.
  const shim = '@SETLOCAL\r\n@"%~dp0\\..\\global\\v11\\x\\node_modules\\pnpm\\pnpm"   %*\r\n';
  const exe = "C:\\Users\\u\\AppData\\Local\\pnpm\\global\\v11\\x\\node_modules\\pnpm\\pnpm.exe";
  const got = windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.cmd`]: shim, [exe]: "MZ native", [exe.replace(/\.exe$/, "")]: "# placeholder" }));
  assert.deepEqual(got.map(([f]) => f), [`${bin}\\pnpm.cmd`, exe]);
  // Another binary is another identity.
  const other = windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.cmd`]: shim, [exe]: "MZ other" }));
  assert.notEqual(other[1][1], got[1][1]);
  // A JS entry counts with the node that runs it.
  const js = windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.cmd`]: '@"%~dp0\\node_modules\\pnpm\\bin\\pnpm.cjs" %*\r\n', [`${bin}\\node_modules\\pnpm\\bin\\pnpm.cjs`]: "js", "C:\\node\\node.exe": "MZ node" }));
  assert.deepEqual(js.map(([f]) => f), [`${bin}\\pnpm.cmd`, `${bin}\\node_modules\\pnpm\\bin\\pnpm.cjs`, "C:\\node\\node.exe"]);
  // A pnpm.exe found directly.
  assert.deepEqual(windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.exe`]: "MZ" })).map(([f]) => f), [`${bin}\\pnpm.exe`]);
  // Not named: no pnpm, a shim through a variable or with two commands, a target that is not a Windows program, a relative PATH entry only.
  assert.equal(windowsPnpmIdentity(env, files({})), null);
  assert.equal(windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.cmd`]: '@"%NODE%" "%~dp0\\pnpm.cjs" %*' })), null);
  assert.equal(windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.cmd`]: '@"%~dp0\\a" %*\r\n@"%~dp0\\b" %*' })), null);
  assert.equal(windowsPnpmIdentity(env, files({ [`${bin}\\pnpm.cmd`]: shim, [exe]: "#!/bin/sh" })), null);
  assert.equal(windowsPnpmIdentity({ Path: "relative" }, files({ "relative\\pnpm.cmd": shim })), null);
});

test("NN12: with no HOME and pnpm's userconfig undefined, Windows takes ~/.npmrc from USERPROFILE (the Dell gate's environment)", async () => {
  const { pnpmConfigIdentity } = await import("./snapshot-dependency-reuse.mjs");
  const answers = { "list --json": "{}", "get userconfig": "undefined", "get globalconfig": process.platform === "win32" ? "C:\\nowhere\\pnpm\\config.yaml" : "/nowhere/pnpm/config.yaml" };
  const spawn = (_cmd, args) => ({ status: 0, stdout: answers[args.slice(1).join(" ")] ?? "" });
  const env = process.platform === "win32" ? { USERPROFILE: "C:\\nowhere\\home" } : { HOME: "/nowhere/home" };
  assert.equal(typeof pnpmConfigIdentity({ dir: ".", env, spawn }), "string");
  assert.throws(() => pnpmConfigIdentity({ dir: ".", env: {}, spawn }), /config-path-unknown/);
});
