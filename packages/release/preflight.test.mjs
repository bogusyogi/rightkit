import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";

import {
  buildPathPrefix,
  collectPreflight,
  findWindowsSdkTool,
  formatPreflight,
  graphVendorsOpenssl,
  nextFreeVersion,
  preflightFailures,
  resolveWindowsPerl,
  PREFLIGHT_FAIL,
} from "./preflight.mjs";

test("windows SDK lookup orders versions numerically, not as strings", () => {
  const root = "C:\\Kits";
  const present = new Set([
    root,
    path.join(root, "10.0.9.0", "x64", "makeappx.exe"),
    path.join(root, "10.0.26100.0", "x64", "makeappx.exe"),
  ]);
  const found = findWindowsSdkTool("makeappx.exe", {
    sdkRoot: root,
    readdir: () => ["10.0.9.0", "10.0.26100.0"],
    exists: (candidate) => present.has(candidate),
  });
  // String sort would pick 10.0.9.0 — that is the bug this guards.
  assert.equal(found.version, "10.0.26100.0");
});

test("windows SDK lookup returns null when the tool is absent from every SDK", () => {
  const found = findWindowsSdkTool("makeappx.exe", {
    sdkRoot: "C:\\Kits",
    readdir: () => ["10.0.26100.0"],
    exists: (candidate) => candidate === "C:\\Kits",
  });
  assert.equal(found, null);
});

test("explicit OPENSSL_SRC_PERL wins over the default Strawberry location", () => {
  const resolved = resolveWindowsPerl({
    env: { OPENSSL_SRC_PERL: "D:\\perl\\bin\\perl.exe" },
    exists: () => true,
  });
  assert.equal(resolved.perlPath, "D:\\perl\\bin\\perl.exe");
  assert.equal(resolved.dir, "D:\\perl\\bin");
});

test("perl resolution reports nothing when no windows perl exists", () => {
  assert.equal(resolveWindowsPerl({ env: {}, exists: () => false }), null);
});

test("vendored openssl is detected from the cargo lock", () => {
  assert.equal(
    graphVendorsOpenssl("Cargo.lock", { exists: () => true, read: () => 'name = "openssl-src"\n' }),
    true,
  );
  assert.equal(
    graphVendorsOpenssl("Cargo.lock", { exists: () => true, read: () => 'name = "rustls"\n' }),
    false,
  );
  assert.equal(graphVendorsOpenssl("missing.lock", { exists: () => false }), false);
});

test("a sealed version collision suggests the next free patch, skipping taken ones", () => {
  const result = nextFreeVersion("/sealed", "heardright", "0.1.33", {
    exists: () => true,
    readdir: () => [
      "heardright-0.1.33-6f2f5e64",
      "heardright-0.1.34-839ad700",
      "heardright-0.1.36-f7035acc",
      "viewright-0.1.35-aaaaaaaa",
    ],
  });
  assert.equal(result.collision, true);
  // 0.1.34 is taken, 0.1.35 is free — and viewright's record must not be counted.
  assert.equal(result.suggestion, "0.1.35");
});

test("an unsealed version is reported as free", () => {
  const result = nextFreeVersion("/sealed", "heardright", "0.1.37", {
    exists: () => true,
    readdir: () => ["heardright-0.1.33-6f2f5e64"],
  });
  assert.equal(result.collision, false);
  assert.equal(result.suggestion, "0.1.37");
});

test("a sealed version collision is a hard failure carrying a concrete fix", () => {
  const checks = collectPreflight({
    platform: "linux",
    repoRoot: process.cwd(),
    app: "nothing-is-sealed-here",
    version: "9.9.9",
  });
  // No sealed dir for this app, so the version check must pass rather than throw.
  const version = checks.find((check) => check.name === "version");
  assert.equal(version.status, "ok");
});

test("formatting surfaces the fix line only for non-ok checks", () => {
  const text = formatPreflight([
    { name: "a", status: "ok", detail: "fine", fix: "unused" },
    { name: "b", status: PREFLIGHT_FAIL, detail: "broken", fix: "do the thing" },
  ]);
  assert.match(text, /\[ok {2}\] a: fine/);
  assert.doesNotMatch(text, /fix: unused/);
  assert.match(text, /fix: do the thing/);
});

test("failures are filtered from warnings so warnings never block a build", () => {
  const failures = preflightFailures([
    { name: "a", status: "warn", detail: "" },
    { name: "b", status: PREFLIGHT_FAIL, detail: "" },
  ]);
  assert.deepEqual(failures.map((check) => check.name), ["b"]);
});

test("pending ownership enrollment is a warning; a real finding still fails (#262)", () => {
  const checks = collectPreflight({
    platform: "linux",
    repoRoot: process.cwd(),
    ownershipEvidenceFactory: () => ({
      ok: true,
      enrollmentPending: true,
      warning: "WARNING: ownership approval enrollment is pending",
      result: { problems: [], violations: [], covered: [], coverage: [] },
      floor: { ok: true, problems: [] },
      evidence: { source: "enrollment-pending-advisory", trustEnrolled: false },
    }),
  });
  const ownership = checks.find((check) => check.name === "ownership");
  assert.equal(ownership.status, "warn");
  assert.match(ownership.detail, /WARNING:/);
  assert.deepEqual(preflightFailures([ownership]), []);
  const blocked = collectPreflight({
    platform: "linux",
    repoRoot: process.cwd(),
    ownershipEvidenceFactory: () => ({
      ok: false,
      enrollmentPending: true,
      warning: "WARNING: ownership approval enrollment is pending",
      result: { problems: [], violations: [{ fingerprint: "pending", file: "src.rs" }], covered: [], coverage: [] },
      floor: { ok: true, problems: [] },
      evidence: { source: "enrollment-pending-advisory", trustEnrolled: false },
    }),
  }).find((check) => check.name === "ownership");
  assert.equal(blocked.status, "fail");
});

test("build PATH prefix is windows-only", () => {
  assert.equal(buildPathPrefix({ platform: "mac" }), null);
});

test("broker-managed preflight reports receipt ownership without probing shell sccache", () => {
  let probes = 0;
  const checks = collectPreflight({
    platform: "linux",
    env: { RIGHTKIT_BUILD_BROKER_SOCKET: "broker" },
    isBrokerManagedHost: () => true,
    versionProbe: () => { probes++; return null; },
  });
  const cache = checks.find((check) => check.name === "compiler-cache");
  assert.equal(cache.status, "ok");
  assert.match(cache.detail, /broker-managed.*selection and status are recorded in build receipts/);
  assert.equal(probes, 0);
});

test("unmanaged missing-sccache warning describes only the shell PATH probe", () => {
  const checks = collectPreflight({
    platform: "linux",
    env: {},
    isBrokerManagedHost: () => false,
    versionProbe: () => null,
  });
  const cache = checks.find((check) => check.name === "sccache");
  assert.equal(cache.status, "warn");
  assert.match(cache.detail, /not found by this shell PATH probe/);
  assert.match(cache.detail, /availability is not determined/);
  assert.equal(cache.fix, null);
});

const diskBase = { platform: "mac", appRoot: process.cwd(), repoRoot: null, app: "a", version: "1.0.0", isBrokerManagedHost: () => true, versionProbe: () => "x", ownership: false, projectType: "tauri", runtimeBudgetAdoption: () => null };
const diskCheck = (checks) => checks.find((c) => c.name === "disk");

test("the disk floor measures the volume builds write to: the cargo target directory when the layout names one", () => {
  const seen = [];
  const checks = collectPreflight({ ...diskBase, nativeLayout: { targetDir: path.join(process.cwd(), "no-such-target-dir", "deeper") }, freeDisk: (p) => { seen.push(p); return 500; } });
  assert.equal(diskCheck(checks).status, "ok");
  // A target that does not exist yet is measured at its nearest existing ancestor.
  assert.deepEqual(seen, [process.cwd()]);
  const noLayout = [];
  collectPreflight({ ...diskBase, freeDisk: (p) => { noLayout.push(p); return 500; } });
  assert.deepEqual(noLayout, [process.cwd()]);
});

test("the disk floor is 25 GB by default and RIGHT_RELEASE_MIN_FREE_GB overrides it for the doctor check only", () => {
  const free = () => 17;
  assert.equal(diskCheck(collectPreflight({ ...diskBase, env: {}, freeDisk: free })).status, "fail");
  assert.match(diskCheck(collectPreflight({ ...diskBase, env: {}, freeDisk: free })).detail, /17\.0GB free on the build drive, below the 25GB floor/);
  assert.equal(diskCheck(collectPreflight({ ...diskBase, env: { RIGHT_RELEASE_MIN_FREE_GB: "10" }, freeDisk: free })).status, "ok");
  assert.equal(diskCheck(collectPreflight({ ...diskBase, env: { RIGHT_RELEASE_MIN_FREE_GB: "30" }, freeDisk: () => 17 })).status, "fail");
  assert.equal(diskCheck(collectPreflight({ ...diskBase, env: { RIGHT_RELEASE_MIN_FREE_GB: "banana" }, freeDisk: free })).status, "fail");
});
