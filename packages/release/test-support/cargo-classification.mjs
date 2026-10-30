// What the RustRight broker does with each cargo subcommand a snapshot stage carries (rightkit#1301 F1).
//
// Source: rustright-policy `command_policy.rs` `cargo_capability` (the classification) and rustright-broker `job_service/service.rs:1201`
// ("a snapshot request must compile": a request that carries a snapshot is admitted only when its capability compiles). Only the
// commands `cargo_capability` maps to BUILD (and CARGO_RUN, which is BUILD plus resident-after-build) compile; every other command is
// CONTROL, ANALYSIS or WORKSPACE_OBSERVE and is refused as a snapshot request today. RustRight will accept snapshot-bound read-only
// subcommands and name the bound output directory in their receipts (rk4 decision); until then `READ_ONLY_NEEDS` marks every such call.
//
// The table is a FIXTURE of that source, not a copy of its code: `classifyCargo` below mirrors the match arms, and the contract test
// (cargo-classification.test.mjs) pins both to the table, so a change in either is a failing test, not a silent drift.
import fs from "node:fs";
import path from "node:path";
import { cargoSubcommandOf } from "../snapshot-request.mjs";

export const READ_ONLY_NEEDS = "requires rustright read-only snapshot acceptance (rightkit#1301 F1)";
export const SNAPSHOT_MUST_COMPILE = "RK_CHECK_ERROR: a snapshot request must compile";

const CONTROL = ["--version", "-V", "-vV", "version", "help", "locate-project", "read-manifest", "logout", "owner", "yank"];
const ANALYSIS = ["search", "info", "pkgid", "verify-project", "report", "config"];
const COMPILE = ["build", "check", "test", "bench", "clippy", "doc", "rustc", "rustdoc", "fix", "publish", "run"];
const hasFlag = (argv, flag) => argv.includes(flag);
const observationOnly = (argv) => {
  // build_rules.rs observation_only_cargo, reduced to what the carried calls pass: `package` with --list and one -p, `metadata` and `tree` without `--` or +toolchain.
  return !argv.includes("--") && !argv.some((a) => a.startsWith("+"));
};

/** `compile` (admitted as a snapshot request), `read-only` (CONTROL, ANALYSIS or WORKSPACE_OBSERVE: refused as one today), `write` (WORKSPACE_WRITE: refused), `denied`. */
export function classifyCargo(argv) {
  const words = [...argv];
  const first = words.find((w) => !w.startsWith("+")) ?? null;
  const command = ["--version", "-V", "-vV"].includes(first) ? first : cargoSubcommandOf(words);
  if (command === null) return "read-only";
  if (["install", "uninstall"].includes(command)) return "denied";
  if (CONTROL.includes(command) || ANALYSIS.includes(command)) return "read-only";
  const locked = hasFlag(words, "--locked") || hasFlag(words, "--frozen");
  if (command === "metadata" && hasFlag(words, "--no-deps")) return "read-only";
  if (command === "metadata" || command === "tree" || command === "fetch") return locked ? "read-only" : "write";
  if (command === "fmt") return words.includes("--check") ? "read-only" : "write";
  if (["new", "init", "update", "vendor", "generate-lockfile", "add", "remove"].includes(command)) return "write";
  if (command === "clean") return "denied";
  if (command === "package") return words.includes("--list") && words.includes("--locked") && observationOnly(words) ? "read-only" : "compile";
  if (COMPILE.includes(command)) return "compile";
  return "read-only"; // an unclassified command (a plugin such as about, audit or deny) is no BUILD capability: never a compile
}

/**
 * What the broker accepts as a snapshot request AFTER RustRight's change (rk4 decision, rightkit#1301 B2; rustright 52a9ed76 for the scan
 * shapes): the compile commands, plus these read-only shapes and nothing else, each flag at most once and in spaced form (`--flag=value` is
 * not admitted):
 *   cargo metadata [--no-deps] [--locked] [--frozen] [--offline] [--format-version 1]   (with dependencies only with --locked or --frozen)
 *   cargo fetch --locked
 *   cargo deny [--frozen] [--metadata-path <file>] [--config <file>] check              (nothing after `check`)
 *   cargo audit --db <dir> --no-fetch
 *   cargo about ... --frozen                                                              (right-release's licence generation)
 * The files deny reads and audit's database directory must be absolute, existing, of the right kind and NOT inside the live checkout
 * (`checkout`; when it is not given only the shape is judged). Cargo's own `--config` and `-Z`, a `+toolchain`, `--manifest-path` and a `--`
 * are never part of a scan shape. A test fake of the broker refuses what the real broker refuses, so this is written from the broker's rules,
 * not from the gate's own shape checker (snapshot-metadata.mjs).
 */
export function acceptedAfterFix(argv, { checkout = null } = {}) {
  if (classifyCargo(argv) === "compile") return true;
  const words = [...argv];
  if (cargoSubcommandOf(words) === "about") return (words.includes("--") ? words.slice(0, words.indexOf("--")) : words).includes("--frozen");
  const [sub, ...rest] = words;
  const parse = (list, spec) => {
    const seen = new Map();
    for (let i = 0; i < list.length; i += 1) {
      const w = list[i];
      if (!Object.hasOwn(spec, w) || seen.has(w)) return null;
      if (spec[w]) { const v = list[i + 1]; if (typeof v !== "string" || v === "" || v.startsWith("-")) return null; seen.set(w, v); i += 1; } else seen.set(w, true);
    }
    return seen;
  };
  const realOf = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
  const placed = (value, kind) => {
    if (!path.isAbsolute(value)) return false;
    if (checkout === null) return true;
    let st; try { st = fs.statSync(value); } catch { return false; }
    if (kind === "file" ? !st.isFile() : !st.isDirectory()) return false;
    const rel = path.relative(realOf(checkout), realOf(value));
    return rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel); // outside the live checkout
  };
  if (sub === "metadata") {
    const f = parse(rest, { "--no-deps": false, "--locked": false, "--frozen": false, "--offline": false, "--format-version": true });
    return f !== null && (!f.has("--format-version") || f.get("--format-version") === "1") && (f.has("--no-deps") || f.has("--locked") || f.has("--frozen"));
  }
  if (sub === "fetch") return rest.length === 1 && rest[0] === "--locked";
  if (sub === "deny") {
    if (rest.at(-1) !== "check") return false;
    const f = parse(rest.slice(0, -1), { "--frozen": false, "--metadata-path": true, "--config": true });
    return f !== null && (!f.has("--metadata-path") || placed(f.get("--metadata-path"), "file")) && (!f.has("--config") || placed(f.get("--config"), "file"));
  }
  if (sub === "audit") {
    const f = parse(rest, { "--db": true, "--no-fetch": false });
    return f !== null && f.size === 2 && f.has("--db") && f.has("--no-fetch") && placed(f.get("--db"), "directory");
  }
  return false;
}

/**
 * Every cargo subcommand a snapshot stage carries, with the broker's classification today. `callers` names who carries it:
 * `seam` (runCargoSync of right-release), `wrapper` (the broker-mode PATH wrapper for third-party children), `gate` (the gate's own),
 * `tauri` (the Tauri CLI, through the wrapper).
 */
export const CARGO_CLASSIFICATION = Object.freeze([
  { argv: ["build", "--locked", "-p", "a"], class: "compile", callers: ["seam", "wrapper", "tauri"] },
  { argv: ["check"], class: "compile", callers: ["seam", "wrapper", "gate"] },
  { argv: ["test", "--locked"], class: "compile", callers: ["seam", "wrapper", "gate"] },
  { argv: ["bench"], class: "compile", callers: ["wrapper"] },
  { argv: ["clippy", "--locked"], class: "compile", callers: ["wrapper", "gate"] },
  { argv: ["doc"], class: "compile", callers: ["wrapper", "gate"] },
  { argv: ["rustc", "--", "-vV"], class: "compile", callers: ["wrapper"] },
  { argv: ["rustdoc"], class: "compile", callers: ["wrapper"] },
  { argv: ["run", "-p", "a"], class: "compile", callers: ["seam", "wrapper"] },
  { argv: ["package", "-p", "a"], class: "compile", callers: ["wrapper"] },
  { argv: ["metadata", "--format-version", "1", "--no-deps"], class: "read-only", callers: ["seam", "wrapper", "tauri"], needs: READ_ONLY_NEEDS },
  { argv: ["metadata", "--offline", "--format-version", "1", "--no-deps"], class: "read-only", callers: ["seam", "wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["metadata", "--frozen", "--format-version", "1"], class: "read-only", callers: ["seam", "wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["metadata", "--locked", "--format-version", "1"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["locate-project", "--workspace"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["--version"], class: "read-only", callers: ["wrapper", "seam"], needs: READ_ONLY_NEEDS },
  { argv: ["version"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["help"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["read-manifest"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["tree", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["pkgid", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["verify-project", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["fetch", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["fmt", "--check"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["audit"], class: "read-only", callers: ["seam"], needs: READ_ONLY_NEEDS },
  { argv: ["deny", "check"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["about", "generate", "--frozen", "--fail"], class: "read-only", callers: ["seam"], needs: READ_ONLY_NEEDS },
  { argv: ["about", "generate", "--locked", "--fail"], class: "read-only", callers: [], needs: READ_ONLY_NEEDS },
  { argv: ["fmt"], class: "write", callers: [] },
  { argv: ["metadata", "--format-version", "1"], class: "write", callers: [] },
  { argv: ["clean"], class: "denied", callers: [] },
  { argv: ["install", "x"], class: "denied", callers: [] },
]);
