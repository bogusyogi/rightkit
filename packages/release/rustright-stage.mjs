#!/usr/bin/env node
// `right-release rustright-stage [--repo <rustright checkout>] [--scratch <dir>] [--cutover] [--apply]` (rightkit#1465, rustright#125).
//
// RustRight's two stage scripts (tools/install/mac-local-stage.sh, tools/install/xps-stage.ps1) folded into right-release: one tool takes the broker, client and
// installer binaries from the gate's compile output for HEAD, signs them with RightKit's own signing, stages and approves them, writes the cutover script, and
// calls `rustright-install cutover` ONLY for the swap. It never runs Cargo and never compiles: the bytes it signs are the bytes `rightkit-gate --to compile`
// built for the commit at HEAD, proved by RustRight's own `tools/install/gate-output.mjs` (read here, never edited) against the gate's receipt.
//
// Order (both hosts): HEAD commit (the checkout must be clean on macOS) -> gate-output proves and copies the three binaries -> their digests are compared with the
// gate's -> macOS: `rustright-install expected-identity`, `rustright-install bundle`, helpers copied in, digests compared again, Developer ID codesign through
// RightKit's signing session, designated-requirement and identifier/team checks; Windows: Authenticode through sign-windows.mjs and a Valid / Damned Ventures
// LLC check -> `rustright-install stage` -> `rustright-install approve` -> the cutover script is written -> with `--cutover`, `rustright-install cutover` runs
// (a dry run that prints the `drain:` line unless `--apply`).
//
// Not done here, and why: the root-owned adapter install and Legion enrollment on the Mac need elevation, which is Adrian's host step (rustright#119); this tool
// runs unelevated. Typed PIPELINE admission stays under rightkit#824.
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, readFileSync, rmSync, copyFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";

export class RustrightStageError extends Error {
  constructor(message) { super(message); this.name = "RustrightStageError"; this.code = "RK_RUSTRIGHT_STAGE"; }
}
const fail = (message) => { throw new RustrightStageError(message); };
const HERE = path.dirname(fileURLToPath(import.meta.url));
const NAMES = ["rustright-broker", "rustright", "rustright-install"];
const sha256File = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const text = (result) => String(result?.stdout ?? "");

/** `key=value` lines (gate-output, expected-identity) as a Map. */
export function parseKeyValueLines(output) {
  const map = new Map();
  for (const line of String(output).split(/\r?\n/)) { const at = line.indexOf("="); if (at > 0) map.set(line.slice(0, at), line.slice(at + 1)); }
  return map;
}
const need = (map, key, what) => { const value = map.get(key); if (value === undefined || value === "") fail(`${what} reported no ${key}`); return value; };

export function parseRustrightStageArgs(argv, { platform = process.platform } = {}) {
  const opts = { repo: process.cwd(), scratch: platform === "win32" ? "D:\\scratch\\rustright-install" : "/Volumes/D/scratch/rustright-install", cutover: false, apply: false, configNext: null, splitControlRoot: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--cutover") opts.cutover = true;
    else if (flag === "--apply") opts.apply = true;
    else if (flag === "--split-control-root") opts.splitControlRoot = true;
    else if (["--repo", "--scratch", "--config-next"].includes(flag)) {
      if (argv[i + 1] === undefined || argv[i + 1].startsWith("--")) fail(`${flag} needs a value`);
      opts[flag === "--config-next" ? "configNext" : flag.slice(2)] = argv[i + 1]; i += 1;
    } else fail(`unknown argument: ${flag}`);
  }
  if (opts.apply && !opts.cutover) fail("--apply belongs to --cutover");
  if (opts.configNext && !path.isAbsolute(opts.configNext)) fail("--config-next must be an absolute candidate config file");
  return opts;
}

/**
 * Stage HEAD of `repo`. `deps` injects: run (spawnSync-shaped), platform, env, sign (macOS: (bundle, identity) => void; Windows: (files) => void), fs pieces, log.
 * Returns { commit, version, staged, cutoverScript, cutoverStatus }.
 */
export function stageRustright(opts, deps = {}) {
  const { run = spawnSync, platform = process.platform, env = process.env, log = (line) => console.log(line), home = os.userInfo().homedir, now = () => new Date() } = deps;
  const checked = (command, args, what, options = {}) => {
    const result = run(command, args, { encoding: "utf8", windowsHide: true, shell: false, ...options });
    if (result?.error || result?.status !== 0) fail(`${what} failed (${result?.error?.message ?? `exit ${result?.status}`})${result?.stderr ? `: ${String(result.stderr).trim().slice(0, 400)}` : ""}`);
    return result;
  };
  const repo = path.resolve(opts.repo);
  const commit = text(checked("git", ["-C", repo, "rev-parse", "HEAD"], "git rev-parse HEAD")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit)) fail(`HEAD of ${repo} is not a commit id: ${commit}`);
  if (platform === "darwin" && text(checked("git", ["-C", repo, "status", "--porcelain"], "git status")).trim()) fail("the primary checkout is not clean");
  const short = commit.slice(0, 7);
  const version = `rustright-${short}`;
  const gateOutput = path.join(repo, "tools", "install", "gate-output.mjs");
  if (!existsSync(gateOutput)) fail(`${gateOutput} is missing: this is not a RustRight checkout`);
  const scratch = path.resolve(opts.scratch);
  mkdirSync(scratch, { recursive: true });
  const exe = (name) => (platform === "win32" ? `${name}.exe` : name);

  // 1. The gate's compile output for HEAD, proved against the gate's receipt and copied out so later writes to the output cannot change what is staged.
  const bins = path.join(scratch, `gate-bin-${short}`);
  rmSync(bins, { recursive: true, force: true });
  let gate;
  try { gate = parseKeyValueLines(text(checked(process.execPath, [gateOutput, "--repo", repo, "--bins", NAMES.join(","), "--copy-to", bins], "gate-output"))); }
  catch (error) { fail(`no gate compile output vouched for HEAD ${short}: run rightkit-gate --to compile in ${repo}, then rerun this (${error.message})`); }
  if (need(gate, "commit", "gate-output") !== commit) fail(`gate-output vouched for ${gate.get("commit")}, HEAD is ${commit}`);
  const gateSha = Object.fromEntries(NAMES.map((name) => [name, need(gate, `sha256.${name}`, "gate-output")]));
  log(`gate compile: receipt=${gate.get("receipt")} executor=${gate.get("executor")} exception=${gate.get("exception")} outputs=${gate.get("outputs.digest")}`);
  const sameAsGate = (file, name) => { const got = sha256File(file); if (got !== gateSha[name]) fail(`${file} is not the gate's compiled bytes (sha256 ${got}, gate ${gateSha[name]}); nothing was signed`); };
  for (const name of NAMES) sameAsGate(path.join(bins, exe(name)), name);

  let brokerBinary; let clientBinary; let installer; let staged; let cutoverArgs; let cutoverScript; let bundle = null;
  const control = deps.controlRoot ?? opts.controlRoot ?? (platform === "win32" ? "D:\\.rightkit-managed" : path.join(home, "Library", "Application Support", "RightKit", "build-control"));
  if (platform === "darwin") {
    // 2. Bundle, helpers, digest re-check, Developer ID signature.
    const dir = path.join(scratch, `local-${short}`);
    bundle = path.join(dir, "RustRight Broker.app");
    const riBin = path.join(bins, "rustright-install");
    const expected = parseKeyValueLines(text(checked(riBin, ["expected-identity"], "rustright-install expected-identity")));
    const team = need(expected, "team", "the installer's expected identity");
    const bundleId = need(expected, "bundle.identifier", "the installer's expected identity");
    const requirement = need(expected, "bundle.requirement", "the installer's expected identity");
    let identity;
    try { identity = (deps.identity ?? (() => resolveMacosDeveloperIdIdentity({ env, run })))(); } catch (error) { fail(error.message); }
    if (!identity.endsWith(`(${team})`)) fail(`signing identity: its team is not the canonical team ${team}`);
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    checked(riBin, ["bundle", "--broker-binary", path.join(bins, "rustright-broker"), "--version", version, "--out", bundle, "--apply"], "rustright-install bundle");
    mkdirSync(path.join(bundle, "Contents", "Helpers"), { recursive: true });
    for (const name of ["rustright", "rustright-install"]) { copyFileSync(path.join(bins, name), path.join(bundle, "Contents", "Helpers", name)); chmodSync(path.join(bundle, "Contents", "Helpers", name), 0o755); }
    sameAsGate(path.join(bundle, "Contents", "MacOS", "rustright-broker"), "rustright-broker");
    sameAsGate(path.join(bundle, "Contents", "Helpers", "rustright"), "rustright");
    sameAsGate(path.join(bundle, "Contents", "Helpers", "rustright-install"), "rustright-install");
    (deps.sign ?? ((app, who) => {
      const results = runMacCodesignCommands([["--force", "--deep", "--options", "runtime", "--timestamp", "--sign", who, app]], { env, run });
      if (!results.length || results.some((r) => r?.error || r?.status !== 0)) fail(`codesign failed: ${String(results.at(-1)?.stderr ?? results.at(-1)?.error?.message ?? "").trim().slice(0, 300)}`);
    }))(bundle, identity);
    checked("codesign", ["--verify", "--deep", "--strict", `-R=${requirement}`, bundle], "codesign --verify against the canonical designated requirement");
    const signature = `\n${String(checked("codesign", ["-dv", "--verbose=2", bundle], "codesign -dv").stderr ?? "")}\n`;
    if (!signature.includes(`\nIdentifier=${bundleId}\n`)) fail(`bundle identifier is not ${bundleId}`);
    if (!signature.includes(`\nTeamIdentifier=${team}\n`)) fail(`team identifier is not ${team}`);
    brokerBinary = path.join(bundle, "Contents", "MacOS", "rustright-broker");
    clientBinary = path.join(bundle, "Contents", "Helpers", "rustright");
    installer = path.join(bundle, "Contents", "Helpers", "rustright-install");
    const bs = sha256File(brokerBinary);
    const cs = sha256File(clientBinary);
    // 3. Stage and approve.
    const out = text(checked(installer, ["stage", "--control-root", control, "--source-checkout", repo, "--commit", commit, "--version", version, "--broker-bundle", bundle, "--broker-sha256", bs, "--client-binary", clientBinary, "--client-sha256", cs, "--apply"], "rustright-install stage"));
    staged = out.split(/\r?\n/).map((line) => /^staged (.*) \((?:reused|version) .*/.exec(line)?.[1]).find(Boolean);
    if (!staged) fail("rustright-install stage printed no staged path");
    checked(installer, ["approve", "--control-root", control, "--staged", staged, "--commit", commit, "--approved-by", `${os.userInfo().username} ran right-release rustright-stage for ${version}`, "--approved-at", now().toISOString().replace(/\.\d+Z$/, ".000Z"), "--apply"], "rustright-install approve");
    const external = deps.externalRoot ?? "/Volumes/D/.rightkit-managed";
    cutoverArgs = ["cutover", "--platform", "darwin", "--control-root", control, "--config", path.join(control, "config.json"), "--external-root", external, "--source-revision", commit, "--source-checkout", repo, "--version", version, "--label", "dev.rustright.broker", "--uid", String(process.getuid?.() ?? 501), "--endpoint", path.join(control, "broker.sock"), "--plist", path.join(home, "Library", "LaunchAgents", "dev.rustright.broker.plist"), "--health-workspace", deps.healthWorkspace ?? "/Volumes/D/claude/cutright", "--broker-bundle", bundle, "--client-binary", clientBinary, "--broker-sha256", bs, "--client-sha256", cs, "--drain-timeout-ms", "1800000", "--timeout-ms", "600000", "--preflight-timeout-ms", "240000", "--agent-bin", path.join(control, "agent-bin")];
    cutoverScript = path.join(scratch, `mac-cutover-${short}.sh`);
    const quote = (value) => `'${String(value).replaceAll("'", "'\\''")}'`;
    writeFileSync(cutoverScript, `#!/bin/bash\n# Written by right-release rustright-stage for ${version} (commit ${commit}). Extra arguments pass through: --apply, and --config-next <absolute candidate config file>.\n${quote(installer)} ${cutoverArgs.map(quote).join(" ")} "$@"\n`);
    chmodSync(cutoverScript, 0o755);
  } else if (platform === "win32") {
    // 2. Authenticode through RightKit's own signer, then require Valid and the product signer on each.
    const dir = path.join(scratch, `activate-${short}`);
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    for (const name of NAMES) copyFileSync(path.join(bins, exe(name)), path.join(dir, exe(name)));
    brokerBinary = path.join(dir, exe("rustright-broker"));
    clientBinary = path.join(dir, exe("rustright"));
    installer = path.join(dir, exe("rustright-install"));
    const files = [brokerBinary, clientBinary, installer];
    (deps.sign ?? ((list) => { checked(process.execPath, [path.join(HERE, "sign-windows.mjs"), ...list], "sign-windows"); }))(files);
    for (const file of files) {
      const probe = checked("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `$s = Get-AuthenticodeSignature -LiteralPath '${file.replaceAll("'", "''")}'; "$($s.Status)|$($s.SignerCertificate.Subject)"`], `Authenticode check of ${file}`);
      const [status, subject = ""] = text(probe).trim().split("|");
      if (status !== "Valid" || !subject.startsWith("CN=Damned Ventures LLC,")) fail(`${file} is not validly signed by Damned Ventures LLC (status ${status}, signer ${subject})`);
    }
    const bs = sha256File(brokerBinary);
    const cs = sha256File(clientBinary);
    // The cutover's source checkout is a clone of exactly HEAD, so the primary checkout moving on does not move what the cutover reads.
    const src = path.join(scratch, `src-${short}`);
    rmSync(src, { recursive: true, force: true });
    checked("git", ["clone", "-q", "--no-checkout", "-c", "core.autocrlf=false", repo, src], "git clone of HEAD");
    checked("git", ["-C", src, "fetch", "-q", repo, "+refs/remotes/origin/main:refs/remotes/origin/main"], "git fetch origin/main");
    checked("git", ["-C", src, "-c", "advice.detachedHead=false", "checkout", "-q", commit], "git checkout of HEAD");
    if (text(checked("git", ["-C", src, "rev-parse", "HEAD"], "git rev-parse")).trim() !== commit) fail(`the source clone is not at ${commit}`);
    const runtimeRoot = deps.runtimeRoot ?? "D:\\.rustright";
    checked(installer, ["stage", "--platform", "windows", "--control-root", control, "--broker-binary", brokerBinary, "--client-binary", clientBinary, "--broker-sha256", bs, "--client-sha256", cs, "--commit", commit, "--source-checkout", src, "--version", version, "--apply"], "rustright-install stage");
    staged = path.join(runtimeRoot, "runtime.next");
    checked(installer, ["approve", "--platform", "windows", "--control-root", control, "--staged", staged, "--commit", commit, "--approved-by", `${os.userInfo().username} ran right-release rustright-stage for ${version}`, "--approved-at", now().toISOString().replace(/\.\d+Z$/, ".000Z"), "--apply"], "rustright-install approve");
    cutoverArgs = ["cutover", "--platform", "win32", "--control-root", control, "--config", path.join(control, "config.json"), "--external-root", control, "--source-revision", commit, "--source-checkout", src, "--version", version, "--task-name", "RustRightBroker", "--endpoint", "\\\\.\\pipe\\rustright-broker", "--health-workspace", deps.healthWorkspace ?? "D:\\Claude\\cutright", "--broker-binary", brokerBinary, "--client-binary", clientBinary, "--broker-sha256", bs, "--client-sha256", cs, "--drain-timeout-ms", "600000", "--agent-bin", path.join(control, "agent-bin")];
    if (opts.splitControlRoot) fail("--split-control-root is not folded in yet: run the split migration with xps-stage.ps1 -SplitControlRoot (rustright#54)");
    cutoverScript = path.join(scratch, `xps-cutover-${short}.ps1`);
    const literal = (value) => `'${String(value).replaceAll("'", "''")}'`;
    writeFileSync(cutoverScript, `# Written by right-release rustright-stage for ${version} (commit ${commit}). Without -Apply it is a dry run that prints the drain: line.\nparam([switch]$Apply, [string]$ConfigNext = '')\nSet-StrictMode -Version Latest\n$ErrorActionPreference = 'Stop'\n$cutover = @(${cutoverArgs.map(literal).join(", ")})\nif ($ConfigNext) { $cutover += @('--config-next', $ConfigNext) }\nif ($Apply) { $cutover += '--apply' }\n& ${literal(installer)} @$cutover\nexit $LASTEXITCODE\n`);
  } else fail(`rustright-stage runs on macOS or Windows, not ${platform}`);

  log(`staged=${staged}`);
  log(`cutover=${cutoverScript}`);
  // 4. The swap is rustright-install's: this tool calls it only when asked, a dry run unless --apply.
  let cutoverStatus = null;
  if (opts.cutover) {
    const args = [...cutoverArgs, ...(opts.configNext ? ["--config-next", opts.configNext] : []), ...(opts.apply ? ["--apply"] : [])];
    const result = run(installer, args, { stdio: "inherit", windowsHide: true, shell: false });
    cutoverStatus = result?.status ?? 1;
    if (cutoverStatus !== 0) fail(`rustright-install cutover exited ${cutoverStatus}`);
  }
  return { commit, version, staged, cutoverScript, cutoverStatus };
}

export function main(argv = process.argv.slice(2)) {
  try { stageRustright(parseRustrightStageArgs(argv)); return 0; } catch (error) {
    console.error(`right-release rustright-stage: ${error.message}`);
    return error instanceof RustrightStageError ? 1 : 2;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
