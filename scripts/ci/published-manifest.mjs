// G29: a published crate's manifest is frozen at its version. crates.io keeps the manifest of each version forever, so changing a crate's
// RightKit dependency requirements without raising its version leaves main and the registry disagreeing at one version number: consumers
// get what the registry has (2026-11-25: 54 crates on main required rightkit-fs 0.5 while their published versions required ^0.4, so apps
// locked both). Dependency-free: reads only what it needs from Cargo.toml text (the preflight runs with no node_modules).

const SECTION = /^\s*\[([^\]]+)\]\s*(?:#.*)?$/;
const unquote = (s) => s.trim().replace(/^"(.*)"$/, "$1").replace(/^'(.*)'$/, "$1");

/** The inline-table fields of `{ a = "x", b = true }` (one line), or null. */
function inlineTable(text) {
  const m = /^\{(.*)\}\s*(?:#.*)?$/.exec(text.trim());
  if (!m) return null;
  const out = {};
  for (const part of m[1].split(",")) {
    const kv = /^\s*([A-Za-z0-9_-]+)\s*=\s*(.+?)\s*$/.exec(part);
    if (kv) out[kv[1]] = kv[2] === "true" ? true : kv[2] === "false" ? false : unquote(kv[2]);
  }
  return out;
}

/**
 * The RightKit (`rightkit-*`) dependencies a Cargo.toml declares, non-dev: [dependencies], [build-dependencies], their [target.*.]
 * forms and [workspace.dependencies] (keyed `workspace` there). Each value is { version, package, workspace } as far as the text says.
 */
export function rightkitDependencies(text) {
  const out = { normal: {}, workspace: {} };
  let section = null; let tableDep = null;
  for (const raw of String(text).split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "");
    const head = SECTION.exec(line);
    if (head) {
      const name = head[1].trim();
      tableDep = null;
      const dotted = /^(?:target\.[^.]+(?:\.[^.]+)*?\.)?(dependencies|build-dependencies)\.("?)(rightkit-[A-Za-z0-9_-]+)\2$/.exec(name);
      if (dotted) { section = "normal"; tableDep = dotted[3]; out.normal[tableDep] = { ...(out.normal[tableDep] ?? {}) }; continue; }
      if (name === "workspace.dependencies") section = "workspace";
      else if (/^(?:target\..+\.)?(?:dependencies|build-dependencies)$/.test(name)) section = "normal";
      else section = null;
      continue;
    }
    if (!section) continue;
    const kv = /^\s*("?)([A-Za-z0-9_.-]+)\1\s*=\s*(.+?)\s*$/.exec(line);
    if (!kv) continue;
    const [, , key, value] = kv;
    if (tableDep) {
      if (key === "version") out.normal[tableDep].version = unquote(value);
      else if (key === "package") out.normal[tableDep].package = unquote(value);
      else if (key === "workspace") out.normal[tableDep].workspace = value === "true";
      continue;
    }
    const dotted = /^(rightkit-[A-Za-z0-9_-]+)\.(version|workspace|package)$/.exec(key);
    if (dotted) {
      const entry = (out[section][dotted[1]] ??= {});
      entry[dotted[2]] = dotted[2] === "workspace" ? value === "true" : unquote(value);
      continue;
    }
    const table = inlineTable(value);
    const name = table?.package && String(table.package).startsWith("rightkit-") ? table.package : key;
    if (!name.startsWith("rightkit-")) continue;
    out[section][name] = table ? { version: table.version, workspace: table.workspace === true } : { version: unquote(value) };
  }
  return out;
}

/** Each RightKit dependency's effective requirement for a crate, `workspace = true` resolved through the root manifest. */
export function effectiveRequirements(crateText, rootText) {
  const crate = rightkitDependencies(crateText);
  const root = rightkitDependencies(rootText ?? "").workspace;
  const out = {};
  for (const [name, entry] of Object.entries(crate.normal)) out[name] = entry.workspace ? (root[name]?.version ?? null) : (entry.version ?? null);
  return out;
}

/** The `[package]` name and version a Cargo.toml states (`version.workspace = true` resolved through the root's [workspace.package]). */
export function packageIdentity(crateText, rootText) {
  let section = null; let name = null; let version = null; let inherits = false; let rootVersion = null;
  for (const raw of String(rootText ?? "").split(/\r?\n/)) {
    const head = SECTION.exec(raw); if (head) { section = head[1].trim(); continue; }
    const m = section === "workspace.package" && /^\s*version\s*=\s*(.+?)\s*$/.exec(raw); if (m) rootVersion = unquote(m[1]);
  }
  section = null;
  for (const raw of String(crateText).split(/\r?\n/)) {
    const head = SECTION.exec(raw); if (head) { section = head[1].trim(); continue; }
    if (section !== "package") continue;
    const n = /^\s*name\s*=\s*(.+?)\s*$/.exec(raw); if (n) name = unquote(n[1]);
    const v = /^\s*version\s*=\s*(.+?)\s*$/.exec(raw); if (v) { const t = inlineTable(v[1]); if (t?.workspace) inherits = true; else version = unquote(v[1]); }
    if (/^\s*version\.workspace\s*=\s*true/.test(raw)) inherits = true;
  }
  return { name, version: inherits ? rootVersion : version };
}

/**
 * The crates whose RightKit requirements changed between `before` and `after` while their version stayed one the feed records as published.
 * `before`/`after`: { root, crates: Map(dir -> Cargo.toml text) }; `published`: { [crate]: version } (rightkit-versions.json registry.cargo).
 */
export function frozenManifestProblems({ before, after, published }) {
  const out = [];
  for (const [dir, text] of after.crates) {
    const old = before.crates.get(dir);
    if (old === undefined) continue;
    const now = packageIdentity(text, after.root); const was = packageIdentity(old, before.root);
    if (!now.name || !now.version || now.version !== was.version || published[now.name] !== now.version) continue;
    const a = effectiveRequirements(old, before.root); const b = effectiveRequirements(text, after.root);
    const changed = [...new Set([...Object.keys(a), ...Object.keys(b)])].filter((d) => (a[d] ?? null) !== (b[d] ?? null)).sort();
    if (changed.length) out.push({ crate: now.name, version: now.version, dir, changed: changed.map((d) => `${d} ${a[d] ?? "-"} -> ${b[d] ?? "-"}`) });
  }
  return out;
}
