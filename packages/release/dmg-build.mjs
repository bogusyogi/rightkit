import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, statSync, symlinkSync, writeFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { backgroundAlias, dmgDSStore } from "./dmg-layout.mjs";
import { assertReleaseExecutionAllowed } from "./execution-policy.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });
const pair = (value, label, positive = false) => {
  const numbers = Array.isArray(value) ? value : String(value).split(",").map(Number);
  if (numbers.length !== 2 || numbers.some((n) => !Number.isInteger(n) || n < (positive ? 1 : 0) || n > 32767)) {
    throw fail("DMG_INVALID_OPTIONS", `${label} requires ${positive ? "positive" : "nonnegative"} x,y integers <=32767`);
  }
  return numbers;
};

export function buildDmg({ app, out, volname, background, iconSize = 128, appPosition = [158, 231],
  applicationsPosition = [492, 231], window = [660, 400], volumeIcon } = {},
{ platform = process.platform, run = spawnSync, makeAlias = backgroundAlias,
  log = (event) => console.error(JSON.stringify(event)) } = {}) {
  if (platform !== "darwin") throw fail("DMG_NATIVE_HOST_REQUIRED", "DMG creation requires macOS");
  if (!app || !out || !volname) throw fail("DMG_INVALID_OPTIONS", "--app, --out & --volname are required");
  app = path.resolve(app); out = path.resolve(out);
  if (!app.endsWith(".app") || !statSync(app).isDirectory() || !existsSync(path.join(app, "Contents", "Info.plist"))) {
    throw fail("DMG_INVALID_OPTIONS", `expected .app bundle with Contents/Info.plist: ${app}`);
  }
  if (!out.endsWith(".dmg") || out.startsWith(`${app}${path.sep}`)) throw fail("DMG_INVALID_OPTIONS", "--out must be a .dmg outside app bundle");
  if (/[\0\r\n/:]/.test(volname) || Buffer.byteLength(volname) > 255) throw fail("DMG_INVALID_OPTIONS", "invalid volume name");
  iconSize = Number(iconSize);
  if (!Number.isInteger(iconSize) || iconSize < 16 || iconSize > 512) throw fail("DMG_INVALID_OPTIONS", "--icon-size must be 16..512");
  appPosition = pair(appPosition, "--app-position"); applicationsPosition = pair(applicationsPosition, "--applications-position");
  window = pair(window, "--window", true);
  if (volumeIcon) {
    volumeIcon = path.resolve(volumeIcon);
    if (!existsSync(volumeIcon) || !statSync(volumeIcon).isFile() || !/\.icns$/i.test(volumeIcon)) throw fail("DMG_INVALID_OPTIONS", "--volume-icon must be an .icns file");
  }
  if (background) {
    background = path.resolve(background);
    // Existing Right Suite backgrounds include TIFF; accept PNG & TIFF without conversion.
    if (!statSync(background).isFile() || !/\.(png|tiff?)$/i.test(background)) throw fail("DMG_INVALID_OPTIONS", "background must be PNG or TIFF");
    if (/\.png$/i.test(background) && !readFileSync(background).subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex"))) {
      throw fail("DMG_INVALID_OPTIONS", "invalid PNG signature");
    }
  }
  for (let cursor = out; cursor !== path.dirname(cursor); cursor = path.dirname(cursor)) {
    if (existsSync(cursor) && lstatSync(cursor).isSymbolicLink()) throw fail("DMG_INVALID_OPTIONS", `output must not traverse symlink: ${cursor}`);
  }
  assertReleaseExecutionAllowed({ cwd: app, command: "right-release dmg build" });
  mkdirSync(path.dirname(out), { recursive: true });
  const work = mkdtempSync(path.join(path.dirname(out), ".rightkit-dmg-")), stage = path.join(work, "stage"), mount = path.join(work, "mount");
  const rw = path.join(work, "writable.dmg"), compressed = path.join(work, "compressed.dmg");
  let attached = false, attaching = false;
  const command = (tool, args) => {
    const result = run(tool, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], shell: false, timeout: 300000 });
    if (result.error || result.status !== 0) throw fail("DMG_COMMAND_FAILED", `${tool} ${args[0]} failed: ${result.error?.message ?? result.stderr ?? result.status}`);
    return result;
  };
  log({ event: "dmg-build-started", app, out, volname });
  try {
    mkdirSync(stage); mkdirSync(mount);
    command("/usr/bin/ditto", [app, path.join(stage, path.basename(app))]);
    symlinkSync("/Applications", path.join(stage, "Applications"));
    const relative = background ? path.join(".background", path.basename(background)) : null;
    if (relative) { mkdirSync(path.join(stage, ".background")); copyFileSync(background, path.join(stage, relative)); }
    // Include layout headroom in hdiutil's source-folder size estimate. Alias
    // CNIDs become available only after mounting, when this reservation is replaced.
    writeFileSync(path.join(stage, ".DS_Store"), Buffer.alloc(64 * 1024));
    command("/usr/bin/hdiutil", ["create", "-fs", "HFS+", "-format", "UDRW", "-volname", volname, "-srcfolder", stage, rw]);
    attaching = true;
    command("/usr/bin/hdiutil", ["attach", "-readwrite", "-nobrowse", "-noautoopen", "-mountpoint", mount, rw]);
    attached = true; attaching = false;
    if (volumeIcon) {
      // Custom volume icon: .VolumeIcon.icns at the root plus the volume's kHasCustomIcon (0x0400) Finder flag.
      copyFileSync(volumeIcon, path.join(mount, ".VolumeIcon.icns"));
      const finderInfo = Buffer.alloc(32); finderInfo.writeUInt16BE(0x0400, 8);
      command("/usr/bin/xattr", ["-wx", "com.apple.FinderInfo", finderInfo.toString("hex"), mount]);
    }
    const alias = relative ? makeAlias({ mount, volname, relative }) : undefined;
    writeFileSync(path.join(mount, ".DS_Store"), dmgDSStore({ appName: path.basename(app), iconSize,
      appPosition, applicationsPosition, window, alias }));
    command("/usr/bin/hdiutil", ["detach", mount]); attached = false;
    command("/usr/bin/hdiutil", ["convert", rw, "-format", "UDZO", "-imagekey", "zlib-level=9", "-o", compressed]);
    command("/usr/bin/hdiutil", ["verify", compressed]);
    renameSync(compressed, out);
    log({ event: "dmg-build-completed", out, format: "UDZO" });
    return { out, format: "UDZO" };
  } catch (error) {
    log({ event: "dmg-build-failed", out, code: error.code ?? "DMG_BUILD_FAILED", message: error.message });
    throw error;
  } finally {
    if (attached || attaching) {
      try { command("/usr/bin/hdiutil", ["detach", "-force", mount]); attached = false; attaching = false; }
      catch (error) { log({ event: "dmg-cleanup-failed", mount, work, message: error.message }); }
    }
    // Never recurse through a mount that failed to detach.
    if (!attached && !attaching) rmSync(work, { recursive: true, force: true });
  }
}

export function parseDmgArguments(args) {
  if (args[0] !== "build") throw fail("DMG_INVALID_OPTIONS", "expected dmg build");
  const flags = new Map([["--app", "app"], ["--out", "out"], ["--volname", "volname"], ["--background", "background"],
    ["--icon-size", "iconSize"], ["--app-position", "appPosition"], ["--applications-position", "applicationsPosition"], ["--window", "window"], ["--volume-icon", "volumeIcon"]]);
  const options = {};
  for (let i = 1; i < args.length; i += 2) {
    const key = flags.get(args[i]);
    if (!key || options[key] !== undefined || !args[i + 1] || args[i + 1].startsWith("--")) throw fail("DMG_INVALID_OPTIONS", `invalid option: ${args[i]}`);
    options[key] = args[i + 1];
  }
  return options;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { buildDmg(parseDmgArguments(process.argv.slice(2))); }
  catch (error) { console.error(`right-release dmg: [${error.code ?? "DMG_BUILD_FAILED"}] ${error.message}`); process.exitCode = 1; }
}
