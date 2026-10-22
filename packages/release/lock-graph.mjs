// Pure Cargo.lock graph functions shipped with @rightkit/release for rightkit-gate (the local gate).
// KEEP IDENTICAL to parseLock, changedLockNames and reach in scripts/ci/select.mjs (RightKit CI, not shipped);
// rightkit-gate.test.mjs compares the two sources and fails on any difference. No process, no filesystem.

/**
 * Parse Cargo.lock into name -> { signatures: string[], deps: Set<name> }.
 * A package's signature covers version, source, checksum and its dependency list, so any change
 * to what gets compiled changes it. Dependency edges are by name: over-approximate, never under.
 */
export function parseLock(text) {
  const packages = new Map();
  for (const block of String(text ?? "").split(/^\[\[package\]\]\s*$/m).slice(1)) {
    const name = /^name = "([^"]+)"/m.exec(block)?.[1];
    if (!name) continue;
    const field = (key) => new RegExp(`^${key} = "([^"]*)"`, "m").exec(block)?.[1] ?? "";
    const depsRaw = /^dependencies = \[([^\]]*)\]/m.exec(block)?.[1] ?? "";
    const deps = [...depsRaw.matchAll(/"([^"\s]+)[^"]*"/g)].map((m) => m[1]);
    const entry = packages.get(name) ?? { signatures: [], deps: new Set() };
    entry.signatures.push([field("version"), field("source"), field("checksum"), deps.join(",")].join("|"));
    for (const dep of deps) entry.deps.add(dep);
    packages.set(name, entry);
  }
  for (const entry of packages.values()) entry.signatures.sort();
  return packages;
}

/** Names whose lock entry is new or different in `next`. */
export function changedLockNames(previous, next) {
  const changed = new Set();
  for (const [name, entry] of next) {
    const before = previous.get(name);
    if (!before || before.signatures.join("\n") !== entry.signatures.join("\n")) changed.add(name);
  }
  return changed;
}

/** Every package name reachable from `name` through the lock graph, `name` excluded. */
export function reach(lock, name, memo = new Map()) {
  if (memo.has(name)) return memo.get(name);
  const seen = new Set();
  const stack = [...(lock.get(name)?.deps ?? [])];
  while (stack.length) {
    const next = stack.pop();
    if (seen.has(next)) continue;
    seen.add(next);
    for (const dep of lock.get(next)?.deps ?? []) stack.push(dep);
  }
  seen.delete(name);
  memo.set(name, seen);
  return seen;
}
