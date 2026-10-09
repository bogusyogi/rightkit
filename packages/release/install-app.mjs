#!/usr/bin/env node
// Sanctioned, non-intrusive local app install from a signed DMG (macOS).
// Never uses a shell, osascript, AppleEvents, or foreground activation.
import { execFile as nodeExecFile } from "./background-process.mjs";
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

const VALUE_FLAGS = new Set(["--dmg", "--app-name", "--bundle-id", "--dest"]);
const BOOL_FLAGS = new Set(["--relaunch", "--no-relaunch", "--json"]);

export function parseArgs(argv) {
  const opts = { dmg: null, appName: null, bundleId: null, dest: "/Applications", relaunch: null, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (VALUE_FLAGS.has(flag)) {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new InstallAppError("usage", `${flag} requires a value`);
      i += 1;
      if (flag === "--dmg") opts.dmg = value;
      else if (flag === "--app-name") opts.appName = value;
      else if (flag === "--bundle-id") opts.bundleId = value;
      else opts.dest = value;
    } else if (BOOL_FLAGS.has(flag)) {
      if (flag === "--json") opts.json = true;
      else opts.relaunch = flag === "--relaunch";
    } else {
      throw new InstallAppError("usage", `unknown argument: ${flag}`);
    }
  }
  if (argv.includes("--relaunch") && argv.includes("--no-relaunch")) throw new InstallAppError("usage", "--relaunch and --no-relaunch are mutually exclusive");
  if (!opts.dmg) throw new InstallAppError("usage", "--dmg <path.dmg> is required");
  if (!opts.dmg.toLowerCase().endsWith(".dmg")) throw new InstallAppError("usage", "--dmg must name a .dmg file");
  if (opts.appName !== null && (!/^[^/\0]+\.app$/.test(opts.appName) || opts.appName === ".app" || opts.appName.includes(".."))) {
    throw new InstallAppError("usage", "--app-name must be a bare bundle name ending in .app");
  }
  if (opts.bundleId !== null && !/^[A-Za-z0-9][A-Za-z0-9.-]*$/.test(opts.bundleId)) throw new InstallAppError("usage", "--bundle-id is not a valid bundle identifier");
  if (!path.isAbsolute(opts.dest)) opts.dest = path.resolve(opts.dest);
  opts.dmg = path.resolve(opts.dmg);
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
  } = deps;
  const event = (name, data = {}) => emit({ event: name, ...data });
  const run = (cmd, args) => execFile(cmd, args);
  let mountRoot = null;
  let mounted = false;
  let mountPoint = null;
  let failedStage = "start";

  const detach = async () => {
    if (!mounted) return;
    mounted = false;
    try { await run("/usr/bin/hdiutil", ["detach", mountPoint]); }
    catch { try { await run("/usr/bin/hdiutil", ["detach", "-force", mountPoint]); } catch { /* best effort */ } }
    try { fsImpl.rmSync(mountRoot, { recursive: true, force: true }); } catch { /* best effort */ }
  };

  try {
    if (platform !== "darwin") throw new InstallAppError("unsupported", `install-app is not implemented on ${platform}`);
    event("start", { dmg: opts.dmg, dest: opts.dest });
    if (!fsImpl.existsSync(opts.dmg)) throw new InstallAppError("dmg-missing", `DMG not found: ${opts.dmg}`);
    if (!fsImpl.existsSync(opts.dest)) throw new InstallAppError("dest-missing", `destination not found: ${opts.dest}`);

    failedStage = "attach";
    mountRoot = fsImpl.mkdtempSync(path.join(tmpdir, "right-install-app-"));
    const attached = await run("/usr/bin/hdiutil", ["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountrandom", mountRoot, "-plist", opts.dmg]);
    const mounts = mountPointsFromAttach(attached.stdout);
    if (mounts.length === 0) throw new InstallAppError("attach", "DMG attached but exposed no mount point");
    mountPoint = mounts[0];
    mounted = true;

    let sourceApp = null;
    for (const mp of mounts) {
      const apps = fsImpl.readdirSync(mp).filter((n) => n.endsWith(".app"));
      if (opts.appName) { if (apps.includes(opts.appName)) { sourceApp = path.join(mp, opts.appName); break; } }
      else if (apps.length === 1) { sourceApp = path.join(mp, apps[0]); break; }
      else if (apps.length > 1) throw new InstallAppError("ambiguous-app", `DMG contains multiple apps (${apps.join(", ")}); pass --app-name`);
    }
    if (!sourceApp) throw new InstallAppError("app-not-found", opts.appName ? `${opts.appName} not found in DMG` : "no .app found in DMG");
    const appName = path.basename(sourceApp);
    const installed = path.join(opts.dest, appName);
    const hadInstalled = fsImpl.existsSync(installed);

    // Verify the mounted app before touching anything installed.
    failedStage = "verify";
    const newTeam = await verifyBundle(run, sourceApp, { assess: true, staple: true });
    const newBundleId = await readBundleId(run, sourceApp);
    if (opts.bundleId && newBundleId !== opts.bundleId) throw new InstallAppError("bundle-id-mismatch", `DMG app bundle id ${newBundleId} != --bundle-id ${opts.bundleId}`);
    let installedTeam = null;
    if (hadInstalled) {
      const installedBundleId = await readBundleId(run, installed);
      if (installedBundleId !== newBundleId) throw new InstallAppError("bundle-id-mismatch", `DMG app bundle id ${newBundleId} != installed bundle id ${installedBundleId}`);
      try { installedTeam = await teamIdOf(run, installed); } catch { installedTeam = null; }
      if (installedTeam && installedTeam !== newTeam) throw new InstallAppError("team-id-changed", `refusing team change: installed ${installedTeam}, DMG ${newTeam}`);
    }
    const bundleId = opts.bundleId || newBundleId;
    event("verified", { app: appName, bundleId, teamId: newTeam, replacing: hadInstalled });

    // Quit running instance without UI.
    failedStage = "quit";
    const wasRunning = hadInstalled ? (await findPids(run, installed)).length > 0 : false;
    const quit = hadInstalled ? await quitApp(run, kill, sleep, installed) : { pids: [], killed: [] };
    event("quit", { wasRunning, pids: quit.pids, forced: quit.killed });

    // Replace.
    failedStage = "replace";
    let trashed = null;
    if (hadInstalled) {
      const trashDir = path.join(home, ".Trash");
      fsImpl.mkdirSync(trashDir, { recursive: true });
      trashed = path.join(trashDir, trashName(appName, now()));
      await moveAside(run, fsImpl, installed, trashed);
    }
    try {
      await run("/usr/bin/ditto", [sourceApp, installed]);
    } catch (error) {
      try { fsImpl.rmSync(installed, { recursive: true, force: true }); } catch { /* ignore */ }
      let restored = false;
      if (trashed) { try { await moveAside(run, fsImpl, trashed, installed); restored = true; } catch { /* reported below */ } }
      throw new InstallAppError("ditto-failed", `ditto failed (${errText(error)}); ${trashed ? (restored ? "previous app restored" : `RESTORE FAILED, previous app is at ${trashed}`) : "no previous app to restore"}`);
    }
    event("replaced", { installed, previous: trashed });
    await detach();

    // Re-verify installed copy.
    failedStage = "reverify";
    await verifyBundle(run, installed, { assess: true, staple: false });

    // Relaunch hidden and in the background only.
    const relaunch = opts.relaunch === null ? wasRunning : opts.relaunch;
    if (relaunch) {
      failedStage = "relaunch";
      await run("/usr/bin/open", ["-g", "-j", "-b", bundleId]);
      event("relaunched", { bundleId });
    }
    const result = { ok: true, app: appName, installed, bundleId, teamId: newTeam, previous: trashed, relaunched: relaunch };
    event("done", result);
    return result;
  } catch (error) {
    const reason = error instanceof InstallAppError ? error.message : errText(error);
    event("failed", { stage: failedStage, code: error.code || "error", reason });
    if (!(error instanceof InstallAppError)) throw new InstallAppError("error", reason);
    throw error;
  } finally {
    await detach();
  }
}

async function verifyBundle(run, app, { assess, staple }) {
  const step = async (code, cmd, args) => {
    try { return await run(cmd, args); }
    catch (error) { throw new InstallAppError(code, `${code} failed for ${app}: ${errText(error)}`); }
  };
  await step("codesign-verify", "/usr/bin/codesign", ["--verify", "--deep", "--strict", app]);
  const team = await teamIdOf(run, app);
  if (!team) throw new InstallAppError("team-id-missing", `${app} has no Team ID (ad-hoc or unsigned)`);
  await step("codesign-requirement", "/usr/bin/codesign", ["-dr", "-", app]);
  if (assess) await step("spctl-assess", "/usr/sbin/spctl", ["--assess", "--type", "execute", app]);
  if (staple) await step("stapler-validate", "/usr/bin/xcrun", ["stapler", "validate", app]);
  return team;
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
    stderr("usage: right-release install-app --dmg <path.dmg> [--app-name Pulse.app] [--bundle-id dev.orthic.pulse] [--dest /Applications] [--relaunch|--no-relaunch] [--json]");
    return 2;
  }
  const emit = (e) => {
    if (opts.json) { stdout(JSON.stringify({ schema: 1, ts: new Date().toISOString(), ...e })); return; }
    const { event, ...rest } = e;
    if (event === "failed") stderr(`install-app: failed at ${rest.stage}: ${rest.reason}`);
    else stdout(`install-app: ${event}${Object.keys(rest).length ? ` ${JSON.stringify(rest)}` : ""}`);
  };
  try {
    await installApp(opts, { ...io.deps, emit });
    return 0;
  } catch (error) {
    return error.code === "unsupported" ? 3 : 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((code) => { process.exitCode = code; });
}
