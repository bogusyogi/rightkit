// `right-release deps --audit` for Cargo workdirs (Orthic-Labs/rightkit#674): replaces an app's own audit-rust script.
//
//   deps: { workdirs: ["."], cargoWorkdirs: ["src-tauri"] }   // app-relative folders that hold a Cargo.lock
//
// Declared only: an app with no `deps.cargoWorkdirs` audits no Rust, exactly as before. Each folder must be inside the app and
// hold a `Cargo.lock` (an unlocked workspace cannot be audited, so it is refused, never skipped). `cargo audit` runs through
// the managed cargo client (`runCargoSync`): on a managed host it goes through the RightKit broker, and a host with no
// broker or no `cargo-audit` fails closed with the reason; there is never a bare-cargo fallback on a managed Mac.
import { existsSync } from "node:fs";
import path from "node:path";
import { runCargoSync } from "./managed-cargo.mjs";

const isInside = (root, target) => {
  const relative = path.relative(root, target);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
};

/** The validated Cargo workdirs `[{ rel, cwd }]` of a release config (empty when none are declared). Throws on a bad declaration. */
export function cargoAuditWorkdirs(config, root, { exists = existsSync } = {}) {
  const declared = config?.deps?.cargoWorkdirs;
  if (declared === undefined) return [];
  if (!Array.isArray(declared) || declared.some((item) => typeof item !== "string" || !item.trim())) {
    throw new Error("deps.cargoWorkdirs must be a list of app-relative folders");
  }
  const seen = new Set();
  const workdirs = [];
  for (const rel of declared) {
    const normalized = rel.replaceAll("\\", "/");
    if (path.isAbsolute(rel) || /^[A-Za-z]:/.test(rel) || normalized.split("/").includes("..")) {
      throw new Error(`deps.cargoWorkdirs entry must be relative and stay inside the app: ${rel}`);
    }
    const cwd = path.resolve(root, normalized);
    if (!isInside(root, cwd)) throw new Error(`deps.cargoWorkdirs entry must stay inside the app: ${rel}`);
    if (seen.has(cwd)) continue;
    seen.add(cwd);
    if (!exists(path.join(cwd, "Cargo.lock"))) throw new Error(`deps.cargoWorkdirs entry ${rel} has no Cargo.lock (${path.join(cwd, "Cargo.lock")}); an unlocked workspace cannot be audited`);
    workdirs.push({ rel, cwd });
  }
  return workdirs;
}

/**
 * Run `cargo audit` in each workdir and collect failures so every workdir reports. `run(args, options)` is the managed
 * cargo runner (injectable). Returns `{ failures: string[] }`: one entry per workdir that could not start or exited non-zero.
 */
export function runCargoAudits(workdirs, { run = runCargoSync, log = console.log, env = process.env } = {}) {
  const failures = [];
  for (const { rel, cwd } of workdirs) {
    log(`\n> cargo audit  (${cwd})`);
    const result = run(["audit"], { cwd, env, stdio: "inherit" });
    if (result?.error) failures.push(`${rel}: cargo audit could not start (${result.error.message})`);
    else if (result?.status !== 0) failures.push(`${rel}: cargo audit exited ${result?.status ?? "without status"}`);
  }
  return { failures };
}
