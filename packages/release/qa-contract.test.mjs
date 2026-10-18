import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { validateQaBackdoorContract } from "./qa-contract.mjs";

function fixture({ optional = true, releaseScript = "right-release release --tier patch" } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-qa-"));
  mkdirSync(path.join(root, "src-tauri", "src"), { recursive: true });
  writeFileSync(path.join(root, "right-qa.config.mjs"), "export default {};\n");
  writeFileSync(path.join(root, "src-tauri", "Cargo.toml"), `
[features]
qa-native = ["dep:tauri-plugin-wdio-webdriver"]
[dependencies]
tauri-plugin-wdio-webdriver = { version = "1.2.0", optional = ${optional} }
`);
  writeFileSync(path.join(root, "src-tauri", "src", "lib.rs"), `
#[cfg(all(not(debug_assertions), feature = "qa-native"))]
compile_error!("qa-native must never be enabled in release builds");
#[cfg(all(debug_assertions, feature = "qa-native"))]
fn qa_enabled() -> bool { std::env::var("RIGHTKIT_QA_NATIVE").as_deref() == Ok("1") }
#[cfg(all(debug_assertions, feature = "qa-native"))]
fn add_plugin() { tauri_plugin_wdio_webdriver::init(); }
`);
  return { root, scripts: { "qa:native": "tauri dev --features qa-native", "release:patch:win": releaseScript } };
}

test("accepts a debug-only, runtime-gated native QA surface", () => {
  const value = fixture();
  assert.deepEqual(validateQaBackdoorContract(value.root, value.scripts), []);
});

test("rejects a non-optional WebDriver plugin", () => {
  const value = fixture({ optional: false });
  assert.match(validateQaBackdoorContract(value.root, value.scripts).join("\n"), /must be optional/);
});

test("rejects release scripts that enable native QA", () => {
  const value = fixture({ releaseScript: "tauri build --features qa-native" });
  assert.match(validateQaBackdoorContract(value.root, value.scripts).join("\n"), /release:patch:win/);
});

test("does not impose native QA on an app before its config lands", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-no-qa-"));
  assert.deepEqual(validateQaBackdoorContract(root, {}), []);
});

test("a native iOS app passes on its repository's QA lane only when asked to, and a repository with no QA is still refused", async () => {
  const { evaluateQaAdoption } = await import("./qa-contract.mjs");
  const fsMod = await import("node:fs");
  const osMod = await import("node:os");
  const pathMod = await import("node:path");
  const repo = fsMod.mkdtempSync(pathMod.join(osMod.tmpdir(), "rk-qa-ios-"));
  const ios = pathMod.join(repo, "apps", "app-ios");
  const desktop = pathMod.join(repo, "apps", "app-desktop");
  fsMod.mkdirSync(ios, { recursive: true });
  fsMod.mkdirSync(desktop, { recursive: true });
  const deps = { versions: { cargo: {}, npm: {} } };
  const without = evaluateQaAdoption({ appRoot: ios, repoRoot: repo, app: "app-ios", inheritRepoLane: true }, deps);
  assert.equal(without.refusals.length, 1, "no QA anywhere in the repository is refused");
  fsMod.writeFileSync(pathMod.join(desktop, "package.json"), JSON.stringify({ devDependencies: { "@rightkit/qa": "0.6.4" } }));
  const inherited = evaluateQaAdoption({ appRoot: ios, repoRoot: repo, app: "app-ios", inheritRepoLane: true }, deps);
  assert.deepEqual(inherited.refusals, []);
  assert.match(inherited.lane[0], /repository QA lane: @rightkit\/qa \(apps\/app-desktop\/package.json\)/);
  assert.match(inherited.warnings.join("\n"), /native iOS app with no QA lane of its own/);
  const notAsked = evaluateQaAdoption({ appRoot: ios, repoRoot: repo, app: "app-ios" }, deps);
  assert.equal(notAsked.refusals.length, 1, "other platforms never inherit");
  fsMod.rmSync(repo, { recursive: true, force: true });
});
