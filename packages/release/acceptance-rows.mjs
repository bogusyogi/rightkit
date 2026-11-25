// The acceptance test's own row enumerator (rightkit#1514 step 3; chat gate, 2026-11-21): which rows exist for a git tree, and the files each reads, worked
// out from the tree alone (file list with blob ids, and the manifests in it), never from the gate's planner or anything the gate writes. It covers only the
// row kinds whose rules a tree states: the Rust formatting row per edition and the per-crate compile rows of every Cargo workspace in the tree. Every
// other kind the gate plans (docs/audits/... the list in D:\scratch\loop-review\twelve-line-step3\row-rules-only-in-gate-code.md) is decided by a rule that
// exists only inside gate code, so it is returned in `uncovered` and the lines say the rows it decides are not measured.
//
//   enumerateRows({ files: Map<path, blob>, read: (path) => text }) -> { rows: Map<id, { stage, files }>, uncovered: [kind] }

const UNCOVERED = Object.freeze([
  "rustfmt buckets (rustfmt:<edition>:<n>) and the vendor, generated and fixture exemptions", "node-check", "json-parse / toml-parse / yaml-parse", "ownership-catalog", "actionlint", "shellcheck",
  "cargo-metadata", "cargo-deny / cargo-audit", "ghsa-audit / advisory-exceptions", "cargo-package-list", "changelog-version / license / secret-scan", "pnpm-lockfile / pnpm-audit",
  "tsc / eslint / biome / oxlint", "generated-docs / suite-api / swift-typecheck / path-policy", "feature-coverage", "swift-lock / gradle-lock / python-lock",
  "feature-set variants (:<i>), cross targets (@<triple>) and their platform scoping", "doc, check-features, check-dependent", "test:rust, test:rust-serial, test:npm, swift-build",
  "path dependencies, include!/#[path]/build.rs reach and vendored crates in a crate's compile key",
]);

const strip = (t) => String(t ?? "").replace(/#.*$/gm, "");
const table = (toml, name) => { const m = new RegExp(`^\\[${name.replace(/\./g, "\\.")}\\]\\s*$([\\s\\S]*?)(?=^\\[|(?![\\s\\S]))`, "m").exec(strip(toml)); return m ? m[1] : null; };
const str = (body, key) => new RegExp(`^${key}\\s*=\\s*"([^"]+)"`, "m").exec(body ?? "")?.[1] ?? null;
const members = (toml) => { const ws = table(toml, "workspace"); if (ws === null) return null; const m = /members\s*=\s*\[([\s\S]*?)\]/.exec(ws); return m ? [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1].replace(/\/+$/, "")) : []; };

export function enumerateRows({ files, read }) {
  const rows = new Map(); const dirOf = (p) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "");
  const under = (dir) => [...files.keys()].filter((p) => (dir === "" ? true : p.startsWith(`${dir}/`)));
  const rootTomls = [...files.keys()].filter((p) => /(^|\/)Cargo\.toml$/.test(p) && !/(^|\/)(target|node_modules|vendor)\//.test(p));
  const editions = new Map();
  for (const manifest of rootTomls) {
    let toml; try { toml = read(manifest); } catch { continue; }
    const ws = members(toml); if (ws === null) continue;
    const wsDir = dirOf(manifest); const wsEdition = str(table(toml, "workspace.package"), "edition");
    const lock = files.has(wsDir ? `${wsDir}/Cargo.lock` : "Cargo.lock") ? [wsDir ? `${wsDir}/Cargo.lock` : "Cargo.lock"] : [];
    for (const m of ws) {
      const dirs = m.endsWith("/*") ? [...new Set(under(wsDir ? `${wsDir}/${m.slice(0, -2)}` : m.slice(0, -2)).map((p) => p.split("/").slice(0, (wsDir ? wsDir.split("/").length : 0) + m.split("/").length).join("/")))].filter((d) => files.has(`${d}/Cargo.toml`)) : [wsDir ? `${wsDir}/${m}` : m];
      for (const d of dirs) {
        let crate; try { crate = read(`${d}/Cargo.toml`); } catch { continue; }
        const pkg = table(crate, "package"); const name = str(pkg, "name"); if (name === null) continue;
        const edition = /^edition\.workspace\s*=\s*true/m.test(pkg) ? wsEdition : str(pkg, "edition");
        const crateFiles = under(d).filter((p) => !/(^|\/)target\//.test(p));
        if (edition) { const e = editions.get(edition) ?? new Set(); crateFiles.filter((p) => p.endsWith(".rs")).forEach((p) => e.add(p)); editions.set(edition, e); }
        rows.set(`clippy:${wsDir || "."}:${name}`, { stage: "B", files: [...new Set([...crateFiles, manifest, ...lock])].sort() });
        rows.set(`check:${wsDir || "."}:${name}`, { stage: "B", files: [...new Set([...crateFiles, manifest, ...lock])].sort() });
      }
    }
  }
  for (const [ed, set] of editions) rows.set(`rustfmt:${ed}`, { stage: "A", files: [...set].sort() });
  return { rows, uncovered: UNCOVERED };
}
