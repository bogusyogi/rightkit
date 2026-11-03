#!/usr/bin/env node
// `right-release stage-xcframework --crate <dir> --targets <triple>[,<triple>...] --headers <dir> --out <Name.xcframework>` (rightkit#1234).
//
// A RightKit-owned way to turn a Rust staticlib crate into an iOS xcframework (Pulse: swift/MeshLink/RightKitMeshFFI.xcframework from
// rightkit-mesh-ffi), usable as a `development.targets.ios.prepare` step and as an `iosPackage.prepare` step. It replaces a hand-made, git-ignored product:
//   1. every target is compiled through RustRight's managed Cargo seam (`rustright cargo build --locked`, the same call stage-sidecar makes: through the
//      broker on Mac and Dell, one output directory, the release profile - GC-A15, there is no dev or debug output), and the `.a` is read from Cargo's own
//      JSON artifact stream (never a hard-coded broker path);
//   2. `xcodebuild -create-xcframework` assembles one slice per target with the headers directory;
//   3. a receipt `<out>.receipt.json` beside the product records the crate, each target's library path and sha256, the headers digest and the xcframework's
//      Info.plist sha256, so the product is never a stale copy nobody can date.
// Output `--out` is replaced whole (a stale xcframework is removed first); the crate, headers and output paths must lie inside the working directory's repository.
import { createHash } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { oneOutput } from "./cross-targets.mjs";
import { defaultRun } from "./stage-sidecar.mjs";
import { spawnSync } from "./background-process.mjs";

export class StageXcframeworkError extends Error {
  constructor(message) { super(message); this.name = "StageXcframeworkError"; this.code = "RK_STAGE_XCFRAMEWORK"; }
}
const fail = (message) => { throw new StageXcframeworkError(message); };

export const XCFRAMEWORK_TARGETS = Object.freeze(["aarch64-apple-ios", "aarch64-apple-ios-sim", "x86_64-apple-ios"]);

export function parseStageXcframeworkArgs(argv) {
  const opts = { crate: null, targets: [], headers: null, out: null, package: null };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (!["--crate", "--targets", "--headers", "--out", "--package"].includes(flag)) fail(`unknown argument: ${flag}`);
    if (value === undefined || value.startsWith("--")) fail(`${flag} needs a value`);
    i += 1;
    if (flag === "--targets") opts.targets = value.split(",").map((item) => item.trim()).filter(Boolean);
    else opts[flag.slice(2)] = value;
  }
  for (const key of ["crate", "headers", "out"]) if (!opts[key]) fail(`--${key} is required`);
  if (!opts.targets.length) fail("--targets is required (for example aarch64-apple-ios,aarch64-apple-ios-sim)");
  const bad = opts.targets.filter((target) => !XCFRAMEWORK_TARGETS.includes(target));
  if (bad.length) fail(`unsupported target(s) ${bad.join(", ")} (allowed: ${XCFRAMEWORK_TARGETS.join(", ")})`);
  if (new Set(opts.targets).size !== opts.targets.length) fail("--targets lists a target twice");
  if (opts.targets.includes("aarch64-apple-ios-sim") && opts.targets.includes("x86_64-apple-ios")) fail("aarch64-apple-ios-sim and x86_64-apple-ios are both simulator slices; an xcframework holds one simulator library (lipo them first)");
  if (!/\.xcframework$/.test(opts.out)) fail("--out must end in .xcframework");
  return opts;
}

/** The staticlib Cargo reports in its JSON stream for the crate at `manifest` (the last matching compiler-artifact), or undefined. */
export function staticlibFromStream(stdout, manifest, packageName) {
  let found;
  for (const line of String(stdout ?? "").split("\n")) {
    if (!line.startsWith("{")) continue;
    let message;
    try { message = JSON.parse(line); } catch { continue; }
    if (message.reason !== "compiler-artifact" || !(message.target?.kind ?? []).includes("staticlib")) continue;
    if (packageName ? message.target?.name !== packageName.replaceAll("-", "_") && message.target?.name !== packageName : path.resolve(String(message.manifest_path ?? "")) !== path.resolve(manifest)) continue;
    const library = (message.filenames ?? []).find((file) => file.endsWith(".a"));
    if (library) found = library;
  }
  return found;
}

const sha256File = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

function digestTree(dir, prefix = "") {
  const hash = createHash("sha256");
  for (const name of readdirSync(path.join(dir, prefix)).sort()) {
    const relative = prefix ? `${prefix}/${name}` : name;
    const full = path.join(dir, relative);
    if (lstatSync(full).isDirectory()) hash.update(`d ${relative}\n${digestTree(dir, relative)}\n`);
    else hash.update(`f ${relative} ${sha256File(full)}\n`);
  }
  return hash.digest("hex");
}

/** Build every slice and assemble the xcframework. Injectable: run (rustright), xcodebuild (spawnSync-shaped), fs pieces, log, cwd, platform. */
export function stageXcframework(opts, { run = defaultRun, xcodebuild = spawnSync, log = (line) => console.log(line), cwd = process.cwd(), platform = process.platform, env = process.env } = {}) {
  if (platform !== "darwin") fail("stage-xcframework runs on macOS (xcodebuild assembles the xcframework)");
  const crate = path.resolve(cwd, opts.crate);
  const manifest = path.join(crate, "Cargo.toml");
  const headers = path.resolve(cwd, opts.headers);
  const out = path.resolve(cwd, opts.out);
  if (!existsSync(manifest)) fail(`no Cargo.toml in the crate directory ${crate}`);
  if (!existsSync(headers) || !lstatSync(headers).isDirectory()) fail(`headers directory not found: ${headers}`);
  if (!readdirSync(headers).length) fail(`headers directory ${headers} is empty`);
  const inside = (file) => { const relative = path.relative(path.resolve(cwd), file); return relative !== "" && !relative.startsWith("..") && !path.isAbsolute(relative); };
  if (!inside(out)) fail(`--out ${out} must be inside the working directory`);
  const libraries = [];
  for (const target of opts.targets) {
    const args = oneOutput(["cargo", "build", "--locked", "--manifest-path", manifest, ...(opts.package ? ["-p", opts.package] : []), "--target", target, "--lib", "--message-format=json-render-diagnostics"]);
    log(`stage-xcframework: rustright ${args.join(" ")}`);
    const built = run("rustright", args, { cwd: crate, env });
    if (built?.error || built?.status !== 0) fail(`the build of ${path.basename(crate)} for ${target} failed (${built?.error?.message ?? `exit ${built?.status}`})`);
    const library = staticlibFromStream(built.stdout, manifest, opts.package);
    if (!library || !existsSync(library)) fail(`Cargo reported no staticlib for ${path.basename(crate)} (${target}); set crate-type = ["staticlib"] in its [lib]`);
    libraries.push({ target, source: library, sha256: sha256File(library) });
  }
  rmSync(out, { recursive: true, force: true });
  mkdirSync(path.dirname(out), { recursive: true });
  const created = xcodebuild("xcodebuild", ["-create-xcframework", ...libraries.flatMap((item) => ["-library", item.source, "-headers", headers]), "-output", out], { encoding: "utf8", windowsHide: true, shell: false });
  if (created?.error || created?.status !== 0) { rmSync(out, { recursive: true, force: true }); fail(`xcodebuild -create-xcframework failed (${created?.error?.message ?? `exit ${created?.status}`}): ${String(created?.stderr ?? created?.stdout ?? "").trim()}`); }
  const plist = path.join(out, "Info.plist");
  if (!existsSync(plist)) { rmSync(out, { recursive: true, force: true }); fail(`xcodebuild reported success but ${plist} does not exist`); }
  const receipt = { schema: 1, crate: path.relative(cwd, crate).split(path.sep).join("/") || ".", targets: libraries.map(({ target, sha256 }) => ({ target, sha256 })), headersSha256: digestTree(headers), xcframeworkInfoPlistSha256: sha256File(plist), profile: "release" };
  writeFileSync(`${out}.receipt.json`, `${JSON.stringify(receipt, null, 2)}\n`);
  log(`stage-xcframework: wrote ${out} (${libraries.length} slice(s)) and ${path.basename(out)}.receipt.json`);
  return { out, receipt };
}

export function main(argv = process.argv.slice(2)) {
  try { stageXcframework(parseStageXcframeworkArgs(argv)); return 0; } catch (error) {
    console.error(`right-release stage-xcframework: ${error.message}`);
    return error instanceof StageXcframeworkError ? 1 : 2;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
