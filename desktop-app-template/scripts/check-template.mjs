// Manifest-consistency gate for the template (and for any app generated from it). No install,
// no network, no build: pure file checks. Exit 1 on any violation.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(join(root, p), "utf8");
const json = (p) => JSON.parse(read(p));
const problems = [];
const need = (ok, msg) => { if (!ok) problems.push(msg); };

const pkg = json("package.json");
const tauri = json("src-tauri/tauri.conf.json");
const shellToml = read("src-tauri/Cargo.toml");
const engineToml = read("crates/engine/Cargo.toml");
const placeholders = json("placeholders.json");
const tomlField = (s, k) => s.match(new RegExp(`^${k}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
const activeLines = (s) => s.split("\n").filter((l) => !l.trim().startsWith("#"));

// 1. App identity agrees across manifests.
const version = pkg.version;
need(tauri.version === version, `tauri.conf version ${tauri.version} != package.json ${version}`);
for (const [name, toml] of [["src-tauri", shellToml], ["engine", engineToml]]) {
  need(tomlField(toml, "version") === version, `${name}/Cargo.toml version != ${version}`);
}
need(tomlField(shellToml, "name") === pkg.name, "src-tauri package name != package.json name");
need(tauri.mainBinaryName === tomlField(shellToml.slice(shellToml.indexOf("[[bin]]")), "name"), "mainBinaryName != [[bin]] name");
need(tauri.productName.toLowerCase() === pkg.name, "productName lowercased != package name");
need(tauri.identifier.endsWith(`.${pkg.name}`), "identifier must end with .<package name>");

// 2. Sidecar wiring: externalBin <-> engine bin <-> build script <-> protocol dependency.
const engineBin = tomlField(engineToml.slice(engineToml.indexOf("[[bin]]")), "name");
need(tauri.bundle.externalBin?.join() === `binaries/${engineBin}`, `externalBin must be binaries/${engineBin}`);
need(read("scripts/build-engine-for-tauri.mjs").includes(`"${engineBin}"`), "build-engine script ENGINE != engine bin");
need(read("src-tauri/src/engine_supervisor.rs").includes(engineBin), "supervisor does not name the engine binary");

// 3. Dev server port agrees: tauri devUrl, vite, package dev script.
const port = new URL(tauri.build.devUrl).port;
need(read("vite.config.ts").includes(`port: ${port}`), "vite port != tauri devUrl port");
need(pkg.scripts.dev.includes(`--port ${port}`), "package dev script port != tauri devUrl port");
need(tauri.build.frontendDist === "../dist" && read("vite.config.ts").includes('outDir: "dist"'), "frontendDist != vite outDir");

// 4. Capabilities cover every registered plugin.
const lib = read("src-tauri/src/lib.rs");
const perms = JSON.stringify(json("src-tauri/capabilities/default.json").permissions);
for (const [plugin, perm] of [["tauri_plugin_updater", "updater:"], ["tauri_plugin_process", "process:"], ["tauri_plugin_os", "os:"]]) {
  need(!lib.includes(plugin) || perms.includes(perm), `capability missing ${perm}* for ${plugin}`);
}

// 5. RightKit pins: exact, registry-only, equal to the version manifest when it is reachable.
const manifestPath = join(root, "..", "packages", "release", "rightkit-versions.json");
const manifest = existsSync(manifestPath) ? JSON.parse(readFileSync(manifestPath, "utf8")) : null;
const deps = { ...pkg.dependencies, ...pkg.devDependencies };
for (const [name, spec] of Object.entries(deps).filter(([n]) => n.startsWith("@rightkit/"))) {
  need(/^\d+\.\d+\.\d+$/.test(spec), `${name} must be an exact registry version, got ${spec}`);
  need(!(name in placeholders.npm), `${name} is a placeholder; remove it from placeholders.json when adopting`);
  if (manifest) need(manifest.npm?.[name] || manifest.stagedNpm?.[name], `${name} unknown to rightkit-versions.json`);
}
for (const spec of Object.values(deps)) need(!/^(file|link|workspace|git|github|npm):|^https?:/.test(spec), `non-registry spec ${spec}`);
const cargoText = activeLines(shellToml).concat(activeLines(engineToml)).join("\n");
for (const m of cargoText.matchAll(/^(rightkit-[a-z-]+)\s*=\s*"([^"]+)"/gm)) {
  const [, name, spec] = m;
  need(/^=\d+\.\d+\.\d+$/.test(spec), `${name} must be pinned with =x.y.z, got ${spec}`);
  need(!(name in placeholders.cargo), `${name} is a placeholder; remove it from placeholders.json when adopting`);
  if (manifest) {
    const want = manifest.stagedCargo?.[name] ?? manifest.cargo?.[name];
    need(want === spec.slice(1), `${name} pin ${spec} != rightkit-versions.json ${want}`);
  }
}
need(!/rightkit[-_a-z]*\s*=\s*\{[^}]*(path|git)\s*=/.test(cargoText), "rightkit crates must not use path/git deps");

// 6. pnpm release-age exclusions list exactly the pinned freshly-published @rightkit versions.
const ws = read("pnpm-workspace.yaml");
for (const [name, spec] of Object.entries(deps).filter(([n]) => ["@rightkit/release", "@rightkit/logs", "@rightkit/tauri", "@rightkit/updates"].includes(n))) {
  need(ws.includes(`'${name}@${spec}'`), `pnpm-workspace.yaml minimumReleaseAgeExclude missing ${name}@${spec}`);
}

// 7. Every @rightkit import is a declared dependency.
const srcFiles = (dir) => readdirSync(join(root, dir), { withFileTypes: true }).flatMap((e) =>
  e.isDirectory() ? srcFiles(join(dir, e.name)) : /\.(tsx?|mjs)$/.test(e.name) ? [join(dir, e.name)] : []);
for (const f of [...srcFiles("src"), ...srcFiles("scripts")]) {
  for (const m of read(f).split("\n").filter((l) => !l.trim().startsWith("//")).join("\n").matchAll(/from\s+["'](@rightkit\/[a-z-]+)(?:\/[^"']*)?["']/g)) {
    need(deps[m[1]], `${f} imports undeclared ${m[1]}`);
  }
}

// 8. Release config agrees with the app (config is evaluated for real, not regex-matched).
let release;
let releaseSkipped = false;
try {
  release = (await import(join(root, "right-release.config.mjs"))).default;
} catch (e) {
  // The config imports @rightkit/release, which exists only after `pnpm install`.
  if (e.code === "ERR_MODULE_NOT_FOUND") releaseSkipped = true;
  else problems.push(`right-release.config.mjs failed to load: ${e.message}`);
}
if (release) {
  need(release.app === pkg.name, `release app ${release.app} != ${pkg.name}`);
  need(release.version === version, "release version != package version");
  need(["github-releases", "r2-rightapps"].includes(release.distribution?.provider), "distribution.provider must be github-releases or r2-rightapps");
}

if (problems.length) {
  console.error(problems.map((p) => `FAIL ${p}`).join("\n"));
  process.exit(1);
}
console.log(`template manifests consistent (${Object.keys(deps).filter((n) => n.startsWith("@rightkit/")).length} npm pins, ${manifest ? "checked against rightkit-versions.json" : "version manifest not reachable"}${releaseSkipped ? "; release config skipped: dependencies not installed" : ""})`);
