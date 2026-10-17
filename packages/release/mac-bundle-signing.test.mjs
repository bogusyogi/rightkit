import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { bundleRelativePath, expandMacSigningEntries, globToRegExp, lookupBundlePath, compileBundlePathMap, nestedSigningFlags, resolveBundleIdentifier, resolveNestedSigningSettings } from "./mac-bundle-signing.mjs";

const APP = path.join(path.sep, "work", "Demo.app");
const at = (relative) => path.join(APP, ...relative.split("/"));
const NESTED = ["Contents/MacOS/demo-engine", "Contents/Resources/runtime/yt-dlp", "Contents/Frameworks/libonnxruntime.1.2.dylib", "Contents/Library/LoginItems/Helper.app"].map(at);
const exists = () => true;
const settingsFor = (sign, extra = {}) => resolveNestedSigningSettings(NESTED, { app: APP, sign, bundleId: "app.demo", root: path.join(path.sep, "work"), exists, ...extra });

test("globs: * stays in one segment, ** crosses segments, ? is one character", () => {
  assert.ok(globToRegExp("Contents/Frameworks/libonnxruntime*.dylib").test("Contents/Frameworks/libonnxruntime.1.2.dylib"));
  assert.ok(!globToRegExp("Contents/*.dylib").test("Contents/Frameworks/x.dylib"));
  assert.ok(globToRegExp("Contents/**/x.dylib").test("Contents/x.dylib"));
  assert.ok(globToRegExp("Contents/**/x.dylib").test("Contents/a/b/x.dylib"));
  assert.ok(globToRegExp("Contents/Resources/**").test("Contents/Resources/runtime/yt-dlp"));
  assert.ok(globToRegExp("Contents/MacOS/engine?").test("Contents/MacOS/engine2"));
  assert.ok(!globToRegExp("a.b").test("axb"), "regex metacharacters are literal");
});

test("lookup prefers an exact key, then the first matching glob, else undefined", () => {
  const reject = (message) => { throw new Error(message); };
  const entries = compileBundlePathMap({ "Contents/MacOS/*": "glob", "Contents/MacOS/engine": "exact" }, "m", { reject });
  assert.equal(lookupBundlePath(entries, "Contents/MacOS/engine"), "exact");
  assert.equal(lookupBundlePath(entries, "Contents/MacOS/other"), "glob");
  assert.equal(lookupBundlePath(entries, "Contents/Resources/x"), undefined);
  assert.equal(bundleRelativePath(APP, at("Contents/MacOS/engine")), "Contents/MacOS/engine");
  assert.equal(bundleRelativePath(APP, APP), "");
});

test("identifier template applies only to plain files and sanitizes <relpath>", () => {
  const args = { identifiers: [], template: "<bundleId>.runtime.<relpath>", bundleId: "app.demo" };
  assert.equal(resolveBundleIdentifier({ ...args, relative: "Contents/Resources/runtime/yt dlp", plain: true }), "app.demo.runtime.Contents.Resources.runtime.yt.dlp");
  assert.equal(resolveBundleIdentifier({ ...args, relative: "Contents/Library/LoginItems/Helper.app", plain: false }), undefined);
  assert.equal(resolveBundleIdentifier({ ...args, relative: "", plain: true }), undefined);
  assert.throws(() => resolveBundleIdentifier({ ...args, bundleId: undefined, relative: "Contents/x", plain: true }), /bundle identifier is unknown/);
});

test("release nested code: nothing declared keeps today's behaviour (empty settings, no entitlements)", () => {
  assert.equal(settingsFor({}).size, 0);
  assert.equal(settingsFor({ entitlements: "release/app.entitlements.plist" }).size, 0, "a string sign.entitlements is the app file, never applied to nested code");
  assert.deepEqual(nestedSigningFlags(undefined), []);
});

test("release nested code: sign.entitlements per path gives the engine and yt-dlp their entitlements and nothing else", () => {
  const settings = settingsFor({ entitlements: { "Contents/MacOS/demo-engine": "src-tauri/entitlements.engine.plist", "Contents/Resources/runtime/yt-dlp": "src-tauri/entitlements.engine.plist" } });
  const engine = path.resolve(path.join(path.sep, "work"), "src-tauri/entitlements.engine.plist");
  assert.deepEqual(settings.get(at("Contents/MacOS/demo-engine")), { entitlements: engine });
  assert.deepEqual(settings.get(at("Contents/Resources/runtime/yt-dlp")), { entitlements: engine });
  assert.equal(settings.has(at("Contents/Frameworks/libonnxruntime.1.2.dylib")), false);
  assert.deepEqual(nestedSigningFlags(settings.get(at("Contents/MacOS/demo-engine"))), ["--entitlements", engine]);
});

test("release nested code: sign.identifiers and sign.identifierTemplate name nested code", () => {
  const settings = settingsFor({ identifiers: { "Contents/MacOS/demo-engine": "app.demo.engine" }, identifierTemplate: "<bundleId>.runtime.<relpath>" });
  assert.deepEqual(settings.get(at("Contents/MacOS/demo-engine")), { identifier: "app.demo.engine" });
  assert.deepEqual(settings.get(at("Contents/Resources/runtime/yt-dlp")), { identifier: "app.demo.runtime.Contents.Resources.runtime.yt-dlp" });
  assert.deepEqual(settings.get(at("Contents/Frameworks/libonnxruntime.1.2.dylib")), { identifier: "app.demo.runtime.Contents.Frameworks.libonnxruntime.1.2.dylib" });
  assert.equal(settings.has(at("Contents/Library/LoginItems/Helper.app")), false, "bundles are not named by the template");
  assert.deepEqual(nestedSigningFlags({ identifier: "a.b", entitlements: "/e.plist" }), ["--identifier", "a.b", "--entitlements", "/e.plist"]);
});

test("release nested code: malformed or missing declarations are refused", () => {
  const code = (error) => error.code === "MAC_SIGN_ENTRY_INVALID";
  assert.throws(() => settingsFor({ identifiers: { "Contents/MacOS/x": "-bad" } }), code);
  assert.throws(() => settingsFor({ identifiers: { "../x": "app.x" } }), code);
  assert.throws(() => settingsFor({ identifierTemplate: "<bundleId>.<nope>" }), code);
  assert.throws(() => settingsFor({ entitlements: { "Contents/MacOS/x": 5 } }), code);
  assert.throws(() => settingsFor({ entitlements: { "Contents/MacOS/x": "missing.plist" } }, { exists: () => false }), /not found/);
});

test("expandMacSigningEntries still rejects directories that are not .app bundles", () => {
  assert.throws(() => expandMacSigningEntries(["/x/dir"], { isDirectory: () => true }), /not a \.app bundle/);
});
