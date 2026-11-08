// The gate's own read-only Cargo calls inside a managed snapshot on a broker host (rightkit#1311).
//
// RustRight refuses a ROOT request whose workspace is inside a managed snapshot ("a root request cannot run inside a managed snapshot",
// rustright job_service/service.rs). The gate reads the snapshot's Cargo metadata to discover units, and every Class A Cargo scan of a
// snapshot (metadata, fetch, cargo-deny, cargo-audit) reads the snapshot too, never the primary checkout (docs/GATE-CONTRACT.md: no
// dirty-tree condition; class A for a build reads the snapshot). So on a broker host each such call runs DIRECTLY with the host's toolchain cargo
// (GATE-CONTRACT GC-A11, directReadSpawn below), never as a broker request; the exact read-only shapes below (the broker's snapshot_read_only
// shapes, rustright_policy::command_policy) are the only ones, and every other call inside the snapshot is refused with RK_SNAPSHOT_CARRIED_REFUSED,
// never run as a root request.
//
//   cargo metadata [--no-deps] [--locked] [--frozen] [--offline] [--format-version 1]   (with dependencies: --locked or --frozen)
//   cargo fetch --locked
//   cargo deny [--frozen] [--metadata-path <file>] [--config <file>] check              (each flag at most once, spaced form, nothing after `check`)
//   cargo audit --db <dir> --no-fetch
//   cargo fmt -p <unit> --check | cargo fmt --all --check                                 (command_policy.rs fmt_check_shape; check only, nothing after)
//
// Cargo's own `--config` and `-Z`, a `+toolchain`, `--manifest-path`, a `--` and every other word never ride along. The files deny reads and
// the database directory audit reads are absolute, exist (a file; a directory) and lie outside the live checkout.
import fs from "node:fs";
import path from "node:path";
import { CARGO_REFUSAL_STATUS } from "./cargo-ledger.mjs";
import { GATE_COMMIT_ENV } from "./gate-seam.mjs";

export const CARRIED_METADATA_REFUSED = "RK_SNAPSHOT_CARRIED_REFUSED";

const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
const within = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };

/** The flags of `rest` as a Map (flag to value, or true); null when one repeats, is unknown (`--flag=value` included) or lacks its value. */
function flagsOf(rest, spec) {
  const found = new Map();
  for (let i = 0; i < rest.length; i += 1) {
    const w = rest[i];
    if (!Object.hasOwn(spec, w) || found.has(w)) return null;
    if (spec[w] === "value") {
      const v = rest[i + 1];
      if (typeof v !== "string" || v === "" || v.startsWith("-")) return null;
      found.set(w, v); i += 1;
    } else found.set(w, true);
  }
  return found;
}

/**
 * Why the broker would not admit `words` (cargo's argv without `cargo`) as a carried read-only request, or null when it does. Without
 * `checkout` only the shape (and that the named paths are absolute) is judged; with it (the live primary checkout) the files the call
 * names must also exist, be of the right kind, and lie outside that checkout.
 */
export function carriedScanRefusal(words, { checkout = null } = {}) {
  if (!Array.isArray(words) || !words.every((w) => typeof w === "string")) return "the argv is not a list of words";
  const [sub, ...rest] = words;
  const pathProblem = (flag, value, kind) => {
    if (!path.isAbsolute(value)) return `${flag} ${value} is not absolute`;
    if (checkout === null) return null;
    let stat; try { stat = fs.statSync(value); } catch { return `${flag} ${value} does not exist`; }
    if (kind === "file" ? !stat.isFile() : !stat.isDirectory()) return `${flag} ${value} is not ${kind === "file" ? "a file" : "a directory"}`;
    return within(real(value), real(checkout)) ? `${flag} ${value} is inside the live checkout ${checkout}` : null;
  };
  if (sub === "metadata") {
    const f = flagsOf(rest, { "--no-deps": "flag", "--locked": "flag", "--frozen": "flag", "--offline": "flag", "--format-version": "value" });
    if (f === null) return "`metadata` takes only --no-deps, --locked, --frozen, --offline and --format-version 1, each once";
    if (f.has("--format-version") && f.get("--format-version") !== "1") return "`metadata` takes --format-version 1 only";
    if (!f.has("--no-deps") && !f.has("--locked") && !f.has("--frozen")) return "`metadata` with dependencies must be --locked or --frozen";
    return null;
  }
  if (sub === "fetch") return rest.length === 1 && rest[0] === "--locked" ? null : "`fetch` is admitted as `fetch --locked` only";
  if (sub === "deny") {
    if (rest.at(-1) !== "check") return "`deny` must end with `check`, with nothing after it";
    const f = flagsOf(rest.slice(0, -1), { "--frozen": "flag", "--metadata-path": "value", "--config": "value" });
    if (f === null) return "`deny` takes only --frozen, --metadata-path <file> and --config <file> before `check`, each once, spaced form";
    for (const flag of ["--metadata-path", "--config"]) if (f.has(flag)) { const problem = pathProblem(flag, f.get(flag), "file"); if (problem) return problem; }
    return null;
  }
  // rustright command_policy.rs fmt_check_shape: exactly `-p <unit> --check` or `--all --check`; no `--`, so no rustfmt argument reaches it.
  if (sub === "fmt") {
    const shapeOk = (rest.length === 3 && rest[0] === "-p" && rest[1] !== "" && !rest[1].startsWith("-") && rest[2] === "--check") || (rest.length === 2 && rest[0] === "--all" && rest[1] === "--check");
    return shapeOk ? null : "`fmt` is admitted as `fmt -p <unit> --check` or `fmt --all --check` only";
  }
  if (sub === "audit") {
    const f = flagsOf(rest, { "--db": "value", "--no-fetch": "flag" });
    if (f === null || f.size !== 2 || !f.has("--db") || !f.has("--no-fetch")) return "`audit` is admitted as `audit --db <dir> --no-fetch` only";
    return pathProblem("--db", f.get("--db"), "directory");
  }
  // rustright command_policy.rs:407-409: class_a_scan "package" -> observation_only_cargo. Exactly: -p <unit> --list --locked [--allow-dirty].
  if (sub === "package") {
    const unit = rest[1];
    // `-p <unit>` first, then --list and --locked and optionally --allow-dirty, each once in either order (the gate plans `--allow-dirty --locked`).
    const tail = rest.slice(2);
    const shapeOk = rest[0] === "-p" && typeof unit === "string" && unit !== "" && !unit.startsWith("-") && new Set(tail).size === tail.length && tail.every((w) => ["--list", "--locked", "--allow-dirty"].includes(w)) && tail.includes("--list") && tail.includes("--locked");
    return shapeOk ? null : "`package` is admitted as `package -p <unit> --list --locked [--allow-dirty]` only";
  }
  // The feature-equivalence partition's read (feature-partition.mjs treeArgs; GC-A18): resolves features, compiles nothing, writes only to the scratch target it is given.
  if (sub === "tree") {
    const own = []; let packages = 0; let targets = 0;
    for (let i = 0; i < rest.length; i += 1) {
      const w = rest[i]; const v = rest[i + 1];
      if (w === "-p") { if (typeof v !== "string" || !/^[A-Za-z0-9_-]+$/.test(v)) return "`tree` takes -p <crate> with a plain crate name"; packages += 1; i += 1; }
      else if (w === "--target") { if (typeof v !== "string" || !/^[A-Za-z0-9_-]+$/.test(v)) return "`tree` takes --target <triple> with a plain triple"; targets += 1; i += 1; }
      else own.push(w);
    }
    const f = flagsOf(own, { "--locked": "flag", "--prefix": "value", "-e": "value", "-f": "value", "--all-features": "flag", "--no-default-features": "flag", "--features": "value", "-F": "value" });
    const shapeOk = f !== null && f.has("--locked") && f.get("--prefix") === "none" && ["normal,build", "normal,build,dev"].includes(f.get("-e") ?? "") && f.get("-f") === "{p}\t{f}" && packages >= 1 && targets <= 1;
    return shapeOk ? null : "`tree` is admitted as `tree --locked --prefix none -e normal,build[,dev] -f '{p}<tab>{f}' [--all-features] [--no-default-features] [--features <list>] [--target <triple>] -p <crate>...` only (the feature-equivalence partition)";
  }
  return `\`cargo ${words.join(" ")}\` is not one of the carried read-only shapes (metadata, fetch --locked, deny, audit --db --no-fetch, fmt --check, package -p <unit> --list --locked, tree)`;
}

/** Whether the broker admits `words` as a carried read-only request (see carriedScanRefusal). */
export const carriedReadOnlyScan = (words, options) => carriedScanRefusal(words, options) === null;

/**
 * GATE-CONTRACT GC-A11 (Adrian, 2026-11-02: "Class a never in the broker. Any cargo things that are cheap and non compiling are in class a,
 * can run outside the broker."): the same read-only shapes as above, run DIRECTLY with the host's real toolchain cargo, never carried to the
 * broker. `directPath` is the PATH with RustRight shims and outer gate wrappers taken off (managed-cargo.mjs directCargoPath), so `cargo`
 * resolves to the pinned toolchain, not the broker's shim. Each call writes only to its own scratch target under `tmpRoot` (cargo would
 * otherwise write target/.rustc_info.json beside the manifest and the snapshot would no longer be the commit's tree); the scratch is removed
 * when the call returns. Every other cargo call inside the snapshot is refused (a compile belongs to the broker); calls outside the snapshot
 * and every non-cargo command pass through to `spawn` unchanged. `spawn` may be synchronous or return a promise.
 *
 * `memo` (a Map the caller shares between the synchronous and the asynchronous spawn of one run; `isAsync` says which this one is) answers a
 * repeated metadata call from the first one's result (rightkit#1460): the gate reads each workspace's `metadata --no-deps --locked` once to
 * discover units and again as the Class A `cargo-metadata` scan, in the same verified snapshot, with the same toolchain and registry state, so the
 * second run can only repeat the first. Only a successful call that streams to no file is kept (a failure is never replayed), keyed on the snapshot
 * directory it ran in and its exact words; every other call (fetch, deny, audit, package, fmt) always runs.
 */
export function directReadSpawn({ spawn, cargoCommand, snapshotRoot, directPath, pathKey = "PATH", tmpRoot, env, err = () => {}, memo = null, isAsync = false }) {
  const prefix = cargoCommand.slice(1);
  const refused = (code, message) => { err(`rightkit-gate: ${code}: ${message}`); return { status: CARGO_REFUSAL_STATUS, signal: null, error: null, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  return (cmd, args, opts = {}) => {
    const cargo = cmd === cargoCommand[0] && prefix.every((w, i) => args[i] === w);
    if (!cargo || typeof opts.cwd !== "string" || !within(real(opts.cwd), real(snapshotRoot))) return spawn(cmd, args, opts);
    // A seam request (reset plan section 3: RUSTRIGHT_GATE_COMMIT set) is a class B compile the broker admits; it runs in the snapshot by design and is never a
    // class A direct read, so it goes to the declared client untouched (rightkit#1542: this wrapper refused every seam compile as "not a read-only shape").
    if (opts.env && typeof opts.env[GATE_COMMIT_ENV] === "string" && opts.env[GATE_COMMIT_ENV]) return spawn(cmd, args, opts);
    const words = args.slice(prefix.length);
    const shape = words.length === 1 && words[0] === "--version" ? null : carriedScanRefusal(words);
    if (shape !== null) return refused(CARRIED_METADATA_REFUSED, `\`cargo ${words.join(" ")}\` would run inside the managed snapshot ${snapshotRoot} and is not one of the read-only shapes Class A runs directly (GC-A11): ${shape}`);
    const memoKey = memo && words[0] === "metadata" && opts.stdoutFile === undefined ? `${real(opts.cwd)}\0${words.join(" ")}` : null;
    if (memoKey !== null && memo.has(memoKey)) { const hit = { ...memo.get(memoKey) }; return isAsync ? Promise.resolve(hit) : hit; }
    const remember = (result) => { if (memoKey !== null && result && !result.error && result.status === 0 && typeof result.stdout === "string") memo.set(memoKey, { status: 0, signal: null, error: null, stdout: result.stdout, stderr: result.stderr ?? "" }); return result; };
    let scratch;
    try { fs.mkdirSync(tmpRoot, { recursive: true }); scratch = fs.mkdtempSync(path.join(tmpRoot, "rk-class-a-cargo-")); }
    catch (e) { return refused("RK_CLASS_A_SCRATCH", `cannot create a scratch target under ${tmpRoot}: ${e.message}`); }
    const childEnv = { ...env, ...(opts.env ?? {}), [pathKey]: directPath, CARGO_TARGET_DIR: scratch };
    const cleanup = () => { try { fs.rmSync(scratch, { recursive: true, force: true }); } catch { /* best effort: a scratch dir under the gate's tmp root */ } };
    let r;
    try { r = spawn("cargo", words, { ...opts, env: childEnv }); }
    catch (e) { cleanup(); throw e; }
    if (r && typeof r.then === "function") return r.then(remember).finally(cleanup);
    cleanup();
    return remember(r);
  };
}
