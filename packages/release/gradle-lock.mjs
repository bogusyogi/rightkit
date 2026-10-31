// File-only Gradle dependency-locking check (class A scan `gradle-lock`). Gradle resolves dependencies at build time, so a build with no
// committed lock can drift between hosts; with a lock, a stale one fails here instead of inside the compile. This module reads committed
// files and executes nothing (no Gradle, no wrapper). It checks DIRECT dependencies only: each project directory that declares dependencies
// must have its own gradle.lockfile (or legacy gradle/dependency-locks/*.lockfile), and every literal direct dependency (build.gradle[.kts]
// strings and the libs.versions.toml entries the build references) must have a locked version at or above the declared one. Dynamic or
// interpolated versions are checked for presence only; a declaration that cannot be read literally is a finding, never a silent pass.
const NOT_SET_UP = "Gradle dependency locking is not set up: run ./gradlew dependencies --write-locks and commit gradle.lockfile";
const join = (dir, f) => (dir === "" || dir === "." ? f : `${dir}/${f}`);
const SKIP_DIRS = new Set([".git", ".gradle", "build", "node_modules", ".idea", "out", "target", ".build"]);
const BUILD_FILE = /^build\.gradle(\.kts)?$/;
export const GRADLE_BUILD_FILE = /(^|\/)(build|settings)\.gradle(\.kts)?$/;

// ---- versions -------------------------------------------------------------------------------------------------------------
const tokensOf = (v) => String(v).split(/[.\-_+]/).filter((t) => t !== "");
const RELEASE_WORDS = new Set(["final", "ga", "release", "sp"]);
/** Gradle-like version order (approximate): numeric segments by value, qualifiers below the plain release. */
export function compareGradleVersions(a, b) {
  const x = tokensOf(a); const y = tokensOf(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const p = x[i]; const q = y[i];
    if (p === undefined) return /^\d+$/.test(q) ? (Number(q) === 0 ? 0 : -1) : RELEASE_WORDS.has(q.toLowerCase()) ? 0 : 1;
    if (q === undefined) return /^\d+$/.test(p) ? (Number(p) === 0 ? 0 : 1) : RELEASE_WORDS.has(p.toLowerCase()) ? 0 : -1;
    const pn = /^\d+$/.test(p); const qn = /^\d+$/.test(q);
    if (pn && qn) { if (Number(p) !== Number(q)) return Number(p) < Number(q) ? -1 : 1; continue; }
    if (pn !== qn) return pn ? 1 : -1;
    if (p.toLowerCase() !== q.toLowerCase()) return p.toLowerCase() < q.toLowerCase() ? -1 : 1;
  }
  return 0;
}
const isDynamic = (v) => v === null || v === undefined || /[+[\](),]|^latest\.|\$/.test(v);

// ---- lockfiles ------------------------------------------------------------------------------------------------------------
/** Map "group:artifact" -> Set of locked versions, from gradle.lockfile text (`g:a:v=config,config`; comments and `empty=` ignored). */
export function parseLockfile(text, into = new Map()) {
  for (const raw of String(text).split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#") || line.startsWith("empty=")) continue;
    const m = /^([^:=\s]+):([^:=\s]+):([^=\s]+)=/.exec(line);
    if (!m) continue;
    const key = `${m[1]}:${m[2]}`;
    if (!into.has(key)) into.set(key, new Set());
    into.get(key).add(m[3]);
  }
  return into;
}

// ---- libs.versions.toml (the literal shapes Gradle documents; no TOML library needed) -------------------------------------
function stripTomlComment(line) {
  let inStr = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '"' && line[i - 1] !== "\\") inStr = !inStr;
    else if (c === "#" && !inStr) return line.slice(0, i);
  }
  return line;
}
const tomlStr = (s) => { const m = /^"((?:[^"\\]|\\.)*)"$/.exec(String(s).trim()); return m ? m[1] : null; };
function inlineFields(body) {
  // `key = "v"`, `key.ref = "v"`, `key = { ref = "v" }` inside one inline table (one level of nesting).
  const out = {}; const re = /([A-Za-z_][\w.-]*)\s*=\s*("(?:[^"\\]|\\.)*"|\{[^}]*\})/g; let m;
  while ((m = re.exec(body))) {
    if (m[2].startsWith("{")) { for (const [k, v] of Object.entries(inlineFields(m[2].slice(1, -1)))) out[`${m[1]}.${k}`] = v; } else out[m[1]] = tomlStr(m[2]);
  }
  return out;
}
/** { versions, libraries: Map(alias -> { module, version, line }), bundles: Map(name -> [alias]), problems: [{ line, text }] } */
export function parseCatalog(text) {
  const versions = {}; const libraries = new Map(); const bundles = new Map(); const problems = []; const refs = [];
  let section = null;
  String(text).split(/\r?\n/).forEach((raw, i) => {
    const line = stripTomlComment(raw).trim();
    if (!line) return;
    const sec = /^\[([\w.-]+)\]$/.exec(line);
    if (sec) { section = sec[1]; return; }
    const kv = /^([A-Za-z_][\w.-]*)\s*=\s*(.*)$/.exec(line);
    if (!kv) return;
    const [, key, value] = kv; const n = i + 1;
    if (section === "versions") {
      const s = tomlStr(value);
      if (s !== null) versions[key] = s;
      else if (value.startsWith("{")) { const f = inlineFields(value.slice(1, value.lastIndexOf("}"))); versions[key] = f.strictly ?? f.require ?? f.prefer ?? null; if (versions[key] === null) problems.push({ line: n, text: `version ${key} has no strictly/require/prefer literal` }); }
      else problems.push({ line: n, text: `version ${key} is not a literal the checker reads` });
    } else if (section === "libraries") {
      const s = tomlStr(value);
      if (s !== null) {
        const p = s.split(":");
        if (p.length >= 2 && p.length <= 3) libraries.set(key, { module: `${p[0]}:${p[1]}`, version: p[2] ?? null, line: n });
        else problems.push({ line: n, text: `library ${key} is not group:artifact[:version]` });
      } else if (value.startsWith("{")) {
        const f = inlineFields(value.slice(1, value.lastIndexOf("}")));
        const module = f.module ?? (f.group && f.name ? `${f.group}:${f.name}` : null);
        if (!module) { problems.push({ line: n, text: `library ${key} has no module or group/name` }); return; }
        const ref = f["version.ref"] ?? f["version.ref"] ?? null;
        const direct = f.version ?? f["version.strictly"] ?? f["version.require"] ?? f["version.prefer"] ?? null;
        const lib = { module, version: direct, line: n };
        if (ref !== null) refs.push([lib, ref]);
        libraries.set(key, lib);
      } else problems.push({ line: n, text: `library ${key} is not a literal the checker reads` });
    } else if (section === "bundles") {
      const members = [...value.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
      bundles.set(key, members);
    }
  });
  for (const [lib, ref] of refs) {
    if (Object.hasOwn(versions, ref)) lib.version = versions[ref];
    else problems.push({ line: lib.line, text: `library ${lib.module} references version ${ref}, which [versions] does not define` });
  }
  return { versions, libraries, bundles, problems };
}
const aliasKey = (a) => a.replace(/[-_.]/g, ".").toLowerCase();

// ---- build files ----------------------------------------------------------------------------------------------------------
const CONFIG = "(?:api|implementation|compileOnly|runtimeOnly|compile|runtime|annotationProcessor|kapt|ksp|coreLibraryDesugaring|lintChecks|lintPublish|[a-z][A-Za-z]*(?:Implementation|Api|CompileOnly|RuntimeOnly|AnnotationProcessor|Kapt|Ksp|Compile|Runtime))";
const DEP_LINE = new RegExp(`^\\s*(${CONFIG})\\b\\s*\\(?\\s*(?:platform\\(\\s*|enforcedPlatform\\(\\s*)?(.*)$`);
function stripGradleComments(text) {
  return String(text).replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " ")).split("\n").map((l) => l.replace(/(^|[^:"'\w])\/\/.*$/, "$1")).join("\n");
}
/** [{ line, config, kind: "gav"|"alias"|"bundle"|"unreadable", group, name, version, alias, text }] from a build.gradle[.kts]'s dependency lines. */
export function parseBuildDependencies(source) {
  const out = [];
  stripGradleComments(source).split("\n").forEach((l, i) => {
    const m = DEP_LINE.exec(l);
    if (!m) return;
    const line = i + 1; const config = m[1]; let rest = m[2].trim();
    if (/^(project|files|fileTree|gradleApi|localGroovy|gradleKotlinDsl)\b/.test(rest) || rest.startsWith("{") || rest === "") return;
    let r;
    if ((r = /^["']([^"':\s]+):([^"':\s]+)(?::([^"':\s]*))?["']/.exec(rest))) { out.push({ line, config, kind: "gav", group: r[1], name: r[2], version: r[3] ?? null }); return; }
    if ((r = /^group\s*[:=]\s*["']([^"']+)["']\s*,\s*name\s*[:=]\s*["']([^"']+)["'](?:\s*,\s*version\s*[:=]\s*["']([^"']+)["'])?/.exec(rest))) { out.push({ line, config, kind: "gav", group: r[1], name: r[2], version: r[3] ?? null }); return; }
    if ((r = /^libs\.bundles\.([\w.]+?)(?:\.get\(\))?\s*\)*\s*$/.exec(rest))) { out.push({ line, config, kind: "bundle", alias: r[1] }); return; }
    if ((r = /^libs\.((?!plugins\.|versions\.)[\w.]+?)(?:\.get\(\))?\s*\)*\s*$/.exec(rest))) { out.push({ line, config, kind: "alias", alias: r[1] }); return; }
    if ((r = /^kotlin\(\s*["']([\w.-]+)["']/.exec(rest))) { out.push({ line, config, kind: "gav", group: "org.jetbrains.kotlin", name: `kotlin-${r[1]}`, version: null }); return; }
    out.push({ line, config, kind: "unreadable", text: rest.slice(0, 70) });
  });
  return out;
}

/** null when `locked` (Set) holds the dependency at or above `version`; else why not. Dynamic versions need only presence. */
function lockedProblem(locked, version) {
  if (!locked || locked.size === 0) return "has no locked version";
  if (isDynamic(version)) return null;
  return [...locked].some((v) => compareGradleVersions(v, version) >= 0) ? null : `is locked at ${[...locked].join(", ")}, below the declared ${version}`;
}

/**
 * Findings for the Gradle build rooted at `dir` ("." for the repository root). `read(file)` / `exists(file)` take repo-relative paths;
 * `list(dir) -> [{ name, isDir }]` lists a directory.
 */
export function gradleLockProblems({ dir, read, exists, list }) {
  const out = [];
  const catalogFile = join(dir, "gradle/libs.versions.toml");
  let catalog = null;
  if (exists(catalogFile)) { catalog = parseCatalog(read(catalogFile)); out.push(...catalog.problems.map((p) => `${catalogFile}:${p.line}: ${p.text}`)); }
  const builds = [];
  const walk = (d, depth) => {
    let entries = [];
    try { entries = list(d); } catch { return; }
    for (const e of entries) {
      if (e.isDir) { if (depth < 6 && !SKIP_DIRS.has(e.name) && !e.name.startsWith(".")) walk(join(d, e.name), depth + 1); } else if (BUILD_FILE.test(e.name)) builds.push(join(d, e.name));
    }
  };
  walk(dir, 0);
  for (const file of builds.sort()) {
    const deps = parseBuildDependencies(read(file));
    if (!deps.length) continue;
    const projectDir = file.includes("/") ? file.slice(0, file.lastIndexOf("/")) : ".";
    const lockFile = join(projectDir, "gradle.lockfile");
    const locks = new Map(); let any = false;
    if (exists(lockFile)) { parseLockfile(read(lockFile), locks); any = true; }
    const legacy = join(projectDir, "gradle/dependency-locks");
    try { for (const e of list(legacy)) if (!e.isDir && e.name.endsWith(".lockfile")) { parseLockfile(read(`${legacy}/${e.name}`), locks); any = true; } } catch { /* no legacy lock directory */ }
    if (!any) { out.push(`${file}: ${deps.length} declared ${deps.length === 1 ? "dependency" : "dependencies"} and no ${lockFile}: ${NOT_SET_UP}`); continue; }
    const expand = [];
    for (const d of deps) {
      const at = `${file}:${d.line}`;
      if (d.kind === "unreadable") { out.push(`${at}: ${d.config} dependency cannot be read literally (${d.text})`); continue; }
      if (d.kind === "gav") {
        if (/\$|\{/.test(d.group + d.name)) { out.push(`${at}: ${d.config} dependency group or name is interpolated, which cannot be read literally`); continue; }
        expand.push({ at, module: `${d.group}:${d.name}`, version: d.version });
        continue;
      }
      if (!catalog) { out.push(`${at}: ${d.config} uses libs.${d.alias} but ${catalogFile} does not exist`); continue; }
      if (d.kind === "bundle") {
        const key = [...catalog.bundles.keys()].find((k) => aliasKey(k) === aliasKey(d.alias));
        if (!key) { out.push(`${at}: libs.bundles.${d.alias} is not in ${catalogFile} [bundles]`); continue; }
        for (const a of catalog.bundles.get(key)) { const lib = catalog.libraries.get(a); if (lib) expand.push({ at, module: lib.module, version: lib.version, via: `libs.bundles.${d.alias}` }); else out.push(`${at}: bundle ${key} names ${a}, which [libraries] does not define`); }
        continue;
      }
      const key = [...catalog.libraries.keys()].find((k) => aliasKey(k) === aliasKey(d.alias));
      if (!key) { out.push(`${at}: libs.${d.alias} is not in ${catalogFile} [libraries]`); continue; }
      const lib = catalog.libraries.get(key);
      expand.push({ at, module: lib.module, version: lib.version, via: `libs.${d.alias}` });
    }
    for (const e of expand) {
      const why = lockedProblem(locks.get(e.module), e.version);
      if (why) out.push(`${e.at}: ${e.module}${e.version ? `:${e.version}` : ""}${e.via ? ` (${e.via})` : ""} ${why} in ${lockFile}`);
    }
  }
  return out;
}

/** Repo-relative Gradle build root for a changed file: the nearest ancestor with settings.gradle[.kts], else the nearest with build.gradle[.kts]. */
export function gradleOwnerOf(file, { exists, ancestorDirs }) {
  const dirs = ancestorDirs(file.includes("/") ? file.slice(0, file.lastIndexOf("/")) : ".").reverse();
  const has = (d, names) => names.some((n) => exists(join(d, n)));
  return dirs.find((d) => has(d, ["settings.gradle", "settings.gradle.kts"])) ?? dirs.find((d) => has(d, ["build.gradle", "build.gradle.kts"])) ?? null;
}
