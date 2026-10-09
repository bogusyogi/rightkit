#!/usr/bin/env node
// Sanctioned, non-intrusive local app install from a signed DMG (macOS); --dev also accepts a
// Developer ID-signed, not-notarized zip or .app.
// Never uses a shell, osascript, AppleEvents, or foreground activation.
import { execFile as nodeExecFile } from "./background-process.mjs";
import { detachWithRetry } from "./hdiutil-detach.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const SIGTERM_WAIT_MS = 15_000;
const POLL_MS = 250;

export class InstallAppError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

const VALUE_FLAGS = new Set(["--dmg", "--zip", "--app", "--app-name", "--bundle-id", "--dest"]);
const BOOL_FLAGS = new Set(["--relaunch", "--no-relaunch", "--json", "--dev", "--recover"]);

export function parseArgs(argv) {
  const opts = { dmg: null, zip: null, app: null, dev: false, appName: null, bundleId: null, dest: "/Applications", relaunch: null, json: false, recover: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (VALUE_FLAGS.has(flag)) {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new InstallAppError("usage", `${flag} requires a value`);
      i += 1;
      if (flag === "--dmg") opts.dmg = value;
      else if (flag === "--zip") opts.zip = value;
      else if (flag === "--app") opts.app = value;
      else if (flag === "--app-name") opts.appName = value;
      else if (flag === "--bundle-id") opts.bundleId = value;
      else opts.dest = value;
    } else if (BOOL_FLAGS.has(flag)) {
      if (flag === "--json") opts.json = true;
      else if (flag === "--dev") opts.dev = true;
      else if (flag === "--recover") opts.recover = true;
      else opts.relaunch = flag === "--relaunch";
    } else {
      throw new InstallAppError("usage", `unknown argument: ${flag}`);
    }
  }
  if (argv.includes("--relaunch") && argv.includes("--no-relaunch")) throw new InstallAppError("usage", "--relaunch and --no-relaunch are mutually exclusive");
  const inputs = [opts.dmg, opts.zip, opts.app].filter((v) => v !== null).length;
  if (opts.recover) {
    if (inputs > 0 || opts.dev || opts.relaunch !== null || opts.bundleId !== null) throw new InstallAppError("usage", "--recover only accepts --dest, --app-name, and --json");
    if (opts.appName !== null && (!/^[^/\0]+\.app$/.test(opts.appName) || opts.appName === ".app" || opts.appName.includes(".."))) {
      throw new InstallAppError("usage", "--app-name must be a bare bundle name ending in .app");
    }
    if (!path.isAbsolute(opts.dest)) opts.dest = path.resolve(opts.dest);
    return opts;
  }
  if (inputs === 0) throw new InstallAppError("usage", opts.dev ? "one of --dmg <path.dmg>, --zip <path.zip>, or --app <path.app> is required" : "--dmg <path.dmg> is required");
  if (inputs > 1) throw new InstallAppError("usage", "--dmg, --zip, and --app are mutually exclusive");
  if ((opts.zip !== null || opts.app !== null) && !opts.dev) throw new InstallAppError("usage", "--zip and --app require --dev (only DMGs are installable as release builds)");
  if (opts.dmg !== null && !opts.dmg.toLowerCase().endsWith(".dmg")) throw new InstallAppError("usage", "--dmg must name a .dmg file");
  if (opts.zip !== null && !opts.zip.toLowerCase().endsWith(".zip")) throw new InstallAppError("usage", "--zip must name a .zip file");
  if (opts.app !== null && !opts.app.replace(/\/+$/, "").toLowerCase().endsWith(".app")) throw new InstallAppError("usage", "--app must name a .app bundle");
  if (opts.appName !== null && (!/^[^/\0]+\.app$/.test(opts.appName) || opts.appName === ".app" || opts.appName.includes(".."))) {
    throw new InstallAppError("usage", "--app-name must be a bare bundle name ending in .app");
  }
  if (opts.bundleId !== null && !/^[A-Za-z0-9][A-Za-z0-9.-]*$/.test(opts.bundleId)) throw new InstallAppError("usage", "--bundle-id is not a valid bundle identifier");
  if (!path.isAbsolute(opts.dest)) opts.dest = path.resolve(opts.dest);
  if (opts.dmg !== null) opts.dmg = path.resolve(opts.dmg);
  if (opts.zip !== null) opts.zip = path.resolve(opts.zip);
  if (opts.app !== null) opts.app = path.resolve(opts.app);
  return opts;
}

// --- pure parsers ---------------------------------------------------------

function xmlUnescape(text) {
  return text.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, "&");
}

// Minimal plist reader for hdiutil attach -plist output (dict/array/string/data/integer/true/false).
export function parsePlist(xml) {
  const cleaned = String(xml).replace(/<\?[\s\S]*?\?>/g, "").replace(/<!--[\s\S]*?-->/g, "").replace(/<!DOCTYPE[^>]*>/gi, "");
  const tokens = [...cleaned.matchAll(/<(\/?)([a-zA-Z]+)(?:\s[^>]*?)?(\s*\/)?>|([^<]+)/g)];
  let pos = 0;
  const skipText = () => { while (pos < tokens.length && tokens[pos][4] !== undefined && tokens[pos][4].trim() === "") pos += 1; };
  function readValue() {
    skipText();
    const t = tokens[pos];
    if (!t || t[4] !== undefined || t[1]) throw new InstallAppError("plist", "malformed plist");
    const name = t[2];
    pos += 1;
    if (t[3]) return name === "true" ? true : name === "false" ? false : "";
    if (name === "dict") {
      const out = {};
      for (;;) {
        skipText();
        const n = tokens[pos];
        if (!n) throw new InstallAppError("plist", "unterminated dict");
        if (n[1] && n[2] === "dict") { pos += 1; return out; }
        if (n[2] !== "key") throw new InstallAppError("plist", "expected key");
        pos += 1;
        const key = tokens[pos]?.[4] !== undefined ? xmlUnescape(tokens[pos++][4]) : "";
        pos += 1; // </key>
        out[key] = readValue();
      }
    }
    if (name === "array") {
      const out = [];
      for (;;) {
        skipText();
        const n = tokens[pos];
        if (!n) throw new InstallAppError("plist", "unterminated array");
        if (n[1] && n[2] === "array") { pos += 1; return out; }
        out.push(readValue());
      }
    }
    let text = "";
    if (tokens[pos]?.[4] !== undefined) { text = xmlUnescape(tokens[pos][4]); pos += 1; }
    pos += 1; // closing tag
    if (name === "integer") return Number(text);
    if (name === "real") return Number(text);
    return text;
  }
  while (pos < tokens.length && !(tokens[pos][2] === "plist" && !tokens[pos][1])) pos += 1;
  if (pos >= tokens.length) throw new InstallAppError("plist", "no <plist> element");
  pos += 1;
  return readValue();
}

export function mountPointsFromAttach(plistXml) {
  const parsed = parsePlist(plistXml);
  const entities = parsed?.["system-entities"];
  if (!Array.isArray(entities)) throw new InstallAppError("attach", "hdiutil attach output has no system-entities");
  return entities.map((e) => e?.["mount-point"]).filter((m) => typeof m === "string" && m.length > 0);
}

export function parseTeamId(codesignDvvOutput) {
  const match = /^TeamIdentifier=(.+)$/m.exec(codesignDvvOutput);
  const value = match?.[1].trim();
  return value && value !== "not set" ? value : null;
}

// Parse `ps -axo pid=,comm=` and return pids whose executable lives inside <app>/Contents/MacOS/.
export function matchPids(psOutput, appBundlePath, { selfPid = process.pid } = {}) {
  const prefix = `${appBundlePath.replace(/\/+$/, "")}/Contents/MacOS/`;
  const pids = [];
  for (const line of String(psOutput).split("\n")) {
    const m = /^\s*(\d+)\s+(.+?)\s*$/.exec(line);
    if (!m) continue;
    const pid = Number(m[1]);
    if (pid === selfPid || pid <= 1) continue;
    if (m[2].startsWith(prefix)) pids.push(pid);
  }
  return pids;
}

export function trashName(appName, now = new Date()) {
  const stamp = now.toISOString().replace(/[:T]/g, "-").replace(/\..*$/, "");
  const base = appName.replace(/\.app$/, "");
  return `${base} ${stamp}.app`;
}

// --- orchestration --------------------------------------------------------

const realExecFile = (cmd, args) => new Promise((resolve, reject) => {
  nodeExecFile(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, windowsHide: true }, (error, stdout, stderr) => {
    if (error) { error.stdout = stdout; error.stderr = stderr; reject(error); } else resolve({ stdout, stderr });
  });
});

function errText(error) {
  return `${error?.stderr || ""}${error?.stdout || ""}`.trim() || error?.message || String(error);
}

// --- crash-safe swap: journal + recovery -----------------------------------

const JOURNAL_RE = /^\.(.+)\.rightkit-install\.json$/;

export function journalPathFor(dest, appName) {
  return path.join(dest, `.${appName.replace(/\.app$/, "")}.rightkit-install.json`);
}

// Atomic write: temp file, fsync, rename, then a best-effort fsync of the directory.
export function writeJournal(fsImpl, file, data) {
  const tmp = `${file}.tmp`;
  const fd = fsImpl.openSync(tmp, "w");
  try { fsImpl.writeSync(fd, `${JSON.stringify(data)}\n`); fsImpl.fsyncSync(fd); }
  finally { fsImpl.closeSync(fd); }
  fsImpl.renameSync(tmp, file);
  try {
    const dfd = fsImpl.openSync(path.dirname(file), "r");
    try { fsImpl.fsyncSync(dfd); } finally { fsImpl.closeSync(dfd); }
  } catch { /* directory fsync is best effort */ }
}

const defaultIsAlive = (pid) => {
  try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; }
};

async function trashBundle(run, fsImpl, home, now, bundle, appName) {
  const trashDir = path.join(home, ".Trash");
  fsImpl.mkdirSync(trashDir, { recursive: true });
  const trashed = path.join(trashDir, trashName(appName, now()));
  await moveAside(run, fsImpl, bundle, trashed);
  return trashed;
}

// Deterministic roll-forward/back of any interrupted install journal in `dest`. Idempotent.
export async function recoverInstalls({ dest, appName = null, run, fsImpl, home, now, event, sleep, isAlive = defaultIsAlive, selfPid = process.pid }) {
  let names = [];
  try { names = fsImpl.readdirSync(dest).filter((n) => JOURNAL_RE.test(n)); } catch { return []; }
  if (appName) names = names.filter((n) => n === path.basename(journalPathFor(dest, appName)));
  const results = [];
  for (const name of names.sort()) {
    const file = path.join(dest, name);
    let j;
    try { j = JSON.parse(fsImpl.readFileSync(file, "utf8")); } catch {
      event("recovery-journal-unreadable", { journal: file });
      try { fsImpl.rmSync(file, { force: true }); } catch { /* best effort */ }
      continue;
    }
    if (j.pid && j.pid !== selfPid && isAlive(j.pid)) { event("recovery-skipped", { journal: file, pid: j.pid, reason: "install still running" }); continue; }
    const has = (p) => Boolean(p) && fsImpl.existsSync(p);
    const rm = (p) => { if (p) { try { fsImpl.rmSync(p, { recursive: true, force: true }); } catch { /* best effort */ } } };
    let action;
    let keepJournal = false;
    try {
      if (!has(j.installed)) {
        if (has(j.backup)) { fsImpl.renameSync(j.backup, j.installed); action = "restored-backup"; }
        else action = "cleared";
      } else if (has(j.backup)) {
        let ok = j.phase === "verified";
        if (!ok) { try { await verifyBundle(run, j.installed, { assess: !j.dev, staple: false }); ok = true; } catch { ok = false; } }
        if (ok) {
          try { await trashBundle(run, fsImpl, home, now, j.backup, path.basename(j.installed)); action = "rolled-forward"; }
          catch { action = "rolled-forward-trash-pending"; keepJournal = true; }
        } else {
          rm(j.installed);
          fsImpl.renameSync(j.backup, j.installed);
          action = "rolled-back";
        }
      } else action = "cleared";
    } catch (error) {
      event("recovery-failed", { journal: file, reason: errText(error) });
      continue; // leave the journal for the next run
    }
    rm(j.staged);
    if (j.mount && has(j.mount)) {
      const out = await detachWithRetry({ mount: j.mount, log: (e) => event(e.event, e), sleep, detach: (args) => run("/usr/bin/hdiutil", args) });
      if (out.ok) rm(j.mountRoot);
    } else rm(j.mountRoot);
    if (!keepJournal) rm(file);
    event("recovered", { journal: file, action, installed: j.installed });
    results.push({ journal: file, action, installed: j.installed });
  }
  return results;
}

export async function recoverApp(opts, deps = {}) {
  const { execFile = realExecFile, fsImpl = fs, home = os.homedir(), now = () => new Date(), sleep = (ms) => new Promise((r) => setTimeout(r, ms)), emit = () => {}, isAlive, platform = process.platform } = deps;
  if (platform !== "darwin") throw new InstallAppError("unsupported", `install-app is not implemented on ${platform}`);
  const results = await recoverInstalls({ dest: opts.dest, appName: opts.appName, run: (c, a) => execFile(c, a), fsImpl, home, now, event: (name, data = {}) => emit({ event: name, ...data }), sleep, isAlive });
  emit({ event: "recover-done", dest: opts.dest, recovered: results.length });
  return { ok: true, recovered: results };
}

export async function installApp(opts, deps = {}) {
  const {
    execFile = realExecFile,
    platform = process.platform,
    fsImpl = fs,
    home = os.homedir(),
    now = () => new Date(),
    sleep = (ms) => new Promise((r) => setTimeout(r, ms)),
    kill = (pid, sig) => process.kill(pid, sig),
    emit = () => {},
    tmpdir = os.tmpdir(),
    pid = process.pid,
    isAlive = defaultIsAlive,
    signalTarget = process,
    exit = (code) => process.exit(code),
  } = deps;
  const dev = opts.dev === true;
  const event = (name, data = {}) => emit({ event: name, ...(dev ? { dev: true } : {}), ...data });
  const run = (cmd, args) => execFile(cmd, args);
  let mountRoot = null;
  let extractRoot = null;
  let mounted = false;
  let mountPoint = null;
  let failedStage = "start";
  let swap = null; // set once the journal exists; tracks which renames happened

  const removeQuiet = (p) => { if (p) { try { fsImpl.rmSync(p, { recursive: true, force: true }); } catch { /* best effort */ } } };
  // Undo an unfinished swap using renames only. Sync on purpose so a signal handler can run it.
  const rollbackSwap = () => {
    if (!swap) return;
    const s = swap;
    swap = null;
    try {
      if (!s.final) {
        if (s.stagedIn) removeQuiet(s.installed);
        if (s.backedUp && !fsImpl.existsSync(s.installed)) fsImpl.renameSync(s.backup, s.installed);
        removeQuiet(s.staged);
        removeQuiet(s.journal);
      } else {
        removeQuiet(s.staged);
      }
    } catch { /* recovery on the next run finishes the job */ }
  };
  let signaled = false;
  const onSignal = (sig) => {
    if (signaled) return;
    signaled = true;
    (async () => {
      try {
        rollbackSwap();
        event("interrupted", { signal: sig });
        await detach();
        cleanupExtract();
      } catch { /* exiting anyway */ }
      exit(128 + (os.constants.signals[sig] || 0));
    })();
  };
  const handlers = { SIGINT: () => onSignal("SIGINT"), SIGTERM: () => onSignal("SIGTERM") };

  const detach = async () => {
    if (!mounted) return;
    mounted = false;
    await detachWithRetry({ mount: mountPoint, log: (e) => event(e.event, e), sleep, detach: (args) => run("/usr/bin/hdiutil", args) });
    try { fsImpl.rmSync(mountRoot, { recursive: true, force: true }); } catch { /* best effort */ }
  };
  const cleanupExtract = () => {
    if (!extractRoot) return;
    try { fsImpl.rmSync(extractRoot, { recursive: true, force: true }); } catch { /* best effort */ }
    extractRoot = null;
  };

  try {
    if (platform !== "darwin") throw new InstallAppError("unsupported", `install-app is not implemented on ${platform}`);
    for (const [sig, fn] of Object.entries(handlers)) signalTarget.on(sig, fn);
    const inputPath = opts.dmg ?? opts.zip ?? opts.app;
    const inputKind = opts.dmg ? "dmg" : opts.zip ? "zip" : "app";
    event("start", { [inputKind]: inputPath, dest: opts.dest });
    if (dev) event("dev-build", { note: "DEV BUILD (not notarized)" });
    if (!fsImpl.existsSync(inputPath)) throw new InstallAppError(`${inputKind}-missing`, `${inputKind.toUpperCase()} not found: ${inputPath}`);
    if (!fsImpl.existsSync(opts.dest)) throw new InstallAppError("dest-missing", `destination not found: ${opts.dest}`);

    await recoverInstalls({ dest: opts.dest, appName: opts.appName, run, fsImpl, home, now, event, sleep, isAlive, selfPid: pid });

    let sourceApp = null;
    const pickApp = (dir, label) => {
      const apps = fsImpl.readdirSync(dir).filter((n) => n.endsWith(".app"));
      if (opts.appName) return apps.includes(opts.appName) ? path.join(dir, opts.appName) : null;
      if (apps.length === 1) return path.join(dir, apps[0]);
      if (apps.length > 1) throw new InstallAppError("ambiguous-app", `${label} contains multiple apps (${apps.join(", ")}); pass --app-name`);
      return null;
    };
    if (opts.dmg) {
      failedStage = "attach";
      mountRoot = fsImpl.mkdtempSync(path.join(tmpdir, "right-install-app-"));
      const attached = await run("/usr/bin/hdiutil", ["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountrandom", mountRoot, "-plist", opts.dmg]);
      const mounts = mountPointsFromAttach(attached.stdout);
      if (mounts.length === 0) throw new InstallAppError("attach", "DMG attached but exposed no mount point");
      mountPoint = mounts[0];
      mounted = true;
      for (const mp of mounts) {
        sourceApp = pickApp(mp, "DMG");
        if (sourceApp) break;
      }
      if (!sourceApp) throw new InstallAppError("app-not-found", opts.appName ? `${opts.appName} not found in DMG` : "no .app found in DMG");
    } else if (opts.zip) {
      failedStage = "extract";
      extractRoot = fsImpl.mkdtempSync(path.join(tmpdir, "right-install-app-zip-"));
      try { await run("/usr/bin/ditto", ["-x", "-k", opts.zip, extractRoot]); }
      catch (error) { throw new InstallAppError("zip-extract", `ditto -x -k failed for ${opts.zip}: ${errText(error)}`); }
      sourceApp = pickApp(extractRoot, "zip");
      if (!sourceApp) throw new InstallAppError("app-not-found", opts.appName ? `${opts.appName} not found in zip` : "no .app found in zip");
    } else {
      sourceApp = opts.app.replace(/\/+$/, "");
      if (opts.appName && path.basename(sourceApp) !== opts.appName) throw new InstallAppError("app-not-found", `--app is ${path.basename(sourceApp)}, not ${opts.appName}`);
    }
    const appName = path.basename(sourceApp);
    const installed = path.join(opts.dest, appName);
    const hadInstalled = fsImpl.existsSync(installed);

    // Verify the mounted app before touching anything installed.
    failedStage = "verify";
    const { team: newTeam, ticket } = await verifyBundle(run, sourceApp, { assess: !dev, staple: !dev, dmg: opts.dmg });
    const newBundleId = await readBundleId(run, sourceApp);
    if (opts.bundleId && newBundleId !== opts.bundleId) throw new InstallAppError("bundle-id-mismatch", `${inputKind.toUpperCase()} app bundle id ${newBundleId} != --bundle-id ${opts.bundleId}`);
    let installedTeam = null;
    if (hadInstalled) {
      const installedBundleId = await readBundleId(run, installed);
      if (installedBundleId !== newBundleId) throw new InstallAppError("bundle-id-mismatch", `${inputKind.toUpperCase()} app bundle id ${newBundleId} != installed bundle id ${installedBundleId}`);
      try { installedTeam = await teamIdOf(run, installed); } catch { installedTeam = null; }
      if (installedTeam && installedTeam !== newTeam) throw new InstallAppError("team-id-changed", `refusing team change: installed ${installedTeam}, ${inputKind.toUpperCase()} ${newTeam}`);
    }
    const bundleId = opts.bundleId || newBundleId;
    event("verified", { app: appName, bundleId, teamId: newTeam, replacing: hadInstalled, ...(ticket ? { ticket } : {}) });

    // Stage a verified copy on the destination volume so the swap is rename(2) only.
    failedStage = "stage";
    const base = appName.replace(/\.app$/, "");
    const staged = path.join(opts.dest, `.${base}.rightkit-install-${pid}.app`);
    const backup = hadInstalled ? path.join(opts.dest, `.${base}.rightkit-backup-${pid}.app`) : null;
    const journal = journalPathFor(opts.dest, appName);
    const journalData = { schema: 1, appName, pid, phase: "staging", dev, installed, backup, staged, mount: mountPoint, mountRoot };
    removeQuiet(staged);
    writeJournal(fsImpl, journal, journalData);
    swap = { installed, backup, staged, journal, backedUp: false, stagedIn: false, final: false };
    try { await run("/usr/bin/ditto", [sourceApp, staged]); }
    catch (error) {
      rollbackSwap();
      throw new InstallAppError("ditto-failed", `ditto failed (${errText(error)}); previous app untouched`);
    }
    {
      const { team: stagedTeam } = await verifyBundle(run, staged, { assess: !dev, staple: false });
      const stagedId = await readBundleId(run, staged);
      if (stagedTeam !== newTeam || stagedId !== newBundleId) throw new InstallAppError("staged-mismatch", `staged copy ${staged} does not match the verified source (team ${stagedTeam}, bundle id ${stagedId})`);
    }

    // Quit running instance without UI.
    failedStage = "quit";
    const wasRunning = hadInstalled ? (await findPids(run, installed)).length > 0 : false;
    const quit = hadInstalled ? await quitApp(run, kill, sleep, installed) : { pids: [], killed: [] };
    event("quit", { wasRunning, pids: quit.pids, forced: quit.killed });

    // Swap: old -> backup, staged -> installed. The old bundle is never trashed before the new one is verified.
    failedStage = "replace";
    writeJournal(fsImpl, journal, { ...journalData, phase: "swapping" });
    if (hadInstalled) { fsImpl.renameSync(installed, backup); swap.backedUp = true; }
    fsImpl.renameSync(staged, installed);
    swap.stagedIn = true;
    // Dev builds are not notarized: clear quarantine on the NEW bundle only so Gatekeeper does not block it.
    if (dev) {
      try { await run("/usr/bin/xattr", ["-dr", "com.apple.quarantine", installed]); }
      catch (error) { event("quarantine-clear-failed", { installed, reason: errText(error) }); }
    }
    await detach();
    cleanupExtract();

    // Re-verify installed copy; on failure the catch below rolls back to the backup.
    failedStage = "reverify";
    await verifyBundle(run, installed, { assess: !dev, staple: false });
    writeJournal(fsImpl, journal, { ...journalData, phase: "verified", mount: null, mountRoot: null });
    swap.final = true;

    // Only now does the old bundle go to the Trash.
    let trashed = null;
    if (backup) {
      try { trashed = await trashBundle(run, fsImpl, home, now, backup, appName); }
      catch (error) { event("backup-trash-failed", { backup, reason: errText(error) }); trashed = backup; }
    }
    if (!backup || trashed !== backup) removeQuiet(journal);
    swap = null;
    event("replaced", { installed, previous: trashed });

    // Relaunch hidden and in the background only.
    const relaunch = opts.relaunch === null ? wasRunning : opts.relaunch;
    if (relaunch) {
      failedStage = "relaunch";
      await run("/usr/bin/open", ["-g", "-j", "-b", bundleId]);
      event("relaunched", { bundleId });
    }
    const result = { ok: true, app: appName, installed, bundleId, teamId: newTeam, previous: trashed, relaunched: relaunch, ...(dev ? { dev: true } : {}) };
    event("done", result);
    return result;
  } catch (error) {
    rollbackSwap();
    const reason = error instanceof InstallAppError ? error.message : errText(error);
    event("failed", { stage: failedStage, code: error.code || "error", reason });
    if (!(error instanceof InstallAppError)) throw new InstallAppError("error", reason);
    throw error;
  } finally {
    for (const [sig, fn] of Object.entries(handlers)) signalTarget.off(sig, fn);
    rollbackSwap();
    await detach();
    cleanupExtract();
  }
}

async function verifyBundle(run, app, { assess, staple, dmg = null }) {
  const step = async (code, cmd, args) => {
    try { return await run(cmd, args); }
    catch (error) { throw new InstallAppError(code, `${code} failed for ${app}: ${errText(error)}`); }
  };
  await step("codesign-verify", "/usr/bin/codesign", ["--verify", "--deep", "--strict", app]);
  const team = await teamIdOf(run, app);
  if (!team) throw new InstallAppError("team-id-missing", `${app} has no Team ID (ad-hoc or unsigned)`);
  await step("codesign-requirement", "/usr/bin/codesign", ["-dr", "-", app]);
  if (assess) await step("spctl-assess", "/usr/sbin/spctl", ["--assess", "--type", "execute", app]);
  let ticket = null;
  let haveStapler = true;
  if (staple) {
    // Clean macOS hosts have no Xcode/CLT, so `xcrun stapler` can be missing.
    try { await run("/usr/bin/xcrun", ["--find", "stapler"]); } catch { haveStapler = false; }
  }
  if (staple && !haveStapler) {
    if (dmg) await step("spctl-dmg", "/usr/sbin/spctl", ["-a", "-t", "open", "--context", "context:primary-signature", dmg]);
    const assessed = await step("spctl-notarized", "/usr/sbin/spctl", ["-a", "-t", "exec", "-vv", app]);
    if (!/Notarized Developer ID/.test(`${assessed?.stderr || ""}\n${assessed?.stdout || ""}`)) {
      throw new InstallAppError("spctl-notarized", `spctl-notarized failed for ${app}: not accepted as Notarized Developer ID`);
    }
    ticket = "spctl-only";
  } else if (staple) {
    try { await run("/usr/bin/xcrun", ["stapler", "validate", app]); ticket = "app-ticket"; }
    catch (error) {
      if (!dmg) throw new InstallAppError("stapler-validate", `stapler-validate failed for ${app}: ${errText(error)}`);
      // Standard Tauri flow: the DMG carries the notarization ticket, not the app. Accept only when the
      // stapled DMG validates, its primary signature is accepted, and Gatekeeper reports the app Notarized Developer ID.
      await step("stapler-validate", "/usr/bin/xcrun", ["stapler", "validate", dmg]);
      await step("spctl-dmg", "/usr/sbin/spctl", ["-a", "-t", "open", "--context", "context:primary-signature", dmg]);
      const assessed = await step("spctl-notarized", "/usr/sbin/spctl", ["-a", "-t", "exec", "-vv", app]);
      if (!/Notarized Developer ID/.test(`${assessed?.stderr || ""}\n${assessed?.stdout || ""}`)) {
        throw new InstallAppError("spctl-notarized", `spctl-notarized failed for ${app}: not accepted as Notarized Developer ID`);
      }
      ticket = "dmg-ticket";
    }
  }
  return { team, ticket };
}

async function teamIdOf(run, app) {
  const out = await run("/usr/bin/codesign", ["-dvv", app]);
  return parseTeamId(`${out.stderr || ""}\n${out.stdout || ""}`);
}

async function readBundleId(run, app) {
  let out;
  try { out = await run("/usr/bin/plutil", ["-convert", "json", "-o", "-", path.join(app, "Contents", "Info.plist")]); }
  catch (error) { throw new InstallAppError("info-plist", `cannot read Info.plist of ${app}: ${errText(error)}`); }
  const id = JSON.parse(out.stdout)?.CFBundleIdentifier;
  if (typeof id !== "string" || !id) throw new InstallAppError("info-plist", `no CFBundleIdentifier in ${app}`);
  return id;
}

async function findPids(run, app) {
  const ps = await run("/bin/ps", ["-axo", "pid=,comm="]);
  return matchPids(ps.stdout, app);
}

async function quitApp(run, kill, sleep, app) {
  const pids = await findPids(run, app);
  const send = (pid, sig) => { try { kill(pid, sig); } catch (e) { if (e.code !== "ESRCH") throw e; } };
  for (const pid of pids) send(pid, "SIGTERM");
  let waited = 0;
  let remaining = pids.length ? await findPids(run, app) : [];
  while (remaining.length && waited < SIGTERM_WAIT_MS) {
    await sleep(POLL_MS);
    waited += POLL_MS;
    remaining = await findPids(run, app);
  }
  for (const pid of remaining) send(pid, "SIGKILL");
  if (remaining.length) await sleep(POLL_MS);
  return { pids, killed: remaining };
}

async function moveAside(run, fsImpl, from, to) {
  try {
    fsImpl.renameSync(from, to);
  } catch (error) {
    if (error.code !== "EXDEV") throw error;
    await run("/usr/bin/ditto", [from, to]);
    fsImpl.rmSync(from, { recursive: true, force: true });
  }
}

export async function main(argv = process.argv.slice(2), io = {}) {
  const stdout = io.stdout || ((s) => console.log(s));
  const stderr = io.stderr || ((s) => console.error(s));
  let opts;
  try { opts = parseArgs(argv); }
  catch (error) {
    stderr(`right-release install-app: ${error.message}`);
    stderr("usage: right-release install-app (--dmg <path.dmg> | --dev (--zip <path.zip> | --app <path.app> | --dmg <path.dmg>)) [--app-name Pulse.app] [--bundle-id dev.orthic.pulse] [--dest /Applications] [--relaunch|--no-relaunch] [--json]");
    stderr("       right-release install-app --recover [--dest /Applications] [--app-name Pulse.app] [--json]");
    stderr("  --dev installs a Developer ID-signed, NOT notarized build (skips spctl/stapler, clears quarantine on the new bundle).");
    return 2;
  }
  const emit = (e) => {
    if (opts.json) { stdout(JSON.stringify({ schema: 1, ts: new Date().toISOString(), ...e })); return; }
    const { event, ...rest } = e;
    if (event === "failed") stderr(`install-app: failed at ${rest.stage}: ${rest.reason}`);
    else if (event === "dev-build") stdout(`install-app: ${rest.note}`);
    else stdout(`install-app: ${event}${Object.keys(rest).length ? ` ${JSON.stringify(rest)}` : ""}`);
  };
  try {
    if (opts.recover) await recoverApp(opts, { ...io.deps, emit });
    else await installApp(opts, { ...io.deps, emit });
    return 0;
  } catch (error) {
    return error.code === "unsupported" ? 3 : 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((code) => { process.exitCode = code; });
}
