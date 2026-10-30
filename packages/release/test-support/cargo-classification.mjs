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
import { GATE_EXTERNAL_COMMANDS, classifyCargoArgv } from "../cargo-shapes.mjs";
import { carriedScanRefusal } from "../snapshot-metadata.mjs";

export const READ_ONLY_NEEDS = "requires rustright read-only snapshot acceptance (rightkit#1301 F1)";
export const SNAPSHOT_MUST_COMPILE = "RK_CHECK_ERROR: a snapshot request must compile";

const hasFlag = (argv, flag) => argv.includes(flag);

/**
 * `compile` (admitted as a snapshot request), `read-only` (CONTROL, ANALYSIS or WORKSPACE_OBSERVE), `write` (WORKSPACE_WRITE: refused), `denied`.
 * GB-82: whether a request compiles or is refused is the production grammar (cargo-shapes.mjs, exercised against RustRight's cargo-shapes vector), not a grammar of
 * this fixture's own; only the descriptive read-only/write split below is the fixture's.
 */
export function classifyCargo(argv) {
  const row = classifyCargoArgv(argv, { registered: GATE_EXTERNAL_COMMANDS });
  if (row.refused) return row.refused.code === "RK_UNKNOWN_COMMAND" ? "read-only" : "denied";
  if (row.compile) return "compile";
  const words = [...argv];
  const command = row.command;
  const locked = hasFlag(words, "--locked") || hasFlag(words, "--frozen");
  if (command === "metadata" && hasFlag(words, "--no-deps")) return "read-only";
  if (command === "metadata" || command === "tree" || command === "fetch") return locked ? "read-only" : "write";
  if (command === "fmt") return words.includes("--check") ? "read-only" : "write";
  if (["new", "init", "update", "vendor", "generate-lockfile", "add", "remove"].includes(command)) return "write";
  return "read-only"; // CONTROL, ANALYSIS and an unclassified plugin (about, audit, deny) are no BUILD capability: never a compile
}

/**
 * What the broker accepts as a snapshot request (GB-82: the vector's own decision): `compile` rows, and, unless `readOnly` is false (a broker that predates
 * RustRight's read-only snapshot acceptance), the carried read-only shapes, none of which carries cargo's `--config` or `-Z`. The files `deny` reads and the database
 * directory `audit` reads must also be absolute, exist, be of the right kind and lie OUTSIDE the live checkout (`checkout`; with none only the shape is judged).
 */
export function acceptedAfterFix(argv, { checkout = null, readOnly = true } = {}) {
  const row = classifyCargoArgv(argv, { registered: GATE_EXTERNAL_COMMANDS });
  if (row.refused || row.carriedRefusal) return false;
  if (row.compile) return true;
  if (!readOnly || !row.carriedReadOnly) return false;
  if (["--version", "-V", "version", "locate-project", "read-manifest", "about"].includes(row.command)) return true;
  return carriedScanRefusal(row.normalizedArgv, { checkout }) === null;
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
