import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { previousPathOf, stagingPathOf, swapInPlace } from "./in-place-install.mjs";

// The scheduled disk job must never bind to a pnpm store, a pnpm global hash directory or a checkout: those move or vanish
// on upgrade. `disk schedule install` copies the installed package into this per-user root and binds the job to it.
// See docs/architecture/development-installed-product-boundary.md (installed bindings use a stable `current` root).

const pathFor = (platform) => (platform === "win32" || platform === "win" ? path.win32 : path.posix);

export function stableRootPath({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  if (platform === "win32" || platform === "win") {
    if (!env.LOCALAPPDATA) throw new Error("disk: LOCALAPPDATA is not set");
    return path.win32.join(env.LOCALAPPDATA, "Orthic Labs", "rightkit-release", "current");
  }
  return path.posix.join(home, "Library", "Application Support", "Orthic Labs", "rightkit-release", "current");
}

export function stableCliPath(stable, platform = process.platform) {
  return pathFor(platform).join(stable, "node_modules", "@rightkit", "release", "cli", "right-release.mjs");
}

export function isUnderStable(file, stable, platform = process.platform) {
  const win = platform === "win32" || platform === "win";
  const n = (v) => (win ? v.toLowerCase().replace(/\//g, "\\") : v);
  const prefix = n(stable).replace(/[\\/]+$/, "") + (win ? "\\" : "/");
  return n(file).startsWith(prefix);
}

const readJson = (file) => { try { return JSON.parse(readFileSync(file, "utf8")); } catch { return null; } };

// Walk up from `from` looking for `node_modules/<name>` (or `<name>` directly inside a node_modules directory).
function findDependency(name, from) {
  let dir = from;
  for (;;) {
    for (const candidate of [path.join(dir, "node_modules", name), path.basename(dir) === "node_modules" ? path.join(dir, name) : null]) {
      if (candidate && existsSync(path.join(candidate, "package.json"))) return candidate;
    }
    const up = path.dirname(dir);
    if (up === dir) return null;
    dir = up;
  }
}

/** Runtime dependency directories of a package (transitive), as `{ name, dir }`, found beside the package (lexical path
 * first, then its real path, so pnpm's symlinked layout works). */
export function collectDependencies(pkgRoot) {
  const found = new Map();
  const visit = (root) => {
    const deps = Object.keys(readJson(path.join(root, "package.json"))?.dependencies ?? {});
    for (const name of deps) {
      if (found.has(name)) continue;
      let real = root;
      try { real = realpathSync(root); } catch { /* keep lexical */ }
      const dir = findDependency(name, root) ?? findDependency(name, real);
      if (!dir) throw new Error(`disk: dependency ${name} of ${root} was not found beside the installed package`);
      found.set(name, dir);
      visit(dir);
    }
  };
  visit(pkgRoot);
  return [...found].map(([name, dir]) => ({ name, dir }));
}

/** Copy the installed @rightkit/release package (and its dependencies) into the stable root. The new tree is built in
 * the fixed `current.staging`, swapped in by rename (the old copy is parked at `current.previous` only until the
 * smoke test at `current` passes, then deleted; a failed swap or smoke run renames it back). No copy is retained.
 * Returns `{ stable, cli, version, replaced, unchanged }`. */
export function installStableRoot({ cli, platform = process.platform, env = process.env, home = os.homedir(), now = Date.now(), nodePath = process.execPath, smoke = true } = {}) {
  const stable = stableRootPath({ platform, env, home });
  const stableCli = stableCliPath(stable, platform);
  let realCli = cli;
  try { realCli = realpathSync(cli); } catch { /* keep lexical */ }
  if (isUnderStable(cli, stable, platform) || isUnderStable(realCli, stable, platform)) {
    return { stable, cli: stableCli, version: readJson(path.join(stable, "node_modules", "@rightkit", "release", "package.json"))?.version, replaced: false, unchanged: true };
  }
  const pkgRoot = path.resolve(path.dirname(cli), "..");
  const pkg = readJson(path.join(pkgRoot, "package.json"));
  if (pkg?.name !== "@rightkit/release") throw new Error(`disk: ${cli} is not inside an @rightkit/release package`);
  const deps = collectDependencies(pkgRoot);

  const parent = path.dirname(stable);
  mkdirSync(parent, { recursive: true });
  // Legacy per-pid names from earlier versions; the fixed names are `current.staging` and `current.previous`.
  for (const name of readdirSync(parent)) {
    if (/^current\.(staging|prev)-/.test(name)) try { rmSync(path.join(parent, name), { recursive: true, force: true }); } catch { /* in use: leave it */ }
  }
  const staging = stagingPathOf(stable);
  const previous = previousPathOf(stable);
  rmSync(staging, { recursive: true, force: true });
  const copy = (from, to, filter) => cpSync(from, to, { recursive: true, dereference: true, force: true, filter });
  try {
    const target = path.join(staging, "node_modules", "@rightkit", "release");
    mkdirSync(path.dirname(target), { recursive: true });
    copy(pkgRoot, target, (src) => path.basename(src) !== "node_modules" && !src.endsWith(".test.mjs"));
    for (const dep of deps) {
      const to = path.join(staging, "node_modules", ...dep.name.split("/"));
      mkdirSync(path.dirname(to), { recursive: true });
      copy(dep.dir, to, (src) => path.basename(src) !== "node_modules");
    }
    writeFileSync(path.join(staging, "install.json"), `${JSON.stringify({ package: pkg.name, version: pkg.version, source: pkgRoot, installedAt: new Date(now).toISOString(), node: nodePath }, null, 2)}\n`);
    const stagedCli = stableCliPath(staging, platform);
    if (!existsSync(stagedCli)) throw new Error("disk: the copied package has no cli/right-release.mjs");
  } catch (error) {
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
  // Swap first, then smoke-run at the fixed `current` path (never from the staging path). A failed run renames
  // the transiently parked `current.previous` back; a passed run deletes it (nothing is retained).
  const replaced = existsSync(stable);
  try {
    swapInPlace({
      staging, destination: stable, previous,
      verify: () => {
        if (!smoke) return;
        const r = spawnSync(nodePath, [stableCli, "--version"], { encoding: "utf8" });
        if (r.status !== 0) throw new Error(`the installed copy does not run: ${r.stderr || r.stdout || r.error?.message}`);
      },
    });
  } catch (error) {
    throw new Error(`disk: could not replace ${stable}: ${error.message}`);
  }
  return { stable, cli: stableCli, version: pkg.version, replaced, unchanged: false };
}

/** Registered schedule target: the CLI path the launchd job or Windows task runs, or null when none is registered. */
export function readScheduleBinding({ platform = process.platform, home = os.homedir(), file, run = spawnSync, label = "RightKit Disk Hygiene", plistFile } = {}) {
  if (platform === "darwin") {
    const plist = plistFile ?? file ?? path.join(home, "Library", "LaunchAgents", "dev.rightkit.disk-hygiene.plist");
    if (!existsSync(plist)) return null;
    // Parse the plist natively: launchd/plutil formatting and XML escaping must not change the binding.
    const result = run("/usr/bin/plutil", ["-extract", "ProgramArguments", "json", "-o", "-", "--", plist], { encoding: "utf8" });
    let args;
    try { if (result.status === 0) args = JSON.parse(String(result.stdout)); } catch { /* malformed binding fails evaluation below */ }
    const valid = Array.isArray(args) && typeof args[0] === "string" && typeof args[1] === "string";
    return valid ? { node: args[0], cli: args[1], source: plist } : { node: null, cli: null, source: plist };
  }
  const r = run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `$t = Get-ScheduledTask -TaskName '${label}' -ErrorAction SilentlyContinue; if (-not $t) { exit 3 }; $a = $t.Actions[0]; Write-Output $a.Execute; Write-Output $a.Arguments`], { encoding: "utf8" });
  if (r.status !== 0) return null;
  const [exe, ...rest] = String(r.stdout).split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  const args = rest.join(" ");
  const quoted = /"([^"]+)"/.exec(args);
  return { node: exe ?? null, cli: quoted ? quoted[1] : (args.split(/\s+/)[0] || null), source: label };
}

/** Evaluate a binding against the stable root. */
export function evaluateBinding(binding, { platform = process.platform, env = process.env, home = os.homedir(), exists = existsSync } = {}) {
  if (!binding) return { registered: false, ok: true, problems: [] };
  const stable = stableRootPath({ platform, env, home });
  const problems = [];
  if (!binding.cli) problems.push("the schedule names no right-release CLI");
  else {
    if (!isUnderStable(binding.cli, stable, platform)) problems.push(`target ${binding.cli} is not under the stable root ${stable} (reinstall: right-release disk schedule install)`);
    if (!exists(binding.cli)) problems.push(`target ${binding.cli} is MISSING`);
  }
  if (binding.node && !exists(binding.node)) problems.push(`node ${binding.node} is MISSING`);
  return { registered: true, ok: problems.length === 0, problems, cli: binding.cli, node: binding.node, stable };
}

/** `disk run` self-check: this process's own package must be intact and a registered schedule must be bound to the stable root. */
export function selfCheck({ moduleDir, platform = process.platform, env = process.env, home = os.homedir(), binding = () => readScheduleBinding({ platform, home }), exists = existsSync } = {}) {
  const problems = [];
  const pkg = readJson(path.join(moduleDir, "package.json"));
  if (!pkg) problems.push(`own package.json is unreadable at ${moduleDir}`);
  else {
    if (!exists(path.join(moduleDir, "cli", "right-release.mjs"))) problems.push("own cli/right-release.mjs is missing");
    for (const dep of Object.keys(pkg.dependencies ?? {})) if (!findDependency(dep, moduleDir)) problems.push(`dependency ${dep} is not resolvable from ${moduleDir}`);
  }
  let bound;
  try { bound = evaluateBinding(binding(), { platform, env, home, exists }); } catch (error) { bound = { registered: false, ok: false, problems: [`cannot read the schedule binding: ${error.message}`] }; }
  problems.push(...bound.problems);
  return { ok: problems.length === 0, problems, registered: bound.registered, version: pkg?.version };
}
