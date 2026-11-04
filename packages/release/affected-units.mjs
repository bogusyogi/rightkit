// Shared planning for RightKit unit runs (rightkit#1505; reconciled scope 2026-11-04): fixed workspace inventory,
// automatic changed-unit plus dependent selection, dependency-first ordering and result classification. Pure: the
// caller supplies the inventory and changed paths; nothing here reads a tree, runs git or starts a process, so the
// selection a run used can be replayed from its report alone.

/** The aggregate report a multi-unit door run writes beside the per-unit receipts. */
export const UNIT_LIST_REPORT_SCHEMA = "rightkit.unit-list-report/1";

/**
 * Repository paths that shape every unit of a kind (or all units), so a change there selects the whole kind with the
 * reason named. Paths not listed and outside a unit directory select nothing and are reported as unmatched, never
 * silently widened: docs and unrelated files must not turn a routine compile into a full sweep.
 */
export const SHARED_PATH_RULES = Object.freeze([
  { paths: ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "deny.toml", ".cargo/audit.toml"], kind: "cargo", reason: "shared-rust-workspace" },
  { paths: ["pnpm-lock.yaml", "pnpm-workspace.yaml", "package.json"], kind: "npm", reason: "shared-npm-workspace" },
  { paths: ["ci/gate.config.json", "ci/asset-gated.json", "ci/known-red.json", ".rightkit/fixture-files.json"], kind: null, reason: "shared-gate-config" },
]);

const selectorOf = (unit) => `${unit.kind}:${unit.name}`;

/**
 * Build the runnable inventory from caller-read manifests.
 * `cargo`: [{ dir: "crates/<x>", name, deps: [<crate name>...] }] with deps already resolved to sibling crate names
 * (crate-workspaces.mjs pathDependencies). `npm`: [{ dir: "packages/<x>", name: "@rightkit/<x>", deps: ["@rightkit/<y>"...] }].
 * Entries whose name cannot form a unit selector are skipped and named in `skipped`.
 */
export function buildInventory({ cargo = [], npm = [] } = {}) {
  const units = [];
  const skipped = [];
  for (const c of cargo) {
    if (typeof c?.name !== "string" || !/^rightkit-[a-z0-9-]+$/.test(c.name) || typeof c.dir !== "string") { skipped.push({ dir: c?.dir ?? null, name: c?.name ?? null, why: "not a cargo:rightkit-<name> unit" }); continue; }
    units.push({ selector: `cargo:${c.name}`, kind: "cargo", name: c.name, dir: c.dir, deps: [...new Set(c.deps ?? [])].filter((d) => typeof d === "string").map((d) => `cargo:${d}`) });
  }
  for (const p of npm) {
    if (typeof p?.name !== "string" || !/^@rightkit\/[a-z0-9-]+$/.test(p.name) || typeof p.dir !== "string") { skipped.push({ dir: p?.dir ?? null, name: p?.name ?? null, why: "not an npm:@rightkit/<name> unit" }); continue; }
    units.push({ selector: `npm:${p.name}`, kind: "npm", name: p.name, dir: p.dir, deps: [...new Set(p.deps ?? [])].filter((d) => typeof d === "string").map((d) => `npm:${d}`) });
  }
  const known = new Set(units.map((u) => u.selector));
  for (const u of units) {
    const missing = u.deps.filter((d) => !known.has(d));
    if (missing.length) throw new Error(`${u.selector} has unresolved internal dependencies: ${missing.join(", ")}`);
    u.deps = u.deps.filter((d) => d !== u.selector);
  }
  units.sort((a, b) => a.selector.localeCompare(b.selector));
  return { units, skipped };
}

/** Reverse-dependency map of an inventory: selector -> Set of selectors that depend on it directly. */
export function reverseGraph(inventory) {
  const reverse = new Map(inventory.units.map((u) => [u.selector, new Set()]));
  for (const u of inventory.units) for (const d of u.deps) reverse.get(d)?.add(u.selector);
  return reverse;
}

/**
 * Dependency-first order over `selected` (a Set of selectors): every unit appears after the selected units it depends
 * on. Deterministic (alphabetical tiebreak) and cycle-tolerant: a dependency cycle is broken at the back edge and the
 * units in it keep alphabetical order, because a cycle must narrow the guarantee, never hang the planner.
 */
export function dependencyFirstOrder(inventory, selected) {
  const bySelector = new Map(inventory.units.map((u) => [u.selector, u]));
  const state = new Map(); // 1 = visiting, 2 = done
  const order = [];
  const visit = (sel) => {
    if (state.get(sel)) return;
    state.set(sel, 1);
    for (const dep of [...(bySelector.get(sel)?.deps ?? [])].sort()) if (selected.has(dep)) visit(dep);
    state.set(sel, 2);
    order.push(sel);
  };
  for (const sel of [...selected].sort()) visit(sel);
  return order;
}

/**
 * Select the units a change owes: units whose directory changed, units a shared path selects, and every transitive
 * dependent of those. Returns dependency-first `selected` rows ({ selector, kind, name, dir, reason, via }) where
 * reason is "changed", a SHARED_PATH_RULES reason, or "dependent" (via names the nearest selected dependency), plus
 * the changed paths no rule claimed (`unmatched`), so a caller can print exactly what selected nothing.
 */
export function selectAffectedUnits({ changedPaths = [], inventory, sharedRules = SHARED_PATH_RULES }) {
  const reasons = new Map(); // selector -> { reason, via }
  const unmatched = [];
  const byDir = [...inventory.units].sort((a, b) => b.dir.length - a.dir.length);
  const claim = (selector, reason, via = null) => { if (!reasons.has(selector)) reasons.set(selector, { reason, via }); };
  for (const raw of changedPaths) {
    const file = String(raw).split("\\").join("/");
    const owner = byDir.find((u) => file === u.dir || file.startsWith(`${u.dir}/`));
    if (owner) { claim(owner.selector, "changed"); continue; }
    const rule = sharedRules.find((r) => r.paths.includes(file));
    if (rule) {
      const unsupported = inventory.skipped?.filter((u) => rule.kind === null || (rule.kind === "cargo" ? u.dir?.startsWith("crates/") : u.dir?.startsWith("packages/"))) ?? [];
      if (unsupported.length) throw new Error(`${file} affects unsupported units: ${unsupported.map((u) => u.name ?? u.dir).join(", ")}; classify their gate ownership`);
      for (const u of inventory.units) if (rule.kind === null || u.kind === rule.kind) claim(u.selector, rule.reason);
      continue;
    }
    const skippedOwner = inventory.skipped?.find((u) => u.dir && (file === u.dir || file.startsWith(`${u.dir}/`)));
    if (skippedOwner) throw new Error(`${file} belongs to unsupported unit ${skippedOwner.name ?? skippedOwner.dir}; select it through its own gate`);
    unmatched.push(file);
  }
  // Transitive dependents, breadth-first so `via` names a nearest selected dependency.
  const reverse = reverseGraph(inventory);
  const queue = [...reasons.keys()];
  while (queue.length) {
    const sel = queue.shift();
    for (const dependent of [...reverse.get(sel) ?? []].sort()) {
      if (!reasons.has(dependent)) { reasons.set(dependent, { reason: "dependent", via: sel }); queue.push(dependent); }
    }
  }
  const bySelector = new Map(inventory.units.map((u) => [u.selector, u]));
  const selected = dependencyFirstOrder(inventory, new Set(reasons.keys())).map((sel) => ({ ...bySelector.get(sel), ...reasons.get(sel) }));
  return { selected, unmatched: [...new Set(unmatched)].sort() };
}

/** Resolve changes through the union of old and current edges, then keep current runnable units. */
export function selectAffectedAcrossInventories({ changedPaths, oldInventory, inventory }) {
  const old = selectAffectedUnits({ changedPaths, inventory: oldInventory });
  const current = selectAffectedUnits({ changedPaths, inventory });
  const reasons = new Map([...current.selected, ...old.selected].map((row) => [row.selector, { reason: row.reason, via: row.via }]));
  const reverse = new Map();
  for (const unit of [...oldInventory.units, ...inventory.units]) for (const dep of unit.deps) {
    if (!reverse.has(dep)) reverse.set(dep, new Set());
    reverse.get(dep).add(unit.selector);
  }
  const queue = [...reasons.keys()];
  while (queue.length) {
    const sel = queue.shift();
    for (const dependent of [...(reverse.get(sel) ?? [])].sort()) if (!reasons.has(dependent)) {
      reasons.set(dependent, { reason: "dependent", via: sel });
      queue.push(dependent);
    }
  }
  const bySelector = new Map(inventory.units.map((unit) => [unit.selector, unit]));
  const surviving = new Set([...reasons.keys()].filter((sel) => bySelector.has(sel)));
  return {
    selected: dependencyFirstOrder(inventory, surviving).map((sel) => ({ ...bySelector.get(sel), ...reasons.get(sel) })),
    unmatched: current.unmatched.filter((p) => old.unmatched.includes(p)),
  };
}

/**
 * Classify one run's outcomes over the selected units. `outcomes` maps selector -> { ran, code } (code 0 = pass).
 * A unit that did not run because a selected dependency failed or was itself blocked is "blocked" (and names it in
 * `blockedBy`); one that did not run for any other reason is "not-run". Passed/failed keep their exit code. This is
 * the honest four-way split the complete report prints: no blocked unit reads as passed, none as failed.
 */
export function classifyUnitResults({ selected, inventory, outcomes }) {
  const bySelector = new Map(inventory.units.map((u) => [u.selector, u]));
  const selectedSet = new Set(selected.map((s) => s.selector));
  const status = new Map();
  for (const row of selected) {
    const o = outcomes.get(row.selector);
    if (o?.ran) status.set(row.selector, o.code === 0 ? "passed" : "failed");
  }
  const blockedByOf = (sel, seen = new Set()) => {
    if (seen.has(sel)) return null;
    seen.add(sel);
    for (const dep of bySelector.get(sel)?.deps ?? []) {
      if (!selectedSet.has(dep)) continue;
      const s = status.get(dep);
      if (s === "failed" || s === "blocked") return dep;
      if (!outcomes.get(dep)?.ran) { const deeper = blockedByOf(dep, seen); if (deeper) return dep; }
    }
    return null;
  };
  return selected.map((row) => {
    const o = outcomes.get(row.selector);
    if (o?.ran) return { selector: row.selector, status: status.get(row.selector), code: o.code };
    const blockedBy = blockedByOf(row.selector);
    if (blockedBy) { status.set(row.selector, "blocked"); return { selector: row.selector, status: "blocked", blockedBy }; }
    return { selector: row.selector, status: "not-run" };
  });
}

/** The aggregate report object (UNIT_LIST_REPORT_SCHEMA): selection reasons and classified results in run order. */
export function unitListReport({ commit, stage, base = null, selected, results, unmatched = [] }) {
  const bySelector = new Map(results.map((r) => [r.selector, r]));
  const counts = { passed: 0, failed: 0, blocked: 0, "not-run": 0 };
  for (const r of results) counts[r.status] = (counts[r.status] ?? 0) + 1;
  return {
    schema: UNIT_LIST_REPORT_SCHEMA, commit, stage, ...(base ? { base } : {}),
    ok: results.every((r) => r.status === "passed"),
    counts,
    units: selected.map((s) => ({ selector: s.selector, reason: s.reason, ...(s.via ? { via: s.via } : {}), ...(bySelector.get(s.selector) ?? { status: "not-run" }) })),
    ...(unmatched.length ? { unmatchedPaths: unmatched } : {}),
  };
}

export { selectorOf };
