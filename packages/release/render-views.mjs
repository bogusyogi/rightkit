// `rightkit-gate --to render`: build an app's declared view-render target through the broker and run it headless, storing the images and hashes as
// evidence (rightkit#1288; Pulse's notch long-text cases, bogusyogi/pulse#558). The app declares the target in the commit's own `ci/gate.config.json`:
//
//   "renderViews": {
//     "<app dir relative to the repository root, \".\" for the root>": {
//       "cargoDir": "src-tauri",              // the Cargo workspace directory that holds the package (repository-relative; default ".")
//       "package": "pulse-app",               // cargo package that builds the render binary
//       "bin": "pulse-app",                   // the binary (default: the package name)
//       "features": ["render-views"],         // optional: features the render target needs
//       "args": ["--render-views"],           // optional: fixed arguments placed before each case's
//       "caseFlag": "--case", "outFlag": "--out",   // optional: the flags that name one case and the output directory (these defaults)
//       "cases": ["send-message-long", "tooltip-send-long-text"],
//       "timeoutMs": 300000                   // optional: the limit for each case (default 5 minutes)
//     }
//   }
//
// The binary is run once per case as `<exe> <args...> <caseFlag> <case> <outFlag> <dir>` and must write `<dir>/<case>.png`. The compile is a carried
// build (the broker on Mac and Dell, the runtime cargo wrapper direct on Linux); the executable it reports must lie inside the output directory that
// build's verified receipt names, and it runs OUTSIDE the broker, like a test executable (GATE-CONTRACT GC-A08). The receipt `render-receipt.json`
// (schema rightkit.render-views/1) lists every case's PNG with its sha256, size and dimensions, the executable's sha256, the commit and tree, and
// the host. Nothing is mocked: the target renders the app's real views.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const RENDER_RECEIPT_SCHEMA = "rightkit.render-views/1";
export const RENDER_RECEIPT_FILE = "render-receipt.json";
export const DEFAULT_RENDER_TIMEOUT_MS = 5 * 60 * 1000;
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const FEATURE = /^[A-Za-z0-9][A-Za-z0-9_/.-]*$/;
const FLAG = /^--?[A-Za-z][A-Za-z0-9-]*$/;
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

/**
 * The declared render target of the app at `appDir` in a parsed `ci/gate.config.json`, normalised, or `{ none: true }` when it declares none, or
 * `{ problems }` when the block is malformed (every problem at once; nothing is guessed).
 */
export function renderViewsOf(gateConfig, appDir = ".") {
  const block = gateConfig?.renderViews;
  if (block === undefined) return { none: true };
  if (!isObject(block)) return { problems: ["ci/gate.config.json renderViews must be an object keyed by app directory"] };
  const spec = block[appDir];
  if (spec === undefined) return { none: true };
  const where = `renderViews[${JSON.stringify(appDir)}]`;
  const problems = [];
  if (!isObject(spec)) return { problems: [`${where} must be an object`] };
  const known = ["cargoDir", "package", "bin", "features", "args", "caseFlag", "outFlag", "cases", "timeoutMs"];
  for (const key of Object.keys(spec)) if (!known.includes(key)) problems.push(`${where}: unknown key ${JSON.stringify(key)} (allowed: ${known.join(", ")})`);
  const cargoDir = spec.cargoDir ?? ".";
  if (typeof cargoDir !== "string" || !cargoDir || cargoDir.startsWith("/") || cargoDir.includes("\\") || cargoDir.split("/").includes("..") || /^[A-Za-z]:/.test(cargoDir)) problems.push(`${where}.cargoDir must be a repository-relative directory`);
  if (typeof spec.package !== "string" || !NAME.test(spec.package)) problems.push(`${where}.package must be a cargo package name`);
  const bin = spec.bin ?? spec.package;
  if (typeof bin !== "string" || !NAME.test(bin)) problems.push(`${where}.bin must be a binary name`);
  const features = spec.features ?? [];
  if (!Array.isArray(features) || !features.every((f) => typeof f === "string" && FEATURE.test(f))) problems.push(`${where}.features must be a list of feature names`);
  const args = spec.args ?? [];
  if (!Array.isArray(args) || !args.every((a) => typeof a === "string" && a !== "" && !/[\0\r\n]/.test(a))) problems.push(`${where}.args must be a list of non-empty strings`);
  const caseFlag = spec.caseFlag ?? "--case"; const outFlag = spec.outFlag ?? "--out";
  for (const [key, value] of [["caseFlag", caseFlag], ["outFlag", outFlag]]) if (typeof value !== "string" || !FLAG.test(value)) problems.push(`${where}.${key} must be a command-line flag`);
  const cases = spec.cases;
  if (!Array.isArray(cases) || !cases.length || !cases.every((c) => typeof c === "string" && NAME.test(c)) || new Set(cases).size !== cases.length) problems.push(`${where}.cases must be a non-empty list of distinct case names ([A-Za-z0-9._-])`);
  const timeoutMs = spec.timeoutMs ?? DEFAULT_RENDER_TIMEOUT_MS;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 60 * 60 * 1000) problems.push(`${where}.timeoutMs must be 1000 to 3600000`);
  if (problems.length) return { problems };
  return { appDir, cargoDir, package: spec.package, bin, features: [...features], args: [...args], caseFlag, outFlag, cases: [...cases], timeoutMs };
}

/** The cargo argv that builds the render binary (the carried seam adds `--locked`). */
export const renderBuildArgs = (target) => ["build", "-p", target.package, "--bin", target.bin, "--message-format=json", ...(target.features.length ? ["--features", target.features.join(",")] : [])];

/** The executable cargo's JSON build output reports for `bin` of `package`, or null. */
export function builtExecutable(stdout, { package: pkg, bin }) {
  let found = null;
  for (const line of String(stdout ?? "").split("\n")) {
    if (!line.startsWith("{")) continue;
    let rec; try { rec = JSON.parse(line); } catch { continue; }
    if (rec?.reason !== "compiler-artifact" || typeof rec.executable !== "string" || !rec.executable) continue;
    const kinds = Array.isArray(rec.target?.kind) ? rec.target.kind : [];
    if (rec.target?.name === bin && kinds.includes("bin") && (typeof rec.package_id !== "string" || rec.package_id.includes(pkg))) found = rec.executable;
  }
  return found;
}

const within = (dir, file) => { const r = path.relative(dir, file); return r !== "" && !r.startsWith("..") && !path.isAbsolute(r); };

/** Width and height from a PNG's IHDR, or null when `bytes` is not a PNG. */
export function pngSize(bytes) {
  if (bytes.length < 24 || !bytes.subarray(0, 8).equals(PNG_SIGNATURE) || bytes.subarray(12, 16).toString("latin1") !== "IHDR") return null;
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

/**
 * Build and run the declared render target, then write the evidence. `targetDir` is the checkout's pinned target directory of the workspace on a direct host
 * (a broker host's is the verified receipt's). Injected: `runCargo(args, { cwd, env })` (the carried seam: a spawnSync-shaped result
 * with `outputDir` and `requestId` when the call verified), `runExecutable(exe, args, { cwd, env, timeoutMs })` (a spawnSync-shaped result, started hidden)
 * and the file system. Returns `{ ok, problems, receiptFile?, receipt? }`; every failure is a problem, nothing is partial-passed.
 */
export function renderViews({ target, snapshotRoot, commit, tree, outDir, env, platform, arch, targetDir = null, runCargo, runExecutable, fsOps = fs, now = () => new Date(), log = () => {} }) {
  const problems = [];
  const startedAt = now().toISOString();
  const cwd = path.join(snapshotRoot, ...(target.cargoDir === "." ? [] : target.cargoDir.split("/")));
  const built = runCargo(renderBuildArgs(target), { cwd, env });
  if (built.error || built.status !== 0) return { ok: false, problems: [`the render target's build failed (${built.error?.message ?? `exit ${built.status}`}): cargo ${renderBuildArgs(target).join(" ")}\n${String(built.stderr ?? "").trim().split("\n").slice(-40).join("\n")}`] };
  const exe = builtExecutable(built.stdout, target);
  if (!exe) return { ok: false, problems: [`the build reported no executable for ${target.package}'s binary ${target.bin}`] };
  // The binary runs outside the broker, so it must be the one the verified build wrote: inside that receipt's output directory (a direct host: the pinned target directory).
  const outputDir = typeof built.outputDir === "string" ? built.outputDir : (typeof targetDir === "string" && targetDir ? targetDir : null);
  if (!outputDir) return { ok: false, problems: ["the build has no verified output directory to contain its executable (no carried receipt names one and the gate pinned no target directory); nothing was run"] };
  let real = exe; let realOut = outputDir;
  try { real = fsOps.realpathSync(exe); realOut = fsOps.realpathSync(outputDir); } catch { /* judged as given below */ }
  if (!within(realOut, real) && !within(outputDir, exe)) return { ok: false, problems: [`the executable ${exe} is outside the build's output directory ${outputDir}; nothing was run`] };
  let exeBytes;
  try { exeBytes = fsOps.readFileSync(real); } catch (error) { return { ok: false, problems: [`cannot read the built executable ${exe} (${error.message})`] }; }
  const exeSha256 = sha256(exeBytes);
  fsOps.mkdirSync(outDir, { recursive: true });
  const cases = [];
  for (const name of target.cases) {
    const caseDir = path.join(outDir, name);
    fsOps.mkdirSync(caseDir, { recursive: true });
    log(`render case ${name}`);
    const ran = runExecutable(real, [...target.args, target.caseFlag, name, target.outFlag, caseDir], { cwd: snapshotRoot, env, timeoutMs: target.timeoutMs });
    if (ran.error || ran.status !== 0) { problems.push(`case ${name}: the render target ${ran.error ? `did not finish (${ran.error.message})` : `exited ${ran.status}`}\n${String(ran.stderr ?? "").trim().split("\n").slice(-20).join("\n")}`); continue; }
    const file = path.join(caseDir, `${name}.png`);
    let bytes;
    try { bytes = fsOps.readFileSync(file); } catch { problems.push(`case ${name}: the target wrote no ${name}.png in ${caseDir}`); continue; }
    const size = pngSize(bytes);
    if (!size || size.width < 1 || size.height < 1) { problems.push(`case ${name}: ${file} is not a PNG image`); continue; }
    cases.push({ name, png: path.relative(outDir, file).split(path.sep).join("/"), sha256: sha256(bytes), bytes: bytes.length, width: size.width, height: size.height });
  }
  if (problems.length) return { ok: false, problems };
  const receipt = {
    schema: RENDER_RECEIPT_SCHEMA, commit, tree, appDir: target.appDir, package: target.package, bin: target.bin, features: target.features,
    executable: { path: exe, sha256: exeSha256 }, buildOutputDir: outputDir, ...(typeof built.requestId === "string" ? { buildRequestId: built.requestId } : {}),
    host: { platform, arch }, startedAt, endedAt: now().toISOString(), cases,
  };
  const receiptFile = path.join(outDir, RENDER_RECEIPT_FILE);
  fsOps.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
  return { ok: true, problems: [], receiptFile, receipt };
}
