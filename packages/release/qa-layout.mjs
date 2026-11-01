// Source QA against the gate-built QA shell, in the layout an install has (rightkit#1309 gap 2).
//
// A TOML-scenario launch of a Tauri app with a sidecar needs the test-mode engine and `resources/` BESIDE the UI binary, and the snapshot a package
// stage reads holds no Cargo target (and may not be written: the snapshot is exactly the commit's tree, GATE-CONTRACT §1). So `right-release qa --stage
// source` does not look for a binary in the source. The gate hands it the output directories its own compile recorded (RIGHT_RELEASE_QA_COMPILED_DIRS,
// set only after the recorded compile still proved intact), and this module builds a RUN-SCOPED directory, never inside the snapshot:
//
//   <run dir>/app/                          the overlay app root: every top-level entry of the snapshot's app root as a symlink (read-only view), the
//                                           qa config as a real COPY at the same relative path (its `app_root` resolves to the overlay, so the runner's
//                                           own writes, `.rightkit-qa-bin` included, land here), `.rightkit-qa-bin/<triple|host>/<bin>` the QA shell
//   <run dir>/app/.rightkit-qa-bin/<triple|host>/   the layout beside the shell: the sidecars and trees the app declared
//
// and the runner is pointed at the staged shell with RIGHTKIT_QA_APP_BINARY (already inside the stash, so it is launched as is, never copied again).
// The shell is the newest `[<triple>/]<debug|dev|release-iterate>/<bin>` in the compile outputs that carries the QA control marker; a `release` profile
// directory is never read. The layout is declared by the app in its release config:
//
//   qa.layout = {
//     binary: "heardright_next_bin",                              // cargo bin target of the QA shell
//     build: ["qa-build-shell", "qa-build-engine", "qa-sign"],    // tests.suites that build (and sign) the QA shell and what sits beside it
//     beside: [                                                    // staged beside the shell, in order
//       { built: "heardright-engine" },                            // another executable of the same compile output (same profile directory)
//       { built: "heardright-engine", optional: true },            // ... absent is allowed
//       { from: "src-tauri/resources", to: "resources" },          // a file or tree of the snapshot (a list names fallbacks: the first that exists)
//       { from: "src-tauri/fonts", to: "fonts", resource: true, platforms: ["mac"] },  // `platforms` (mac, win) limits an entry to those hosts
//       { from: "src-tauri/resources", to: ".", resource: true },  // ... into the app's RESOURCE directory, as an install has it: beside the exe on
//                                                                  //     Windows, `Contents/Resources` of the bundle rightkit-qa makes on macOS
//       { from: ["src-tauri/binaries/heardright-engine-aarch64-apple-darwin"], to: "heardright-engine" },
//     ],
//   }
//
// `build` names suites of the app's own `tests.suites` (the steps `--to test` runs): source QA runs them first, through the same door and signing policy
// (test-command.mjs runSuites), and the shell and `built` entries are then found in THOSE suites' output directories, never the release compile's. When a
// sign step is among them, the shell and every `built` entry are staged from its signed copies (rightkit#1303), and one it did not sign is refused.
// Without `build` the shell is looked for in the compile outputs the gate hands over.
// On macOS rightkit-qa wraps the stashed shell in a bundle and carries the staged layout into it (control.rs make_bundle): every entry beside the
// shell goes to `Contents/MacOS` (where an install keeps its externalBin sidecars), and the `Resources/` directory a `resource: true` entry is
// staged into goes to `Contents/Resources` (where Tauri's resource_dir() points in an install). On Windows the shell runs in place, its
// resource directory is its own directory, so a `resource: true` entry is staged beside it.
// `from` stays inside the snapshot app root (links resolved), `to` stays beside the shell. Scenario state (a seeded settings.json) is the scenario's
// own `write_file` step inside the runner's isolated data directory, not this layout. Every file staged is digested into the receipt.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { QA_CONTROL_MARKER } from "./qa-control-scan.mjs";
import { realish } from "./qa-snapshot.mjs";

export const QA_COMPILED_DIRS_ENV = "RIGHT_RELEASE_QA_COMPILED_DIRS";
/** A direct host's Cargo output directories the gate pinned for this stage (JSON list): what a suite built there lands in (test-command.mjs). */
export const GATE_TARGET_DIRS_ENV = "RIGHTKIT_GATE_TARGET_DIRS";
export const QA_APP_BINARY_ENV = "RIGHTKIT_QA_APP_BINARY";
export const STASH_DIR = ".rightkit-qa-bin";
/** The profile directories the runner reads for a dev build (binary.rs DEV_PROFILES); `release` is never one. */
export const DEV_PROFILES = Object.freeze(["release-iterate", "debug", "dev"]);

const LAYOUT_KEYS = new Set(["binary", "beside", "build"]);
const BESIDE_KEYS = new Set(["built", "from", "to", "optional", "resource", "platforms"]);
const LAYOUT_PLATFORMS = new Set(["mac", "win"]);
/** The directory beside a macOS shell that rightkit-qa's bundle maps to `Contents/Resources` (rightkit-qa control.rs MAC_RESOURCES_DIR). */
export const MAC_RESOURCES_DIR = "Resources";
const BARE_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;

const fail = (code, message) => Object.assign(new Error(`${code}: ${message}`), { code });
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const sha256Of = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const insideOrSame = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)); };
const cleanRel = (value) => (typeof value === "string" && value && !path.isAbsolute(value) && !/^[A-Za-z]:/.test(value) && !value.replaceAll("\\", "/").split("/").includes("..") ? value.replaceAll("\\", "/").replace(/\/+$/, "") || "." : null);

/** Problems with a `qa.layout` declaration, all at once; empty when it is well formed. */
export function validateQaLayout(layout) {
  if (!isObject(layout)) return ["qa.layout must be an object { binary, beside? }"];
  const problems = [];
  for (const key of Object.keys(layout)) if (!LAYOUT_KEYS.has(key)) problems.push(`qa.layout has unknown key ${JSON.stringify(key)} (allowed: ${[...LAYOUT_KEYS].join(", ")})`);
  if (typeof layout.binary !== "string" || !BARE_NAME.test(layout.binary) || /\.exe$/i.test(layout.binary)) problems.push("qa.layout.binary must be a bare cargo bin target name (no path, no .exe)");
  if (layout.beside !== undefined && !Array.isArray(layout.beside)) problems.push("qa.layout.beside must be a list");
  if (layout.build !== undefined && (!Array.isArray(layout.build) || !layout.build.length || layout.build.some((name) => typeof name !== "string" || !name) || new Set(layout.build).size !== layout.build.length)) problems.push("qa.layout.build must be a non-empty list of distinct tests.suites names");
  const tos = new Set();
  (Array.isArray(layout.beside) ? layout.beside : []).forEach((item, index) => {
    const at = `qa.layout.beside[${index}]`;
    if (!isObject(item)) { problems.push(`${at} must be an object`); return; }
    for (const key of Object.keys(item)) if (!BESIDE_KEYS.has(key)) problems.push(`${at} has unknown key ${JSON.stringify(key)} (allowed: ${[...BESIDE_KEYS].join(", ")})`);
    const hasBuilt = item.built !== undefined; const hasFrom = item.from !== undefined;
    if (hasBuilt === hasFrom) problems.push(`${at} needs exactly one of built or from`);
    if (hasBuilt && (typeof item.built !== "string" || !BARE_NAME.test(item.built) || /\.exe$/i.test(item.built))) problems.push(`${at}.built must be a bare cargo bin target name`);
    if (hasFrom) {
      const list = Array.isArray(item.from) ? item.from : [item.from];
      if (!list.length || list.some((from) => cleanRel(from) === null)) problems.push(`${at}.from must be a relative path (or a non-empty list of them) inside the app, without ..`);
      if (item.to === undefined) problems.push(`${at}.to is required with from`);
    }
    if (item.to !== undefined && cleanRel(item.to) === null) problems.push(`${at}.to must be a relative path beside the binary, without ..`);
    if (item.optional !== undefined && typeof item.optional !== "boolean") problems.push(`${at}.optional must be a boolean`);
    if (item.resource !== undefined && (item.resource !== true || !hasFrom)) problems.push(`${at}.resource must be true and only with from`);
    if (item.platforms !== undefined && (!Array.isArray(item.platforms) || !item.platforms.length || item.platforms.some((p) => !LAYOUT_PLATFORMS.has(p)))) problems.push(`${at}.platforms must be a non-empty list of ${[...LAYOUT_PLATFORMS].join(", ")}`);
    const to = hasBuilt ? (cleanRel(item.to) ?? item.built) : item.resource ? `${MAC_RESOURCES_DIR}/${cleanRel(item.to)}` : cleanRel(item.to);
    // Several `from` entries may fill one directory (a file both would write is refused when staged); a built executable is staged once.
    if (to && hasBuilt) { if (tos.has(to)) problems.push(`${at} stages ${to} a second time`); tos.add(to); }
  });
  return problems;
}

/** `RIGHT_RELEASE_QA_COMPILED_DIRS`: a JSON list of absolute directories; null when unset, a typed error when malformed. */
export function compiledDirsFromEnv(env = process.env) {
  const raw = env?.[QA_COMPILED_DIRS_ENV];
  if (raw === undefined || raw === "") return null;
  let dirs;
  try { dirs = JSON.parse(raw); } catch { throw fail("RK_QA_LAYOUT_DIRS", `${QA_COMPILED_DIRS_ENV} is not a JSON list`); }
  if (!Array.isArray(dirs) || !dirs.length || dirs.some((dir) => typeof dir !== "string" || !path.isAbsolute(dir))) throw fail("RK_QA_LAYOUT_DIRS", `${QA_COMPILED_DIRS_ENV} must be a non-empty JSON list of absolute directories`);
  return dirs;
}

const exeName = (name, platform) => (platform === "win" ? `${name}.exe` : name);
const hasMarker = (file) => {
  const bytes = fs.readFileSync(file);
  return bytes.includes(Buffer.from(QA_CONTROL_MARKER, "latin1")) || bytes.includes(Buffer.from(QA_CONTROL_MARKER, "utf16le"));
};
const listDirs = (dir) => { try { return fs.readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name); } catch { return []; } };

/** Every `<dir>/[<triple>/]<profile>/<name>` regular file under the compile output directories, newest first, with the profile directory and triple. */
export function findBuiltExecutables(dirs, name, platform) {
  const found = [];
  for (const dir of dirs) {
    const profileRoots = [{ root: dir, triple: "host" }, ...listDirs(dir).filter((n) => n.includes("-") && !DEV_PROFILES.includes(n)).map((n) => ({ root: path.join(dir, n), triple: n }))];
    for (const { root, triple } of profileRoots) {
      for (const profile of DEV_PROFILES) {
        const file = path.join(root, profile, exeName(name, platform));
        try { const meta = fs.statSync(file); if (meta.isFile()) found.push({ file, dir, profileDir: path.dirname(file), triple, mtimeMs: meta.mtimeMs }); } catch { /* not built here */ }
      }
    }
  }
  return found.sort((a, b) => b.mtimeMs - a.mtimeMs);
}

function copyTree(from, to, entries, relBase) {
  const meta = fs.lstatSync(from);
  if (meta.isSymbolicLink()) throw fail("RK_QA_LAYOUT_LINK", `${from} is a symbolic link; a staged layout copies only regular files`);
  if (meta.isDirectory()) {
    fs.mkdirSync(to, { recursive: true });
    for (const name of fs.readdirSync(from).sort()) copyTree(path.join(from, name), path.join(to, name), entries, `${relBase}/${name}`);
    return;
  }
  if (!meta.isFile()) throw fail("RK_QA_LAYOUT_KIND", `${from} is neither a file nor a directory`);
  fs.mkdirSync(path.dirname(to), { recursive: true });
  // Two entries filling one directory may not write the same file with other bytes.
  if (fs.existsSync(to)) {
    if (sha256Of(fs.readFileSync(to)) !== sha256Of(fs.readFileSync(from))) throw fail("RK_QA_LAYOUT_COLLISION", `${relBase} is staged twice with different bytes (${from})`);
    return; // the same bytes, already staged and recorded
  }
  fs.copyFileSync(from, to);
  fs.chmodSync(to, meta.mode & 0o777);
  entries.push({ path: relBase, sha256: sha256Of(fs.readFileSync(to)), bytes: meta.size });
}

/** A directory that mirrors `src` with every entry a symlink, except the directories on the way to `relFile`, which are real, and `relFile` itself (a copy). */
function mirror(src, dst, relParts, copyFile) {
  fs.mkdirSync(dst, { recursive: true });
  const [head, ...tail] = relParts;
  for (const name of fs.readdirSync(src)) {
    if (name === ".git" || name === STASH_DIR) continue;
    const from = path.join(src, name); const to = path.join(dst, name);
    if (name === head && tail.length) mirror(from, to, tail, copyFile);
    else if (name === head) copyFile(from, to);
    else fs.symlinkSync(from, to);
  }
}

/**
 * Stage the run-scoped overlay app root, the QA shell and its declared layout. Returns
 * `{ runDir, appRoot, configFile, binary, evidence, cleanup }`; `cleanup()` removes `runDir`. Nothing is written under `appRoot` of the snapshot.
 * `snapshotAppRoot` is the app root the release config lives in, `qaConfigFile` the absolute qa config inside it, `runsRoot` the gate's runs root
 * (outside every repository and snapshot), `dirs` the compile output directories. `signed` (a sign step's `{ "<path in its output dir>": "<signed copy>" }`)
 * makes the shell and every `built` entry come from their signed copies; a file it does not name is refused.
 */
export function stageQaLayout({ layout, snapshotAppRoot, qaConfigFile, runsRoot, dirs, platform, signed = null, now = () => Date.now() }) {
  const problems = validateQaLayout(layout);
  if (problems.length) throw fail("RK_QA_LAYOUT_INVALID", problems.join("; "));
  const sourceRoot = fs.realpathSync.native(snapshotAppRoot);
  const configReal = fs.realpathSync.native(qaConfigFile);
  if (!insideOrSame(configReal, sourceRoot)) throw fail("RK_QA_LAYOUT_CONFIG", `the qa config ${qaConfigFile} is outside the app root ${snapshotAppRoot}`);
  if (!path.isAbsolute(runsRoot) || insideOrSame(realish(runsRoot), sourceRoot)) throw fail("RK_QA_LAYOUT_PLACE", `the runs root ${runsRoot} must be an absolute directory outside the snapshot`);

  const candidates = findBuiltExecutables(dirs, layout.binary, platform);
  if (!candidates.length) throw fail("RK_QA_LAYOUT_NO_BINARY", `no ${exeName(layout.binary, platform)} in a ${DEV_PROFILES.join("/")} directory of the compile outputs (${dirs.join(", ")}); the compile did not build the QA shell`);
  // The bytes staged for a built file: its signed copy when a sign step ran, keyed by its path inside the output directory it was found in.
  const bytesOf = (file, dir) => {
    if (!signed) return file;
    const rel = path.relative(dir, file).split(path.sep).join("/");
    const copy = signed[rel];
    if (typeof copy !== "string" || !fs.existsSync(copy)) throw fail("RK_QA_LAYOUT_UNSIGNED", `${file} (${rel}) has no signed copy; the declared sign step signed ${Object.keys(signed).join(", ") || "nothing"}`);
    return copy;
  };
  const shell = candidates.find((candidate) => hasMarker(candidate.file));
  if (!shell) throw fail("RK_QA_LAYOUT_NOT_QA", `${candidates.map((c) => c.file).join(", ")} carry no QA control marker; the compile built a plain app, not the QA shell`);

  fs.mkdirSync(runsRoot, { recursive: true, mode: 0o700 });
  const runDir = fs.mkdtempSync(path.join(runsRoot, `qa-layout-${now().toString(36)}-`));
  const cleanup = () => { try { fs.rmSync(runDir, { recursive: true, force: true }); } catch { /* the runs root is pruned */ } };
  try {
    const appRoot = path.join(runDir, "app");
    const relConfig = path.relative(sourceRoot, configReal).split(path.sep);
    const configFile = path.join(appRoot, ...relConfig);
    mirror(sourceRoot, appRoot, relConfig, (from, to) => fs.copyFileSync(from, to));
    const besideDir = path.join(appRoot, STASH_DIR, shell.triple);
    fs.mkdirSync(besideDir, { recursive: true });
    const binary = path.join(besideDir, exeName(layout.binary, platform));
    fs.copyFileSync(bytesOf(shell.file, shell.dir), binary);
    fs.chmodSync(binary, 0o755);
    const evidence = { source: shell.file, ...(signed ? { signed: true } : {}), binary: { name: path.basename(binary), sha256: sha256Of(fs.readFileSync(binary)) }, qaConfig: { sha256: sha256Of(fs.readFileSync(configFile)) }, beside: [] };
    for (const item of layout.beside ?? []) {
      if (Array.isArray(item.platforms) && !item.platforms.includes(platform)) continue;
      const optional = item.optional === true;
      if (item.built !== undefined) {
        const from = path.join(shell.profileDir, exeName(item.built, platform));
        const toName = cleanRel(item.to) ?? exeName(item.built, platform);
        if (!fs.existsSync(from)) { if (optional) continue; throw fail("RK_QA_LAYOUT_MISSING", `layout names built executable ${item.built}, but ${from} is not in the QA shell's output directory`); }
        copyTree(bytesOf(from, shell.dir), path.join(besideDir, toName), evidence.beside, toName);
        continue;
      }
      const options = (Array.isArray(item.from) ? item.from : [item.from]).map(cleanRel);
      const pick = options.map((rel) => path.join(sourceRoot, rel)).find((file) => fs.existsSync(file));
      if (!pick) { if (optional) continue; throw fail("RK_QA_LAYOUT_MISSING", `layout source ${options.join(" or ")} does not exist in the snapshot`); }
      if (!insideOrSame(fs.realpathSync.native(pick), sourceRoot)) throw fail("RK_QA_LAYOUT_ESCAPE", `layout source ${pick} resolves outside the app root`);
      const rel = cleanRel(item.to);
      // A resource goes to the app's resource directory: `Resources/` (Contents/Resources of the macOS bundle) on macOS, beside the shell on Windows.
      const toName = item.resource && platform === "mac" ? path.posix.join(MAC_RESOURCES_DIR, rel) : rel;
      copyTree(pick, path.join(besideDir, toName), evidence.beside, toName);
    }
    evidence.beside.sort((a, b) => a.path.localeCompare(b.path));
    return { runDir, appRoot, configFile, binary, evidence, cleanup };
  } catch (error) { cleanup(); throw error; }
}
