import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { cargoAboutArgs, generateCargoNotices, parseCargoNoticeArgs, resolveCargoNoticeOptions } from "./cargo-license-notices.mjs";

const APP = path.resolve("/rk-app");
const config = (over = {}) => ({ legal: { cargoNotices: { output: "legal/CARGO.html", template: "legal/about.hbs", config: "legal/about.toml", manifestPath: "src-tauri/Cargo.toml", ...over } } });
const key = (...parts) => path.normalize(path.join(APP, ...parts));

test("options are validated and the cargo about argument list is exact", () => {
  const options = resolveCargoNoticeOptions(config({ features: ["a", "b"] }));
  assert.deepEqual(cargoAboutArgs(options), ["about", "generate", "--frozen", "--fail", "--config", "legal/about.toml", "--manifest-path", "src-tauri/Cargo.toml", "--features", "a,b", "legal/about.hbs"]);
  assert.deepEqual(cargoAboutArgs(resolveCargoNoticeOptions(config({ allFeatures: true, config: undefined, manifestPath: undefined }))), ["about", "generate", "--frozen", "--fail", "--all-features", "legal/about.hbs"]);
  for (const bad of [{ output: "../x" }, { template: undefined }, { template: "/abs" }, { features: "a" }, { features: ["a b"] }, { allFeatures: true, features: ["a"] }, { allFeatures: "yes" }, { nope: 1 }]) {
    assert.throws(() => resolveCargoNoticeOptions(config(bad)), /legal\.cargoNotices|no output|required/, JSON.stringify(bad));
  }
  assert.throws(() => resolveCargoNoticeOptions({}), /no output file/);
  assert.deepEqual(parseCargoNoticeArgs(["--check", "--output", "o"]), { config: "right-release.config.mjs", output: "o", check: true });
});

test("generate writes the normalised stdout and --check compares it", () => {
  const files = new Map([[key("legal", "about.hbs"), "t"], [key("legal", "about.toml"), "c"], [key("src-tauri", "Cargo.toml"), "m"]]);
  const seen = {};
  const deps = { exists: (f) => files.has(path.normalize(f)), readText: (f) => files.get(path.normalize(f)), writeText: (f, t) => files.set(path.normalize(f), t), run: (args, options) => { seen.args = args; seen.options = options; return { status: 0, stdout: "<html>\r\nlicenses\r\n\r\n" }; } };
  const written = generateCargoNotices({ appRoot: APP, config: config(), ...deps });
  assert.equal(files.get(key("legal", "CARGO.html")), "<html>\nlicenses\n");
  assert.equal(seen.options.cwd, APP);
  assert.equal(written.upToDate, true);
  assert.equal(generateCargoNotices({ appRoot: APP, config: config(), check: true, ...deps }).upToDate, true);
  files.set(key("legal", "CARGO.html"), "old");
  assert.equal(generateCargoNotices({ appRoot: APP, config: config(), check: true, ...deps }).upToDate, false);
});

test("generate fails closed: missing inputs, a failing or absent cargo about, empty output", () => {
  const ok = { exists: () => true, readText: () => "", writeText: () => {} };
  assert.throws(() => generateCargoNotices({ appRoot: APP, config: config(), exists: () => false, run: () => ({ status: 0, stdout: "x" }) }), /does not exist/);
  assert.throws(() => generateCargoNotices({ appRoot: APP, config: config(), ...ok, run: () => ({ error: new Error("ENOENT") }) }), /could not start/);
  assert.throws(() => generateCargoNotices({ appRoot: APP, config: config(), ...ok, run: () => ({ status: 1, stderr: "unaccepted license" }) }), /unaccepted license/);
  assert.throws(() => generateCargoNotices({ appRoot: APP, config: config(), ...ok, run: () => ({ status: 0, stdout: "  \n" }) }), /no output/);
});

test("right-release routes license-notices cargo and binaries to their own modules", async () => {
  const { spawnSync } = await import("./background-process.mjs");
  const { fileURLToPath } = await import("node:url");
  const cli = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
  for (const kind of ["cargo", "binaries"]) {
    const result = spawnSync(process.execPath, [cli, "license-notices", kind, "--not-an-option"], { encoding: "utf8", windowsHide: true });
    assert.equal(result.status, 1, `${kind}: ${result.stderr}`);
    assert.match(result.stderr, /unknown license-notices argument: --not-an-option/, kind);
  }
});
