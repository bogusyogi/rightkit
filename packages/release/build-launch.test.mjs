import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";

const CLIENT = "/managed/agent-bin/rustright";
const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
const direct = { env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [] };
// Broker-managed (a RustRight data root exists) but the client is unreachable from this shell.
const unreachable = { env: { PATH: "/usr/bin", RIGHTKIT_BUILD_BROKER_SOCKET: "/x/broker.sock" }, platform: "darwin", exists: () => true, which: () => [], readFile: () => "" };
const ownRightRelease = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-release.mjs");

test("managed host: pnpm is a rightkit-pnpm broker job; cargo and rustright go by the broker client; the door's own right-release is unchanged", () => {
  assert.deepEqual(buildLaunch("pnpm", ["--dir", "apps/web", "run", "build"], managed), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "--dir", "apps/web", "run", "build"] });
  assert.deepEqual(buildLaunch("pnpm.cmd", ["run", "test"], managed).args, ["run", "rightkit-pnpm", "--", "run", "test"]);
  assert.deepEqual(buildLaunch("cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] });
  assert.deepEqual(buildLaunch("swift", ["build", "--package-path", "swift/helper"], managed), { command: CLIENT, args: ["run", "rightkit-swift", "--", "build", "--package-path", "swift/helper"] });
  // A real cargo named by absolute path is still a broker job: the path is not trusted, the name is routed.
  assert.deepEqual(buildLaunch("/Users/x/.cargo/bin/cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] });
  assert.deepEqual(buildLaunch("/elsewhere/rustright", ["cargo", "test"], managed), { command: CLIENT, args: ["cargo", "test"] });
  assert.deepEqual(buildLaunch(process.execPath, [ownRightRelease, "web-build"], managed), { command: process.execPath, args: [ownRightRelease, "web-build"] });
});

test("managed host: node runs a workspace script as a rightkit-node broker job; a rightkit-qa run is a rightkit-qa broker job", () => {
  const host = { ...managed, cwd: "/w/app" };
  assert.deepEqual(buildLaunch("node", ["node_modules/vite/bin/vite.js", "build"], host), { command: CLIENT, args: ["run", "rightkit-node", "--", "node_modules/vite/bin/vite.js", "build"] });
  assert.deepEqual(buildLaunch(process.execPath, ["/w/app/scripts/dev-build.mjs", "--platform", "mac"], host).args, ["run", "rightkit-node", "--", "scripts/dev-build.mjs", "--platform", "mac"]);
  // The whole QA run (its test binaries included) is one broker job.
  assert.deepEqual(buildLaunch("rightkit-qa", ["run-tests", "--app", "heardright"], host), { command: CLIENT, args: ["run", "rightkit-qa", "--", "run-tests", "--app", "heardright"] });
});

test("managed host: any other build or test command is refused, never run around the broker", () => {
  // An executable merely named node that names right-release is not this Node binary running this package's right-release.
  assert.throws(() => buildLaunch("/tmp/evil/node", [ownRightRelease, "web-build"], { ...managed, cwd: "/w/app" }), (e) => e.code === BUILD_LAUNCH_REFUSED);
  for (const [command, args] of [["vite", ["build"]], ["npm", ["run", "build"]], ["sh", ["scripts/prepackage-mac.sh"]], ["xcodebuild", ["build"]], ["gradle", ["assembleDebug"]], ["sh", ["-c", "pnpm run build"]], ["tsc", ["-p", "."]], ["node", ["/elsewhere/build.mjs"]], ["node", ["../other/build.mjs"]], ["node", ["-e", "1"]], ["powershell", ["-File", "x.ps1"]]]) {
    assert.throws(() => buildLaunch(command, args, managed), (e) => e.code === BUILD_LAUNCH_REFUSED && /rightkit#1241/.test(e.message), command);
  }
});

test("host detection reads the cargo on PATH: the RightKit cargo guard makes it managed (pnpm then needs the client); a plain cargo is direct", () => {
  // Mac and Windows only: every Linux host is direct whatever cargo is on PATH (gate-executor directPlatform).
  const guard = { env: { PATH: "/g" }, platform: "darwin", exists: () => false, which: (c) => (c === "cargo" ? ["/g/cargo"] : []), readFile: () => "#!/usr/bin/env node\nimport './cargo-guard.mjs';\n" };
  assert.throws(() => buildLaunch("pnpm", ["run", "build"], guard), (e) => e.code === BUILD_LAUNCH_REFUSED && /rustright is not on PATH/.test(e.message));
  const plain = { ...guard, readFile: () => "\x7fELF" };
  assert.deepEqual(buildLaunch("pnpm", ["run", "build"], plain), { command: "pnpm", args: ["run", "build"] });
  assert.equal(buildLaunch("pnpm", ["run", "build"], { ...guard, platform: "linux" }).command, "pnpm");
});

test("a managed host whose broker is unreachable refuses every launch; a host without a broker runs directly", () => {
  for (const [command, args] of [["pnpm", ["run", "build"]], ["cargo", ["build"]], ["node", ["scripts/build.mjs"]], ["rightkit-qa", ["run-tests"]]]) {
    assert.throws(() => buildLaunch(command, args, unreachable), (e) => e.code === BUILD_LAUNCH_REFUSED && /broker-managed/.test(e.message), command);
  }
  assert.deepEqual(buildLaunch("pnpm", ["run", "build"], direct), { command: "pnpm", args: ["run", "build"] });
  assert.deepEqual(buildLaunch("vite", ["build"], direct), { command: "vite", args: ["build"] });
});

test("managed host: node flags that change no code path pass to the adapter; eval, import, require and loader flags refuse", () => {
  const host = { ...managed, cwd: "/w/app" };
  assert.deepEqual(buildLaunch("node", ["--disable-warning=DEP0205", "node_modules/typescript/bin/tsc"], host).args, ["run", "rightkit-node", "--", "--disable-warning=DEP0205", "node_modules/typescript/bin/tsc"]);
  for (const flags of [["-e", "1"], ["--eval=1"], ["-p", "1"], ["--import", "x.mjs"], ["--import=x.mjs"], ["-r", "x"], ["--require=x"], ["--loader", "x"], ["--experimental-loader=x"]]) {
    assert.throws(() => buildLaunch("node", [...flags, "scripts/build.mjs"], host), (e) => e.code === BUILD_LAUNCH_REFUSED, flags.join(" "));
  }
});

test("managed host: npm (whose install lifecycle scripts can build) refuses; the door's own right-release refuses added startup code", () => {
  assert.throws(() => buildLaunch("npm", ["ci"], managed), (e) => e.code === BUILD_LAUNCH_REFUSED);
  const injected = { ...managed, env: { ...managed.env, NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=/tmp/build.mjs`.trim() } };
  assert.throws(() => buildLaunch(process.execPath, [ownRightRelease, "web-build"], injected), (e) => e.code === BUILD_LAUNCH_REFUSED && /NODE_OPTIONS/.test(e.message));
  // Reordered tokens of an inherited value can re-pair an option with another value: anything but the exact string refuses.
  const original = process.env.NODE_OPTIONS;
  process.env.NODE_OPTIONS = "--max-old-space-size=4096 --import ./a.mjs";
  try {
    const reordered = { ...managed, env: { ...managed.env, NODE_OPTIONS: "--import --max-old-space-size=4096 ./a.mjs" } };
    assert.throws(() => buildLaunch(process.execPath, [ownRightRelease, "web-build"], reordered), (e) => e.code === BUILD_LAUNCH_REFUSED);
    const exact = { ...managed, env: { ...managed.env, NODE_OPTIONS: process.env.NODE_OPTIONS } };
    assert.equal(buildLaunch(process.execPath, [ownRightRelease, "web-build"], exact).command, process.execPath);
  } finally { if (original === undefined) delete process.env.NODE_OPTIONS; else process.env.NODE_OPTIONS = original; }
  const same = { ...managed, env: { ...managed.env, ...(process.env.NODE_OPTIONS !== undefined ? { NODE_OPTIONS: process.env.NODE_OPTIONS } : {}) } };
  assert.deepEqual(buildLaunch(process.execPath, [ownRightRelease, "web-build"], same).command, process.execPath);
});
