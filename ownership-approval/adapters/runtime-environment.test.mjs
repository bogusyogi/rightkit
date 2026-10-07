import assert from "node:assert/strict";
import test from "node:test";
import { nativeGitExecutable, protectedChildEnvironment } from "./runtime-environment.mjs";

test("protected children discard runtime and Git injection while retaining ordinary host settings", () => {
  for (const platform of ["win32", "darwin"]) {
    const input = { NODE_OPTIONS: "--import attacker.mjs", Node_Path: "/attacker", GIT_CONFIG_COUNT: "1", GIT_CONFIG_KEY_0: "core.fsmonitor", GIT_CONFIG_VALUE_0: "attacker", LD_PRELOAD: "/attacker.so", DYLD_INSERT_LIBRARIES: "/attacker.dylib", BASH_ENV: "/attacker.sh", ENV: "/attacker.sh", PSModulePath: "/attacker", Path: "/attacker", PATH: "/attacker2", TEMP: "/host-temp", SystemRoot: "C:\\Windows" };
    const env = protectedChildEnvironment(input, platform);
    assert.equal(env.NODE_OPTIONS, undefined);
    assert.equal(env.Node_Path, undefined);
    assert.equal(env.GIT_CONFIG_COUNT, undefined);
    assert.equal(env.GIT_CONFIG_KEY_0, undefined);
    assert.equal(env.GIT_CONFIG_VALUE_0, undefined);
    assert.equal(env.LD_PRELOAD, undefined);
    assert.equal(env.DYLD_INSERT_LIBRARIES, undefined);
    assert.equal(env.BASH_ENV, undefined);
    assert.equal(env.ENV, undefined);
    assert.doesNotMatch(env.PSModulePath ?? "", /attacker/);
    assert.equal(env.Path, undefined);
    assert.doesNotMatch(env.PATH, /attacker/);
    assert.equal(env.TEMP, input.TEMP);
    assert.equal(env.SystemRoot, input.SystemRoot);
    assert.equal(env.GIT_NO_REPLACE_OBJECTS, "1");
    assert.equal(env.GIT_CONFIG_NOSYSTEM, "1");
    assert.equal(env.GIT_CONFIG_GLOBAL, platform === "win32" ? "NUL" : "/dev/null");
    assert.equal(input.NODE_OPTIONS, "--import attacker.mjs");
  }
  assert.equal(nativeGitExecutable("win32"), "C:\\Program Files\\Git\\cmd\\git.exe");
  assert.equal(nativeGitExecutable("darwin"), "/usr/bin/git");
});
