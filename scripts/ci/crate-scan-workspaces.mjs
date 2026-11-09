// Which Cargo workspace owns a crate, for the class A scans gate.mjs plans in a sharded run (cargo-metadata, cargo-deny, cargo-audit
// run with the workspace root as cwd, and cargo-audit reads that directory's Cargo.lock and never walks up to a parent's).
// RightKit is one Cargo workspace with one Cargo.lock at the root (rightkit#1505); a crate that declares its own `[workspace]`
// (crates/rightkit-mesh-ffi) is a separate root with its own lock. The owner is the nearest directory, the crate's own first, that
// holds a Cargo.toml with a `[workspace]` table or a Cargo.lock; the repository root is the last resort. Pure: no cargo, manifests only.
import fs from "node:fs";
import path from "node:path";

const defaults = { readFile: (f) => fs.readFileSync(f, "utf8"), exists: fs.existsSync };
const WORKSPACE_TABLE = /^\s*\[workspace(?:\.[^\]]*)?\]\s*(?:#.*)?$/m;

/** The repository-relative directory ("." for the root) of the Cargo workspace that owns the crate in `crateDir`. */
export function owningWorkspace(root, crateDir, io = {}) {
  const { readFile, exists } = { ...defaults, ...io };
  let dir = path.posix.normalize(String(crateDir).split(path.sep).join("/")).replace(/\/+$/, "") || ".";
  for (;;) {
    const at = (f) => path.join(root, dir === "." ? f : `${dir}/${f}`);
    if (exists(at("Cargo.toml"))) {
      let text = "";
      try { text = readFile(at("Cargo.toml")); } catch { /* unreadable manifest: fall through to the lock */ }
      if (WORKSPACE_TABLE.test(String(text))) return dir;
    }
    if (exists(at("Cargo.lock"))) return dir;
    if (dir === "." || dir === "") return ".";
    const parent = path.posix.dirname(dir);
    dir = parent === "" ? "." : parent;
  }
}

/** The scan identity of a unit, as rightkit-gate names it (rust-inventory.mjs rustUnitName): the plain crate for the root or a workspace that is the crate's own directory. */
export const scanUnitName = (cargoDir, crate) => (cargoDir === "." || cargoDir === `crates/${crate}` ? crate : `${cargoDir}:${crate}`);

/** planScans rustUnits for every `{ name, dir }` crate workspace entry: each carries the cargoDir that owns it. */
export function scanRustUnits(root, crates, io = {}) {
  return crates.map((w) => {
    const cargoDir = owningWorkspace(root, w.dir, io);
    return { name: scanUnitName(cargoDir, w.name), dir: w.dir, cargoDir, crate: w.name };
  });
}

/** The distinct Cargo workspaces of those units, root first: one set of workspace scans each, however many crates it holds. */
export const scanWorkspaceDirs = (units) => [...new Set(units.map((u) => u.cargoDir))].sort((a, b) => (a === "." ? -1 : b === "." ? 1 : a < b ? -1 : 1));
