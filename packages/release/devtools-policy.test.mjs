import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  CONTENT_ONLY_MARKER, declaredDevtools, devtoolsEvidence, manifestSelectsContentDevtools, readContentOnlyApps, resolveDevtoolsAllowance, sealedManifestAllowsContentDevtools,
} from "./devtools-policy.mjs";
import { assertNoForbiddenBuildFeatures, cargoManifestIssues, commandFeatureIssues, isForbiddenFeature } from "./qa-build-features.mjs";
import { assertNoQaControl, bufferQaFindings } from "./qa-control-scan.mjs";

const SHELL_CONTENT = '[package]\nname = "app"\n[features]\ndefault = []\n[dependencies]\nrightkit-shell = { version = "0.4", features = ["content-devtools"] }\ntauri = "2"\n';
const SHELL_PLAIN = '[package]\nname = "app"\n[features]\ndefault = []\n[dependencies]\nrightkit-shell = "0.4"\ntauri = "2"\n';
const BUILD_RS_ACL = 'fn main() { tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&["open_document"]))).unwrap(); }';

function app({ manifest = SHELL_CONTENT, buildRs = BUILD_RS_ACL } = {}) {
  const root = mkdtempSync(join(tmpdir(), "devtools-policy-"));
  mkdirSync(join(root, "src-tauri"), { recursive: true });
  writeFileSync(join(root, "src-tauri", "Cargo.toml"), manifest);
  if (buildRs !== null) writeFileSync(join(root, "src-tauri", "build.rs"), buildRs);
  return root;
}

test("the allow-list file names exactly one app: viewright", () => {
  assert.deepEqual([...readContentOnlyApps()], ["viewright"]);
});

test("the declaration is denied by default and only knows two values", () => {
  assert.equal(declaredDevtools({}), "denied");
  assert.equal(declaredDevtools({ devtools: "denied" }), "denied");
  assert.equal(declaredDevtools({ devtools: "content-only" }), "content-only");
  for (const bad of [true, "on", "debug", "Content-Only", 1]) assert.throws(() => declaredDevtools({ devtools: bad }), /devtools must be/);
});

test("viewright declaring content-only with the shell feature and an ACL manifest is allowed and recorded", () => {
  const allowance = resolveDevtoolsAllowance({ config: { app: "viewright", devtools: "content-only" }, root: app() });
  assert.equal(allowance.mode, "content-only");
  assert.equal(allowance.shellFeature, "rightkit-shell/content-devtools");
  const evidence = devtoolsEvidence(allowance);
  assert.equal(evidence.mode, "content-only");
  assert.equal(evidence.app, "viewright");
  assert.match(evidence.evidence, /allow-listed/);
  assert.deepEqual(devtoolsEvidence({ mode: "denied" }), { mode: "denied" });
});

test("any other app declaring content-only is refused, whatever else is true", () => {
  for (const name of ["heardright", "mailright", "genright", "cutright", "postright", "scraperight", "pulse", "coderight", "viewright-fork", undefined]) {
    assert.throws(
      () => resolveDevtoolsAllowance({ config: { app: name, devtools: "content-only" }, root: app() }),
      /not on the content-only DevTools allow-list/,
      String(name),
    );
  }
});

test("an app on a different allow-list entry is refused unless the list says so", () => {
  const allowance = resolveDevtoolsAllowance({ config: { app: "other", devtools: "content-only" }, root: app(), contentOnlyApps: ["other"] });
  assert.equal(allowance.mode, "content-only");
});

test("content-only without the shell feature or without the ACL manifest is refused", () => {
  assert.throws(() => resolveDevtoolsAllowance({ config: { app: "viewright", devtools: "content-only" }, root: app({ manifest: SHELL_PLAIN }) }), /does not depend on rightkit-tauri-shell with the `content-devtools` feature/);
  assert.throws(() => resolveDevtoolsAllowance({ config: { app: "viewright", devtools: "content-only" }, root: app({ buildRs: "fn main() { tauri_build::build() }" }) }), /no application ACL manifest/);
  assert.throws(() => resolveDevtoolsAllowance({ config: { app: "viewright", devtools: "content-only" }, root: app({ buildRs: null }) }), /no application ACL manifest/);
});

test("turning content-devtools on without the declaration is refused", () => {
  assert.throws(() => resolveDevtoolsAllowance({ config: { app: "viewright" }, root: app() }), /does not declare devtools: "content-only"/);
  assert.deepEqual(resolveDevtoolsAllowance({ config: { app: "viewright" }, root: app({ manifest: SHELL_PLAIN }) }), { mode: "denied" });
  const viaFeature = '[features]\ndefault = []\ncontent-devtools = ["rightkit-shell/content-devtools"]\n[dependencies]\nrightkit-shell = "0.4"\n';
  assert.equal(manifestSelectsContentDevtools(viaFeature), true);
  assert.equal(manifestSelectsContentDevtools(SHELL_PLAIN), false);
});

test("build features: devtools stays refused for everyone, accepted for the allow-listed app", () => {
  for (const f of ["devtools", "content-devtools", "rightkit-shell/content-devtools", "tauri/devtools"]) {
    assert.equal(isForbiddenFeature(f), true, f);
    assert.equal(isForbiddenFeature(f, { allowDevtools: true }), false, f);
  }
  for (const f of ["qa-native", "qa-control", "dev-native", "dep:tauri-plugin-wdio-webdriver"]) assert.equal(isForbiddenFeature(f, { allowDevtools: true }), true, f);
  const command = { cmd: "cargo", args: ["build", "--release", "--features", "custom-protocol,content-devtools"] };
  assert.notDeepEqual(commandFeatureIssues(command, { label: "prePackage" }), []);
  assert.deepEqual(commandFeatureIssues(command, { label: "prePackage", allowDevtools: true }), []);
  assert.notDeepEqual(commandFeatureIssues({ cmd: "cargo", args: ["build", "--all-features"] }, { label: "x", allowDevtools: true }), []);
  assert.notDeepEqual(commandFeatureIssues({ cmd: "cargo", args: ["build", "--features", "qa-native,content-devtools"] }, { label: "x", allowDevtools: true }), []);
  const tauriDevtools = '[features]\ndefault = []\n[dependencies]\ntauri = { version = "2", features = ["devtools"] }\n';
  assert.notDeepEqual(cargoManifestIssues(tauriDevtools), []);
  assert.deepEqual(cargoManifestIssues(tauriDevtools, "Cargo.toml", { allowDevtools: true }), []);
  const defaultReach = '[features]\ndefault = ["content-devtools"]\ncontent-devtools = ["rightkit-shell/content-devtools"]\n[dependencies]\nrightkit-shell = "0.4"\n';
  assert.notDeepEqual(cargoManifestIssues(defaultReach), []);
});

test("assertNoForbiddenBuildFeatures: manifest enabling content-devtools in default features is refused without the allowance", () => {
  const root = app({ manifest: '[features]\ndefault = ["tauri/devtools"]\n[dependencies]\ntauri = "2"\n' });
  assert.throws(() => assertNoForbiddenBuildFeatures({ root, commands: [] }), /reaches `tauri\/devtools`/);
  assert.doesNotThrow(() => assertNoForbiddenBuildFeatures({ root, commands: [], devtools: { mode: "content-only" } }));
});

const MACHO = Buffer.from([0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01]);
const withText = (...texts) => Buffer.concat([MACHO, ...texts.map((t) => Buffer.concat([Buffer.from("\0"), Buffer.from(t, "latin1"), Buffer.from("\0")]))]);
const TAURI = "plugin:webview|internal_toggle_devtools";

test("binary scan: tauri devtools needs the allowance AND the shell's content-only marker", () => {
  const plain = withText(TAURI);
  const marked = withText(TAURI, CONTENT_ONLY_MARKER);
  assert.deepEqual(bufferQaFindings(plain).map((f) => f.rule), ["tauri-devtools"]);
  assert.deepEqual(bufferQaFindings(marked).map((f) => f.rule), ["tauri-devtools"]);
  assert.deepEqual(bufferQaFindings(plain, { allowContentOnlyDevtools: true }).map((f) => f.rule), ["tauri-devtools"]);
  assert.deepEqual(bufferQaFindings(marked, { allowContentOnlyDevtools: true }), []);
  // The allowance never covers the other surfaces.
  assert.deepEqual(bufferQaFindings(withText(TAURI, CONTENT_ONLY_MARKER, "RIGHTKIT-QA-CONTROL-SERVER-PRESENT"), { allowContentOnlyDevtools: true }).map((f) => f.rule), ["qa-control-server"]);
});

test("binary scan on disk: the allowance reaches assertNoQaControl", () => {
  const dir = mkdtempSync(join(tmpdir(), "devtools-scan-"));
  const file = join(dir, "app");
  writeFileSync(file, withText(TAURI, CONTENT_ONLY_MARKER));
  assert.throws(() => assertNoQaControl([file], "release"), /tauri-devtools|Tauri devtools/);
  assert.doesNotThrow(() => assertNoQaControl([file], "release", { allowContentOnlyDevtools: true }));
});

test("sealed manifest: the allowance is honoured only for an allow-listed app that recorded it", () => {
  const manifest = { app: "viewright", devtools: { mode: "content-only", app: "viewright" } };
  assert.equal(sealedManifestAllowsContentDevtools(manifest), true);
  assert.equal(sealedManifestAllowsContentDevtools({ app: "mailright", devtools: { mode: "content-only", app: "mailright" } }), false);
  assert.equal(sealedManifestAllowsContentDevtools({ app: "viewright", devtools: { mode: "denied" } }), false);
  assert.equal(sealedManifestAllowsContentDevtools({ app: "viewright" }), false);
  assert.equal(sealedManifestAllowsContentDevtools({ app: "viewright", devtools: { mode: "content-only", app: "mailright" } }), false);
});

test("the renamed rightkit-tauri-shell crate is covered the same as the old rightkit-shell name", () => {
  const renamed = '[package]\nname = "app"\n[features]\ndefault = []\ncontent-devtools = ["rightkit-tauri-shell/content-devtools"]\n[dependencies]\nrightkit-tauri-shell = "0.4"\n';
  assert.equal(manifestSelectsContentDevtools(renamed), true);
  assert.equal(manifestSelectsContentDevtools('[package]\nname = "app"\n[dependencies]\nrightkit-tauri-shell = { version = "0.4", features = ["content-devtools"] }\n'), true);
  assert.equal(manifestSelectsContentDevtools('[package]\nname = "app"\n[dependencies]\nrightkit-tauri-shell = "0.4"\n'), false);
});
