// Assemble the RightKit shape matrix: one row per package, one column per app.
// node build-matrix.mjs <rightkit root> <scratch dir> <out.json> <out.md>
import { readFileSync, readdirSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
const [RK, S, OUT_JSON, OUT_MD] = process.argv.slice(2);
const J = (f) => JSON.parse(readFileSync(f, "utf8"));
const pm = J(join(RK, "docs/port-matrix.json"));
const cargoReg = Object.fromEntries(J(join(S, "registry.json")).map((r) => [r.name, r]));
const npmReg = Object.fromEntries(J(join(S, "npm-registry.json")).map((r) => [r.name, r]));
const cov = J(join(S, "coverage-rows.json"));
const APPS = ["heardright", "viewright", "pulse", "coderight", "genright", "mailright", "scraperight", "cutright", "postright", "legion"];

// plan coverage per target per app
const plan = new Map();
for (const r of cov) { const t = plan.get(r.target) ?? {}; const a = t[r.app] ?? { loc: 0, n: 0, tgt: 0, any: 0 }; a.loc += r.loc ?? 0; a.n += r.n; a.tgt += r.tgt; a.any += r.any; t[r.app] = a; plan.set(r.target, t); }

// review verdict rows, later files win
const verdict = new Map();
for (const f of readdirSync(S).filter((f) => /^matrix-rows-.*\.json$/.test(f)).sort()) {
  let rows; try { rows = J(join(S, f)); } catch (e) { console.error(`skip ${f}: ${e.message}`); continue; }
  for (const r of rows) if (r && r.package) { const prev = verdict.get(r.package); verdict.set(r.package, { ...r, apps: { ...(prev?.apps ?? {}), ...(r.apps ?? {}) }, actions: [...new Set([...(r.actions ?? []), ...(prev?.actions ?? [])])], boundary: [...new Set([...(r.boundary ?? []), ...(prev?.boundary ?? [])])], source: prev ? prev.source + ", " + f : f }); }
}

const rows = [];
const seen = new Set();
for (const p of pm.packages) {
  seen.add(p.name);
  const reg = p.kind === "cargo" ? cargoReg[p.name] : p.kind === "npm" ? npmReg[p.name] : null;
  rows.push({ package: p.name, kind: p.kind, dir: p.dir, class: p.class, domain: p.domain, rowsOwned: p.mandatoryRows ?? [], version: reg?.version ?? p.source, registryLatest: reg ? reg.latest : (p.registry || null), published: reg ? (reg.publish === false || reg.private ? "private" : reg.onRegistry ? "yes" : "no") : "n/a", pins: Object.fromEntries(APPS.map((a) => [a, p.cells?.[a]?.state ?? "-"])), plan: plan.get(p.name) ?? {}, review: verdict.get(p.name) ?? null });
}
// planned packages with no RightKit unit under that exact name, and reviewed packages not in the port matrix
for (const name of new Set([...plan.keys(), ...verdict.keys()])) {
  if (seen.has(name) || ["app-shell", "UNMAPPED"].includes(name)) continue;
  const v = verdict.get(name) ?? null;
  rows.push({ package: name, kind: name.startsWith("@") ? "npm" : name.startsWith("RightKit") ? "swift" : "cargo", dir: v?.existsAs?.join(", ") ?? null, class: "planned", domain: "planned", rowsOwned: [], version: null, registryLatest: null, published: "n/a", pins: Object.fromEntries(APPS.map((a) => [a, "-"])), plan: plan.get(name) ?? {}, review: v });
}
// a sub-crate with no verdict of its own takes its family's (rightkit-agent-engine-core <- rightkit-agent-engine)
for (const r of rows) {
  if (r.review) continue;
  const fam = [...verdict.keys()].filter((k) => r.package.startsWith(k + "-")).sort((a, b) => b.length - a.length)[0];
  if (fam) { const v = verdict.get(fam); r.review = { ...v, package: r.package, inheritedFrom: fam, basis: `family verdict of ${fam}`, actions: [], boundary: [] }; }
}
rows.sort((a, b) => a.package.localeCompare(b.package));
writeFileSync(OUT_JSON, JSON.stringify({ generated: new Date().toISOString(), rightkitHead: pm.rightkitHead, apps: APPS, rows }, null, 1));

// Markdown
const pct = (a) => (a && a.n ? Math.round((100 * a.tgt) / a.n) + "%" : "");
const cell = (r, a) => { const fit = r.review?.apps?.[a]?.fit; const pin = r.pins[a]; const pl = r.plan[a]; const parts = []; if (fit && fit !== "n/a") parts.push(fit); if (pin && pin !== "-" && pin !== "unused" && pin !== "not present") parts.push(pin); if (pl?.loc) parts.push(`${pl.loc}L ${pct(pl)}`); return parts.join(" · ") || ""; };
const reviewed = rows.filter((r) => r.review);
const count = (k) => reviewed.filter((r) => r.review.shape === k).length;
let md = `# RightKit shape matrix\n\nGenerated ${new Date().toISOString().slice(0, 16)}Z from RightKit ${pm.rightkitHead}.\n\n`;
md += `Packages: ${rows.length} (${rows.filter((r) => r.kind === "cargo").length} crates, ${rows.filter((r) => r.kind === "npm").length} npm, ${rows.filter((r) => r.kind === "swift").length} Swift). Reviewed for shape: ${reviewed.length} (best ${count("best")}, gaps ${count("gaps")}, wrong ${count("wrong")}, missing ${count("missing")}). Not yet reviewed: ${rows.length - reviewed.length}.\n\n`;
md += `Cell: review fit · pin state on the app's main · planned lines and the share found in the package. Empty means the app has nothing for the package.\n\n`;
const byDomain = new Map(); for (const r of rows) { const k = r.review?.family ?? r.domain ?? "other"; (byDomain.get(k) ?? byDomain.set(k, []).get(k)).push(r); }
for (const [d, list] of [...byDomain].sort()) {
  md += `## ${d} (${list.length})\n\n| Package | Kind | Published | Shape | Basis | ${APPS.join(" | ")} |\n|---|---|---|---|---|${APPS.map(() => "---").join("|")}|\n`;
  for (const r of list) md += `| \`${r.package}\` | ${r.kind} | ${r.published}${r.published === "no" ? ` (${r.version}, registry ${r.registryLatest ?? "none"})` : ""} | ${r.review ? r.review.shape + (r.review.inheritedFrom ? " (family)" : "") : "not reviewed"} | ${(r.review?.basis ?? "").replace(/\|/g, "/")} | ${APPS.map((a) => cell(r, a)).join(" | ")} |\n`;
  md += "\n";
}
const own = (k) => rows.filter((r) => r.review && !r.review.inheritedFrom && r.review.shape === k);
md += `## Wrong shape (${own("wrong").length} packages; sub-crates that inherit a family verdict are not repeated)\n\n| Package | Why | Reference app |\n|---|---|---|\n`;
for (const r of own("wrong")) md += `| \`${r.package}\` | ${(r.review.shapeReason ?? "").replace(/\|/g, "/").replace(/\n/g, " ")} | ${r.review.referenceApp ?? ""} |\n`;
md += `\n## Missing (${own("missing").length})\n\n| Package | Note |\n|---|---|\n`;
for (const r of own("missing")) md += `| \`${r.package}\` | ${(r.review.shapeReason ?? "").replace(/\|/g, "/").replace(/\n/g, " ")} |\n`;
md += `\n## Actions before publication\n\n`;
for (const r of reviewed.filter((r) => (r.review.actions ?? []).length)) md += `- \`${r.package}\` (${r.review.shape}): ${r.review.actions.join("; ")}\n`;
writeFileSync(OUT_MD, md);
console.log(`rows ${rows.length}, reviewed ${reviewed.length}, best ${count("best")}, gaps ${count("gaps")}, wrong ${count("wrong")}, missing ${count("missing")}`);
