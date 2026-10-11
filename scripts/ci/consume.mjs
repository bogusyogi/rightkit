// Consumer verification of the SHIPPED bytes, run by `publish.mjs verify` on each verify host. The gate
// tests workspace source; this proves the sealed artifacts themselves:
//   npm     install the sealed tarballs of the request TOGETHER (plus their already-published
//           dependencies, from the registry) into a scratch project with `npm install --ignore-scripts`,
//           then prove from the INSTALLED copy that every declared main/module/types/bin/exports target
//           exists, every exports subpath resolves, and every JS entry imports without throwing.
//   crates  extract the sealed .crate, check that the packaged Cargo.toml still yields the sealed upload
//           metadata, and `cargo check` the packaged crate (with --locked when it ships a Cargo.lock and
//           needs no patch). Sibling crates of the same request that are not on the registry yet are
//           patched in from their own sealed .crate.
// Nothing here publishes or holds a token; the caller has already refused any.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const fwd = (p) => p.replace(/\\/g, "/");
const tail = (text, n = 600) => String(text ?? "").trim().slice(-n);

/** How to run npm without a shell: node + npm-cli.js beside the running node, else the npm shim. */
export function npmCliCommand({ execPath = process.execPath, platform = process.platform, exists = fs.existsSync } = {}) {
  const candidates = [
    path.join(path.dirname(execPath), "node_modules", "npm", "bin", "npm-cli.js"),
    path.join(path.dirname(execPath), "..", "lib", "node_modules", "npm", "bin", "npm-cli.js"),
  ];
  const cli = candidates.find((c) => exists(c));
  if (cli) return { command: execPath, prefix: [cli], shell: false };
  return { command: platform === "win32" ? "npm.cmd" : "npm", prefix: [], shell: platform === "win32" };
}

// ---- npm --------------------------------------------------------------------------------------

/**
 * What a package.json promises. Returns { subpaths: [{ subpath, spec, runtime: [files], types: [files] }],
 * patterns: [subpath], files: [every non-pattern file the manifest points at], bins, hasEntry }.
 */
export function declaredEntries(pkg) {
  const rel = (v) => (typeof v === "string" && v.startsWith("./") ? v.slice(2) : typeof v === "string" && v && !v.startsWith("/") ? v : "");
  const files = new Set(); const bins = [];
  const addFile = (v) => { const r = rel(v); if (r && !r.includes("*")) files.add(r); };
  for (const key of ["main", "module", "types", "typings"]) addFile(pkg[key]);
  if (typeof pkg.bin === "string") { addFile(pkg.bin); bins.push(rel(pkg.bin)); }
  else if (pkg.bin && typeof pkg.bin === "object") for (const v of Object.values(pkg.bin)) { addFile(v); bins.push(rel(v)); }

  const collect = (node, underTypes, out) => {
    if (typeof node === "string") { const r = rel(node); if (r) (underTypes ? out.types : out.runtime).push(r); }
    else if (Array.isArray(node)) node.forEach((n) => collect(n, underTypes, out));
    else if (node && typeof node === "object") for (const [k, v] of Object.entries(node)) collect(v, underTypes || k === "types", out);
  };
  const subpaths = []; const patterns = [];
  const exp = pkg.exports;
  if (exp !== undefined && exp !== null) {
    const map = typeof exp === "object" && !Array.isArray(exp) && Object.keys(exp).some((k) => k.startsWith(".")) ? exp : { ".": exp };
    for (const [subpath, target] of Object.entries(map)) {
      if (subpath.includes("*")) { patterns.push(subpath); continue; }
      const out = { runtime: [], types: [] };
      collect(target, false, out);
      for (const f of [...out.runtime, ...out.types]) if (!f.includes("*")) files.add(f);
      subpaths.push({ subpath, spec: subpath === "." ? pkg.name : `${pkg.name}/${subpath.slice(2)}`, runtime: out.runtime, types: out.types });
    }
  } else if (pkg.main) subpaths.push({ subpath: ".", spec: pkg.name, runtime: [rel(pkg.main)], types: [] });
  return { subpaths, patterns, files: [...files], bins, hasEntry: subpaths.length > 0 || files.size > 0 };
}

/** `rightkit.publishCheck.skipImport`: subpaths a package declares cannot be imported in plain Node (DOM or Tauri host only). */
export function skipImportList(pkg, declared) {
  const raw = pkg?.rightkit?.publishCheck?.skipImport;
  if (raw === undefined) return { list: [], problems: [] };
  if (!Array.isArray(raw) || raw.some((v) => typeof v !== "string")) return { list: [], problems: [`${pkg.name}: rightkit.publishCheck.skipImport must be an array of exports subpaths`] };
  const known = new Set(declared.subpaths.map((s) => s.subpath));
  const stray = raw.filter((v) => !known.has(v));
  return { list: raw, problems: stray.length ? [`${pkg.name}: rightkit.publishCheck.skipImport names ${stray.join(", ")}, which package.json does not export`] : [] };
}

const CHECK_SCRIPT = `import fs from "node:fs";
const plan = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const out = [];
for (const item of plan.items) {
  const row = { spec: item.spec, ok: false, imported: false };
  try {
    const url = import.meta.resolve(item.spec);
    row.resolved = url;
    if (item.import && /\\.(?:[cm]?js)$/.test(new URL(url).pathname)) { await import(item.spec); row.imported = true; }
    row.ok = true;
  } catch (error) { row.error = String(error && error.stack ? error.stack : error).split("\\n").slice(0, 4).join(" | ").slice(0, 500); }
  out.push(row);
}
process.stdout.write("RK_CONSUME " + JSON.stringify(out) + "\\n");
`;

/**
 * Install the sealed tarballs together into a scratch project and check each from its installed copy.
 * Returns Map(unit name -> { problems: [], report }). Never throws for a package defect.
 */
export function consumeNpm({ units, packDir, scratchRoot = os.tmpdir(), spawn = spawnSync, env = process.env, npm = npmCliCommand(), node = process.execPath }) {
  const results = new Map(units.map((u) => [u.name, { problems: [], report: {} }]));
  const scratch = fs.mkdtempSync(path.join(scratchRoot, "rk-consume-npm-"));
  try {
    fs.writeFileSync(path.join(scratch, "package.json"), `${JSON.stringify({ name: "rk-consumer", version: "0.0.0", private: true, type: "module" })}\n`);
    const userconfig = path.join(scratch, "npmrc"); fs.writeFileSync(userconfig, "");
    const childEnv = { ...env, npm_config_cache: path.join(scratch, "cache"), npm_config_userconfig: userconfig, npm_config_update_notifier: "false", npm_config_fund: "false", npm_config_audit: "false" };
    const install = spawn(npm.command, [...npm.prefix, "install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock", ...units.map((u) => path.resolve(packDir, u.file))], { cwd: scratch, encoding: "utf8", env: childEnv, shell: npm.shell, maxBuffer: 64 * 1024 * 1024, timeout: 20 * 60_000 });
    if (install.error || install.status !== 0) {
      const why = `npm install --ignore-scripts of the sealed tarballs failed: ${tail(install.stderr || install.error?.message)}`;
      for (const r of results.values()) r.problems.push(why);
      return results;
    }
    for (const unit of units) {
      const out = results.get(unit.name);
      const dir = path.join(scratch, "node_modules", ...unit.name.split("/"));
      let pkg;
      try { pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8")); } catch { out.problems.push(`${unit.name}: not installed from the sealed tarball (no node_modules/${unit.name}/package.json)`); continue; }
      if (pkg.name !== unit.name || pkg.version !== unit.version) { out.problems.push(`${unit.name}: installed ${pkg.name}@${pkg.version}, sealed ${unit.version}`); continue; }
      const declared = declaredEntries(pkg);
      const skip = skipImportList(pkg, declared);
      out.problems.push(...skip.problems);
      for (const file of declared.files) if (!fs.existsSync(path.join(dir, file))) out.problems.push(`${unit.name}: installed package.json points at ${file}, which the installed package does not contain`);
      const items = declared.subpaths
        .filter((s) => s.runtime.length > 0)
        .map((s) => ({ spec: s.spec, subpath: s.subpath, import: !skip.list.includes(s.subpath) }));
      const report = { entries: items.length, imported: [], resolvedOnly: [], skippedImport: skip.list, patterns: declared.patterns, bins: declared.bins, filesChecked: declared.files.length, noJsEntry: items.length === 0 };
      out.report = report;
      if (items.length === 0) continue;
      const scriptFile = path.join(scratch, `check-${unit.name.replace(/[^A-Za-z0-9]+/g, "_")}.mjs`);
      const planFile = `${scriptFile}.json`;
      fs.writeFileSync(scriptFile, CHECK_SCRIPT);
      fs.writeFileSync(planFile, JSON.stringify({ items }));
      const ran = spawn(node, [scriptFile, planFile], { cwd: scratch, encoding: "utf8", env: childEnv, maxBuffer: 64 * 1024 * 1024, timeout: 5 * 60_000 });
      const line = String(ran.stdout ?? "").split(/\r?\n/).find((l) => l.startsWith("RK_CONSUME "));
      if (!line) { out.problems.push(`${unit.name}: the import check produced no result (status ${ran.status}): ${tail(ran.stderr || ran.error?.message)}`); continue; }
      let rows;
      try { rows = JSON.parse(line.slice("RK_CONSUME ".length)); } catch { out.problems.push(`${unit.name}: unreadable import check result`); continue; }
      for (const row of rows) {
        if (!row.ok) out.problems.push(`${unit.name}: ${row.spec} ${row.imported ? "imported" : "did not resolve or import"}: ${row.error ?? "unknown error"}`);
        else if (row.imported) report.imported.push(row.spec);
        else report.resolvedOnly.push(row.spec);
      }
      if (rows.length !== items.length) out.problems.push(`${unit.name}: import check covered ${rows.length} of ${items.length} entries`);
    }
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  return results;
}

// ---- crates -----------------------------------------------------------------------------------

/**
 * Check each sealed .crate as a consumer would get it. `uploadMetadata(pkg, readmeText)` is the same
 * function pack used, so the packaged manifest must still reproduce the sealed `.upload.json` exactly.
 * `excluded` names the units this host is allowed not to build (allow_host_exclusion). Returns
 * Map(unit name -> { problems: [], report }).
 */
export function consumeCrates({ units, packDir, scratchRoot = os.tmpdir(), spawn = spawnSync, env = process.env, excluded = [], uploadMetadata }) {
  const results = new Map(units.map((u) => [u.name, { problems: [], report: {} }]));
  const scratch = fs.mkdtempSync(path.join(scratchRoot, "rk-consume-crates-"));
  try {
    const dirs = new Map(); const pkgs = new Map();
    for (const unit of units) {
      const out = results.get(unit.name);
      const dest = path.join(scratch, "src", unit.name);
      fs.mkdirSync(dest, { recursive: true });
      const untar = spawn("tar", ["-xzf", path.basename(unit.file), "-C", dest], { cwd: packDir, encoding: "utf8", env });
      const inner = path.join(dest, `${unit.name}-${unit.version}`);
      if (untar.error || untar.status !== 0 || !fs.existsSync(inner)) { out.problems.push(`${unit.name}: cannot extract ${unit.file}`); continue; }
      dirs.set(unit.name, inner);
      const meta = spawn("cargo", ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", path.join(inner, "Cargo.toml")], { cwd: inner, encoding: "utf8", env, maxBuffer: 256 * 1024 * 1024 });
      let pkg;
      try { if (meta.error || meta.status !== 0) throw new Error(tail(meta.stderr || meta.error?.message)); pkg = JSON.parse(meta.stdout).packages.find((p) => p.name === unit.name && p.version === unit.version); } catch (error) { out.problems.push(`${unit.name}: cargo metadata of the packaged Cargo.toml failed: ${error.message}`); continue; }
      if (!pkg) { out.problems.push(`${unit.name}: packaged Cargo.toml does not describe ${unit.name}@${unit.version}`); continue; }
      pkgs.set(unit.name, pkg);
      try {
        const readme = pkg.readme ? fs.readFileSync(path.join(inner, pkg.readme), "utf8") : null;
        const derived = `${JSON.stringify(uploadMetadata(pkg, readme))}\n`;
        const sealed = fs.readFileSync(path.join(packDir, unit.metaFile), "utf8");
        if (derived !== sealed) out.problems.push(`${unit.name}: the sealed upload metadata (${unit.metaFile}) does not match the packaged Cargo.toml and README`);
      } catch (error) { out.problems.push(`${unit.name}: cannot compare upload metadata with the packaged manifest: ${error.message}`); }
    }
    const targetDir = path.join(scratch, "target");
    for (const unit of units) {
      const out = results.get(unit.name);
      if (excluded.includes(unit.name)) { out.report = { checked: false, reason: "allowed host exclusion (allow_host_exclusion)" }; continue; }
      const inner = dirs.get(unit.name); const pkg = pkgs.get(unit.name);
      if (!inner || !pkg) continue;
      const siblings = [...new Set((pkg.dependencies ?? []).map((d) => d.name))].filter((n) => n !== unit.name && dirs.has(n));
      const patches = siblings.flatMap((n) => ["--config", `patch.crates-io.${n}.path="${fwd(dirs.get(n))}"`]);
      const locked = patches.length === 0 && fs.existsSync(path.join(inner, "Cargo.lock"));
      const args = ["check", ...(locked ? ["--locked"] : []), "--manifest-path", path.join(inner, "Cargo.toml"), ...patches];
      const ran = spawn("cargo", args, { cwd: inner, encoding: "utf8", env: { ...env, CARGO_TARGET_DIR: targetDir, CARGO_TERM_COLOR: "never" }, maxBuffer: 256 * 1024 * 1024, timeout: 60 * 60_000 });
      out.report = { checked: true, locked, patchedSiblings: siblings, hasCargoLock: fs.existsSync(path.join(inner, "Cargo.lock")) };
      if (ran.error || ran.status !== 0) out.problems.push(`${unit.name}: cargo check of the packaged crate failed: ${tail(ran.stderr || ran.error?.message, 1500)}`);
    }
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  return results;
}
