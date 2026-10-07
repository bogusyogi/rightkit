import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { generateKeyPairSync, sign as signBytes } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { createKeyEvent, keyFingerprint, verifyKeyLog } from "./ownership-key-log.mjs";
import { prepareTrustUpdate } from "./ownership-key-trust.mjs";
import { readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { resolveKeyLogTrust } from "./ownership-approval.mjs";
import { __testOnly, main } from "./ownership-key-command.mjs";

const canonical = (v) => v === null || typeof v !== "object" ? JSON.stringify(v) : Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(",")}}`;
const nativeTestHost = process.platform === "win32" ? "windows" : process.platform === "darwin" ? "macos" : null;
const ceremonyOptions = nativeTestHost ? {} : { skip: "native ownership ceremony supports Windows & macOS only" };
const stableLegion = process.platform === "win32" && process.env.LOCALAPPDATA
  ? path.join(process.env.LOCALAPPDATA, "Orthic Labs", "Legion", "current", "bin", "legion.exe")
  : process.platform === "darwin" && process.env.HOME
    ? path.join(process.env.HOME, "Library", "Application Support", "Orthic Labs", "Legion", "current", "bin", "legion")
    : null;
const actualLegionOptions = ceremonyOptions.skip || !stableLegion || !existsSync(stableLegion)
  ? { skip: "stable installed Legion CLI is unavailable" }
  : {};
function key(platform, deviceName) {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  const keyId = keyFingerprint(publicKeyPem);
  return { privateKey: pair.privateKey, record: { keyId, publicKeyPem, platform, deviceName } };
}
function signed(draft, pair) {
  return { ...draft, signature: { keyId: pair.record.keyId, host: pair.record.platform, value: signBytes("sha256", Buffer.from(canonical(draft.payload)), pair.privateKey).toString("base64") } };
}
function setup() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-key-cli-"));
  const repo = path.join(dir, "repo"); mkdirSync(repo);
  const run = (cmd, args, opts) => spawnSync(cmd, args, { ...opts, cwd: repo, encoding: "utf8", windowsHide: true });
  assert.equal(run("git", ["init", "-q", repo]).status, 0);
  run("git", ["-C", repo, "config", "user.email", "test@example.invalid"]);
  run("git", ["-C", repo, "config", "user.name", "Test"]);
  writeFileSync(path.join(repo, "README"), "fixture\n");
  run("git", ["-C", repo, "add", "README"]); run("git", ["-C", repo, "commit", "-qm", "fixture"]);
  const root = key(nativeTestHost ?? "windows", "root");
  const active = { value: root };
  const candidate = key(nativeTestHost ?? "windows", "second device");
  let log = { schema: 1, events: [] };
  const trustHolder = { trust: null };
  const authority = (checkoutPath) => ({ repositoryIdentity: "https://example.invalid/shared-ownership", checkoutPath: path.resolve(checkoutPath) });
  const native = (args) => {
    if (args.includes("--enroll-root")) {
      const record = { ...root.record, deviceName: args[args.indexOf("--name") + 1] };
      const draft = createKeyEvent(log, { operation: "root", subject: record, signerKeyId: record.keyId });
      const event = signed(draft, root);
      writeFileSync(args[args.indexOf("--out") + 1], JSON.stringify(event));
      return { output: JSON.stringify(event), outPath: args[args.indexOf("--out") + 1] };
    }
    if (args.includes("--key-operation")) {
      const draft = JSON.parse(readFileSync(args[args.indexOf("--key-operation") + 1], "utf8"));
      return { output: JSON.stringify(signed(draft, active.value)), outPath: args[args.indexOf("--out") + 1] };
    }
    const pair = args.includes("--enroll") ? candidate : active.value;
    const prefix = pair.record.platform === "windows" ? "win" : "mac";
    return { output: JSON.stringify({ host: pair.record.platform, keyId: `${prefix}-${pair.record.keyId}`, publicKeyPem: pair.record.publicKeyPem }), outPath: args[args.indexOf("--out") + 1] };
  };
  const runner = (cmd, args, opts = {}) => {
    if (cmd === "native-helper") {
      const result = native(args);
      if (result.outPath) writeFileSync(result.outPath, result.output);
      else if (args.includes("--out")) writeFileSync(args[args.indexOf("--out") + 1], result.output);
      return { status: 0, stdout: "", stderr: "" };
    }
    if (String(cmd).endsWith("powershell.exe")) return { status: 0, stdout: "", stderr: "" };
    return run(cmd, args, opts);
  };
  const deps = {
    root: repo, runner, helper: "native-helper", print: () => {}, lockPath: path.join(dir, "ownership-command.lock"),
    authority,
    baseline: { schema: 1, minimumScannerVersion: "1.0.0", minimumCatalogVersion: "1.0.0", revokedKeyIds: [], verifiedTrackingEvidence: [], repositories: [], provenance: [], hosts: {} },
    trustLoader: () => ({ trust: trustHolder.trust }),
    elevate: (_run, requestPath, digest) => {
      const request = JSON.parse(readFileSync(requestPath, "utf8"));
      assert.equal(request.schema, 1);
      assert.deepEqual(request.authority, authority(repo));
      assert.equal(digest.length, 64);
      trustHolder.trust = prepareTrustUpdate(trustHolder.trust, request);
    },
    resolveTrust: resolveKeyLogTrust,
  };
  return { dir, repo, root, candidate, active, runner, deps, trustHolder, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}

test("first-host ceremony writes self-signed genesis, pins baseline, elevates once, then verifies", ceremonyOptions, async () => {
  const f = setup();
  try {
    let nativeCalls = 0, elevations = 0;
    const original = f.deps.runner;
    f.deps.runner = (cmd, args, opts) => { if (cmd === "native-helper") nativeCalls++; return original(cmd, args, opts); };
    const elevate = f.deps.elevate;
    f.deps.elevate = (...args) => { elevations++; return elevate(...args); };
    // The policy baseline is an explicit injected file for deterministic tests.
    const code = await main(["enroll", "--root", "--name", "Primary"], f.deps);
    assert.equal(code, 0);
    assert.equal(nativeCalls, 1); assert.equal(elevations, 1);
    const log = JSON.parse(readFileSync(path.join(f.repo, ".rightkit/ownership-keys.json"), "utf8"));
    assert.equal(verifyKeyLog(log, { rootFingerprint: f.root.record.keyId }).ok, true);
    assert.equal(f.trustHolder.trust.rootFingerprint, f.root.record.keyId);
    assert.equal(f.trustHolder.trust.keyCheckpoint.sequence, 0);
    assert.deepEqual(f.trustHolder.trust.repositories, f.deps.baseline.repositories);
    assert.deepEqual(f.trustHolder.trust.keyAuthority, f.deps.authority(f.repo));
    assert.equal(f.runner("git", ["-C", f.repo, "remote", "add", "origin", f.trustHolder.trust.keyAuthority.repositoryIdentity]).status, 0);
    const committed = readCommittedKeyAuthority(f.trustHolder.trust);
    assert.equal(committed.ok, true, committed.problems?.join("; "));
    assert.equal(committed.rootFingerprint, f.root.record.keyId);
    assert.equal(committed.keyCheckpoint.sequence, 0);
    assert.equal(committed.authority.repositoryIdentity, f.trustHolder.trust.keyAuthority.repositoryIdentity);
  } finally { f.cleanup(); }
});

test("invalid fingerprint stops before native authorization, metadata, or elevation", ceremonyOptions, async () => {
  const f = setup();
  try {
    let nativeCalls = 0, elevations = 0;
    f.deps.runner = (cmd, args, opts) => { if (cmd === "native-helper") nativeCalls++; return f.runner(cmd, args, opts); };
    f.deps.elevate = () => { elevations++; };
    const code = await main(["enroll", "--root-fingerprint", "NOPE"], f.deps);
    assert.equal(code, 1); assert.equal(nativeCalls, 0); assert.equal(elevations, 0);
    assert.equal(readFileSync(path.join(f.repo, "README"), "utf8"), "fixture\n");
    assert.equal(f.runner("git", ["-C", f.repo, "status", "--porcelain", "--", ".rightkit"]).stdout.trim(), "");
  } finally { f.cleanup(); }
});

test("native cancellation is attempted once and leaves no committed metadata or trust update", ceremonyOptions, async () => {
  const f = setup();
  try {
    let nativeCalls = 0, elevations = 0;
    f.deps.runner = (cmd, args, opts) => {
      if (cmd === "native-helper") { nativeCalls++; return { status: 19, stdout: "", stderr: "cancelled" }; }
      return f.runner(cmd, args, opts);
    };
    f.deps.elevate = () => { elevations++; };
    assert.equal(await main(["enroll", "--root"], f.deps), 1);
    assert.equal(nativeCalls, 1); assert.equal(elevations, 0);
    assert.equal(f.trustHolder.trust, null);
    assert.equal(f.runner("git", ["-C", f.repo, "status", "--porcelain", "--untracked-files=all"]).stdout.trim(), "");
  } finally { f.cleanup(); }
});

test("failed first-host elevation resumes from committed genesis with key discovery only", ceremonyOptions, async () => {
  const f = setup();
  try {
    let authCalls = 0, elevations = 0;
    const runner = f.deps.runner;
    f.deps.runner = (cmd, args, opts) => {
      if (cmd === "native-helper" && args.includes("--enroll-root")) authCalls++;
      return runner(cmd, args, opts);
    };
    f.deps.elevate = () => { elevations++; throw new Error("UAC cancelled"); };
    assert.equal(await main(["enroll", "--root"], f.deps), 1);
    assert.equal(authCalls, 1); assert.equal(elevations, 1);
    const headWithGenesis = f.runner("git", ["-C", f.repo, "rev-parse", "HEAD"]).stdout.trim();
    const firstLog = readFileSync(path.join(f.repo, ".rightkit/ownership-keys.json"), "utf8");
    f.deps.elevate = (_run, requestPath, digest) => {
      elevations++;
      const request = JSON.parse(readFileSync(requestPath, "utf8"));
      f.trustHolder.trust = prepareTrustUpdate(null, request);
    };
    assert.equal(await main(["enroll", "--root"], f.deps), 0);
    assert.equal(authCalls, 1); assert.equal(elevations, 2);
    assert.equal(f.runner("git", ["-C", f.repo, "rev-parse", "HEAD"]).stdout.trim(), headWithGenesis);
    assert.equal(readFileSync(path.join(f.repo, ".rightkit/ownership-keys.json"), "utf8"), firstLog);
    assert.equal(f.trustHolder.trust.rootFingerprint, f.root.record.keyId);
  } finally { f.cleanup(); }
});

test("failed additional-device elevation resumes matching committed request without another enrollment", ceremonyOptions, async () => {
  const f = setup();
  try {
    assert.equal(await main(["enroll", "--root"], f.deps), 0);
    const peerRepo = path.join(f.dir, "peer");
    assert.equal(f.runner("git", ["clone", "--no-hardlinks", f.repo, peerRepo]).status, 0);
    f.runner("git", ["-C", peerRepo, "remote", "remove", "origin"]);
    f.runner("git", ["-C", peerRepo, "config", "user.email", "test@example.invalid"]);
    f.runner("git", ["-C", peerRepo, "config", "user.name", "Test"]);
    const peerTrust = { trust: null };
    const peerDeps = { ...f.deps, root: peerRepo, trustLoader: () => peerTrust };
    let enrollCalls = 0, elevations = 0;
    const runner = peerDeps.runner;
    peerDeps.runner = (cmd, args, opts) => {
      if (cmd === "native-helper" && args.includes("--enroll")) enrollCalls++;
      return runner(cmd, args, opts);
    };
    peerDeps.elevate = () => { elevations++; throw new Error("UAC cancelled"); };
    f.active.value = f.candidate;
    const pin = f.root.record.keyId;
    assert.equal(await main(["enroll", "--name", "Peer", "--root-fingerprint", pin], peerDeps), 1);
    assert.equal(enrollCalls, 1); assert.equal(elevations, 1);
    const headWithRequest = f.runner("git", ["-C", peerRepo, "rev-parse", "HEAD"]).stdout.trim();
    const reqFile = path.join(peerRepo, ".rightkit", "key-requests", `${f.candidate.record.keyId}.json`);
    assert.equal(JSON.parse(readFileSync(reqFile, "utf8")).key.keyId, f.candidate.record.keyId);
    const discoverRunner = peerDeps.runner;
    peerDeps.runner = (cmd, args, opts) => cmd === "native-helper" && args.includes("--public-key")
      ? { status: 8, stdout: "", stderr: "unavailable" }
      : discoverRunner(cmd, args, opts);
    assert.equal(await main(["enroll", "--name", "Peer", "--root-fingerprint", pin], peerDeps), 1);
    assert.equal(enrollCalls, 1); assert.equal(elevations, 1);
    peerDeps.runner = discoverRunner;
    peerDeps.elevate = (_run, requestPath, digest) => {
      elevations++;
      const request = JSON.parse(readFileSync(requestPath, "utf8"));
      peerTrust.trust = prepareTrustUpdate(null, request);
    };
    assert.equal(await main(["enroll", "--name", "Peer", "--root-fingerprint", pin], peerDeps), 0);
    assert.equal(enrollCalls, 1); assert.equal(elevations, 2);
    assert.equal(f.runner("git", ["-C", peerRepo, "rev-parse", "HEAD"]).stdout.trim(), headWithRequest);
    assert.equal(peerTrust.trust.rootFingerprint, pin);
  } finally { f.cleanup(); }
});

test("elevated Windows launch has one PowerShell child and safely quotes paths", () => {
  const call = __testOnly.buildWindowsElevationInvocation("C:\\Program Files\\node.exe", "D:\\RightKit's files\\ownership-key-trust.mjs", "D:\\repo with spaces\\request.json", "a".repeat(64));
  assert.match(call.childScript, /^& 'C:\\Program Files\\node\.exe' /);
  assert.match(call.childScript, /RightKit''s files/);
  assert.match(call.childScript, /repo with spaces/);
  assert.equal((call.childScript.match(/node\.exe/g) ?? []).length, 1);
  assert.equal(call.parentExecutable, "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
  assert.equal(call.parentArgs[1], "-EncodedCommand");
});

test("reparse metadata parents fail before native authorization or external writes", ceremonyOptions, async (t) => {
  const f = setup();
  try {
    const outside = path.join(f.dir, "outside"); mkdirSync(outside);
    mkdirSync(path.join(f.repo, ".rightkit"));
    const metadata = path.join(f.repo, ".rightkit", "key-requests");
    try { symlinkSync(outside, metadata, process.platform === "win32" ? "junction" : "dir"); }
    catch { t.skip("directory symlink creation unavailable"); return; }
    let nativeCalls = 0, elevations = 0;
    const runner = f.deps.runner;
    f.deps.runner = (cmd, args, opts) => { if (cmd === "native-helper") nativeCalls++; return runner(cmd, args, opts); };
    f.deps.elevate = () => { elevations++; };
    assert.equal(await main(["enroll", "--root"], f.deps), 1);
    assert.equal(nativeCalls, 0); assert.equal(elevations, 0);
    assert.deepEqual(readdirSync(outside), []);
  } finally { f.cleanup(); }
});

test("second-device request and active-host approval each use one signing ceremony and advance checkpoint", ceremonyOptions, async () => {
  const f = setup();
  try {
    assert.equal(await main(["enroll", "--root"], f.deps), 0);
    let nativeAuth = 0, elevations = 0;
    const runner = f.deps.runner;
    f.deps.runner = (cmd, args, opts) => { if (cmd === "native-helper" && (args.includes("--enroll") || args.includes("--key-operation"))) nativeAuth++; return runner(cmd, args, opts); };
    const elevate = f.deps.elevate;
    f.deps.elevate = (...args) => { elevations++; return elevate(...args); };
    f.active.value = f.candidate;
    assert.equal(await main(["enroll", "--name", "Second", "--root-fingerprint", f.root.record.keyId], f.deps), 0);
    assert.equal(nativeAuth, 1); assert.equal(elevations, 1);
    const requestPath = path.join(f.repo, ".rightkit/key-requests", `${f.candidate.record.keyId}.json`);
    assert.equal(JSON.parse(readFileSync(requestPath, "utf8")).key.keyId, f.candidate.record.keyId);
    assert.equal(f.trustHolder.trust.keyCheckpoint.sequence, 0);
    f.active.value = f.root;
    assert.equal(await main(["approve-key", f.candidate.record.keyId], f.deps), 0);
    assert.equal(nativeAuth, 2); assert.equal(elevations, 2);
    assert.equal(await main(["revoke-key", f.candidate.record.keyId], f.deps), 0);
    assert.equal(nativeAuth, 3); assert.equal(elevations, 3);
    const finalLog = JSON.parse(readFileSync(path.join(f.repo, ".rightkit/ownership-keys.json"), "utf8"));
    const finalState = verifyKeyLog(finalLog, { rootFingerprint: f.root.record.keyId });
    assert.equal(finalState.keys.some((k) => k.keyId === f.candidate.record.keyId), false);
    assert.deepEqual(finalState.revokedKeyIds, [f.candidate.record.keyId]);
    assert.equal(f.trustHolder.trust.keyCheckpoint.sequence, 2);
  } finally { f.cleanup(); }
});

test("configured origin syncs peer request before approval and pushes each exact metadata commit", ceremonyOptions, async () => {
  const f = setup();
  try {
    let boundedNetworkCalls = 0;
    const originalRunner = f.deps.runner;
    f.deps.runner = (cmd, args, opts) => {
      if (cmd === "git" && (args.includes("fetch") || args.includes("push"))) {
        boundedNetworkCalls++;
        assert.equal(opts.timeout, 30_000);
        assert.equal(opts.env.GIT_TERMINAL_PROMPT, "0");
        assert.match(opts.env.GIT_SSH_COMMAND, /BatchMode=yes/);
      }
      return originalRunner(cmd, args, opts);
    };
    const bare = path.join(f.dir, "remote.git");
    assert.equal(f.runner("git", ["init", "--bare", "--initial-branch=main", bare]).status, 0);
    assert.equal(f.runner("git", ["-C", f.repo, "branch", "-M", "main"]).status, 0);
    assert.equal(f.runner("git", ["-C", f.repo, "remote", "add", "origin", bare]).status, 0);
    assert.equal(f.runner("git", ["-C", f.repo, "push", "-u", "origin", "main"]).status, 0);
    assert.equal(await main(["enroll", "--root"], f.deps), 0);

    const secondRepo = path.join(f.dir, "second");
    assert.equal(f.runner("git", ["clone", bare, secondRepo]).status, 0);
    f.runner("git", ["-C", secondRepo, "config", "user.email", "test@example.invalid"]);
    f.runner("git", ["-C", secondRepo, "config", "user.name", "Test"]);
    const secondTrust = { trust: prepareTrustUpdate(null, {
      log: JSON.parse(readFileSync(path.join(secondRepo, ".rightkit/ownership-keys.json"), "utf8")),
      rootFingerprint: f.root.record.keyId,
      baseline: f.deps.baseline,
      authority: f.deps.authority(secondRepo),
    }) };
    const secondDeps = { ...f.deps, root: secondRepo, trustLoader: () => secondTrust };
    secondDeps.elevate = (_run, requestPath, digest) => {
      const request = JSON.parse(readFileSync(requestPath, "utf8"));
      assert.equal(digest.length, 64);
      secondTrust.trust = prepareTrustUpdate(secondTrust.trust, request);
    };
    f.active.value = f.candidate;
    assert.equal(await main(["enroll", "--name", "Peer", "--root-fingerprint", f.root.record.keyId], secondDeps), 0);
    assert.equal(readFileSync(path.join(bare, "HEAD"), "utf8").trim(), "ref: refs/heads/main");

    f.active.value = f.root;
    assert.equal(await main(["approve-key", f.candidate.record.keyId], f.deps), 0);
    const remotelyApproved = JSON.parse(readFileSync(path.join(f.repo, ".rightkit/ownership-keys.json"), "utf8"));
    assert.equal(verifyKeyLog(remotelyApproved, { rootFingerprint: f.root.record.keyId }).keys.length, 2);

    // This command first fast-forwards peer clone, proving request and approval shared through ordinary Git.
    assert.equal(await main(["revoke-key", f.candidate.record.keyId], secondDeps), 0);
    const peerLog = JSON.parse(readFileSync(path.join(secondRepo, ".rightkit/ownership-keys.json"), "utf8"));
    assert.equal(verifyKeyLog(peerLog, { rootFingerprint: f.root.record.keyId }).revokedKeyIds.includes(f.candidate.record.keyId), true);
    assert.ok(boundedNetworkCalls >= 6);
  } finally { f.cleanup(); }
});

test("installed Legion pre-commit hook receives isolated candidate tree and preserves unrelated staged files", ceremonyOptions, async () => {
  const f = setup();
  try {
    const hookFile = path.join(f.repo, ".git", "hooks", "pre-commit");
    writeFileSync(hookFile, "#!/bin/sh\n# legion minimize commit verify .audit/minimize/commit-receipt.json\nexit 0\n", { mode: 0o755 });
    assert.equal(f.runner("git", ["-C", f.repo, "config", "core.hooksPath", path.join(f.repo, ".git", "hooks")]).status, 0);
    const resolvedHook = f.runner("git", ["-C", f.repo, "rev-parse", "--git-path", "hooks/pre-commit"]);
    assert.equal(resolvedHook.status, 0);
    assert.match(readFileSync(path.resolve(f.repo, resolvedHook.stdout.trim()), "utf8"), /minimize commit verify/);
    assert.deepEqual(__testOnly.detectMinimizeHook(f.repo, f.deps.runner, { legionPath: "mock-installed-legion" }), { legion: "mock-installed-legion" });
    writeFileSync(path.join(f.repo, "unrelated.txt"), "keep staged\n");
    assert.equal(f.runner("git", ["-C", f.repo, "add", "unrelated.txt"]).status, 0);
    const invoked = [];
    const original = f.deps.runner;
    f.deps.legionPath = "mock-installed-legion";
    f.deps.runner = (cmd, args, opts = {}) => {
      if (cmd === "mock-installed-legion") {
        invoked.push(args);
        const env = opts.env;
        assert.ok(env?.GIT_INDEX_FILE);
        const listed = spawnSync("git", ["-C", f.repo, "ls-files"], { encoding: "utf8", env });
        assert.equal(listed.status, 0);
        assert.match(listed.stdout, /\.rightkit\/ownership-keys\.json/);
        assert.doesNotMatch(listed.stdout, /unrelated\.txt/);
        return { status: 0, stdout: "", stderr: "" };
      }
      if (cmd === "git" && args.includes("commit")) {
        const staged = spawnSync("git", ["-C", f.repo, "diff", "--cached", "--name-only"], { encoding: "utf8" });
        assert.doesNotMatch(staged.stdout, /ownership-keys\.json/);
      }
      return original(cmd, args, opts);
    };
    assert.equal(await main(["enroll", "--root"], f.deps), 0);
    assert.equal(invoked.length, 2);
    assert.match(invoked[0][3], /commit-review\.json$/);
    assert.match(invoked[1][4], /commit-receipt\.json$/);
    const staged = f.runner("git", ["-C", f.repo, "diff", "--cached", "--name-only"]).stdout;
    assert.match(staged, /unrelated\.txt/);
    assert.doesNotMatch(staged, /ownership-keys\.json/);
    assert.equal(existsSync(path.join(f.repo, ".audit", "minimize", "commit-receipt.json")), false);
  } finally { f.cleanup(); }
});

test("installed Legion review, receipt, and real pre-commit verification accept ownership metadata", actualLegionOptions, async () => {
  const f = setup();
  try {
    const hookFile = path.join(f.repo, ".git", "hooks", "pre-commit");
    // Git Bash on Windows needs a POSIX path (cygpath); macOS and Linux use the path as is.
    const quotedLegion = `'${stableLegion.replaceAll("\\", "/").replaceAll("'", "'\\''")}'`;
    const gitBashLegion = process.platform === "win32" ? `$(cygpath -u ${quotedLegion})` : quotedLegion;
    writeFileSync(hookFile, `#!/bin/sh\nlegion=${gitBashLegion}\nexec "$legion" minimize commit verify "$PWD/.audit/minimize/commit-receipt.json"\n`, { mode: 0o755 });
    assert.equal(f.runner("git", ["-C", f.repo, "config", "core.hooksPath", path.join(f.repo, ".git", "hooks")]).status, 0);
    writeFileSync(path.join(f.repo, "unrelated.txt"), "preserved staged content\n");
    assert.equal(f.runner("git", ["-C", f.repo, "add", "unrelated.txt"]).status, 0);
    f.deps.legionPath = stableLegion;
    let authCalls = 0, elevations = 0;
    const runner = f.deps.runner;
    f.deps.runner = (cmd, args, opts) => { if (cmd === "native-helper") authCalls++; return runner(cmd, args, opts); };
    const elevate = f.deps.elevate;
    f.deps.elevate = (...args) => { elevations++; return elevate(...args); };
    const messages = [];
    f.deps.print = (message) => messages.push(message);
    assert.equal(await main(["enroll", "--root"], f.deps), 0, messages.join("\n"));
    assert.equal(authCalls, 1);
    assert.equal(elevations, 1);
    const staged = f.runner("git", ["-C", f.repo, "diff", "--cached", "--name-only"]).stdout;
    assert.match(staged, /unrelated\.txt/);
    assert.doesNotMatch(staged, /ownership-keys\.json/);
    assert.equal(f.runner("git", ["-C", f.repo, "status", "--porcelain", "--", ".rightkit"]).stdout.trim(), "");
  } finally { f.cleanup(); }
});
