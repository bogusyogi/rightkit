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
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { CARGO_REFUSAL_STATUS } from "./cargo-ledger.mjs";
import { GATE_COMMIT_ENV } from "./gate-seam.mjs";

export const CARRIED_METADATA_REFUSED = "RK_SNAPSHOT_CARRIED_REFUSED";

const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
const within = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };

/** Project Cargo's discovery answer onto snapshot-relative paths so another verified snapshot can use it. */
export function cargoWorkspaceFacts({ root, stdout, realpath = fs.realpathSync }) {
  const metadata = JSON.parse(stdout);
  if (typeof metadata?.workspace_root !== "string" || !Array.isArray(metadata.packages)) throw new Error("cargo metadata lacks workspace_root or packages");
  const resolve = (p) => { try { return realpath(p); } catch { return path.resolve(p); } };
  const rootPath = resolve(root);
  const relative = (absolute) => {
    if (!path.isAbsolute(absolute)) throw new Error(`cargo metadata path ${absolute} is not absolute`);
    const resolved = resolve(absolute);
    if (!within(resolved, rootPath)) throw new Error(`cargo metadata path ${absolute} is outside the repository`);
    return path.relative(rootPath, resolved).replaceAll(path.sep, "/") || ".";
  };
  const cargoDir = relative(metadata.workspace_root);
  const members = metadata.packages.map((pkg) => {
    if (typeof pkg?.name !== "string" || !pkg.name || typeof pkg.manifest_path !== "string") throw new Error("cargo metadata package lacks name or manifest_path");
    const dir = relative(path.dirname(pkg.manifest_path));
    const kinds = (t) => Array.isArray(t.kind) ? t.kind : [];
    const unconditional = (t) => !(t["required-features"] ?? t.required_features ?? []).length;
    const targets = pkg.targets ?? [];
    const bins = targets.filter((t) => kinds(t).includes("bin") && unconditional(t)).map((t) => t.name).sort();
    const testLinksBins = targets.some((t) => kinds(t).includes("test") && t.test !== false && unconditional(t));
    return { name: pkg.name, dir, manifest: path.posix.join(dir, "Cargo.toml"), publish: !(Array.isArray(pkg.publish) && pkg.publish.length === 0), bins, testLinksBins };
  }).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  return { cargoDir, members };
}

const load = createRequire(import.meta.url);

/**
 * Bind discovery to what Cargo can read under the snapshot. An exact local member or dependency has a finite
 * read set. A member glob or path escaping the snapshot needs Cargo to discover its inputs, so leave it unproven.
 */
export function cargoWorkspaceTopology({ root, dir, cargoHome, fsApi = fs, parse = (source) => load("smol-toml").parse(source) }) {
  try {
    const base = fsApi.realpathSync(root);
    const workspace = path.resolve(base, dir);
    if (!within(workspace, base) || !path.isAbsolute(cargoHome ?? "")) return null;
    const files = new Map();
    const targetDirs = new Map();
    const snapshotName = (absolute) => path.relative(base, absolute).replaceAll(path.sep, "/") || ".";
    const stat = (absolute) => {
      let entry;
      try { entry = fsApi.lstatSync(absolute); } catch (error) { if (error.code === "ENOENT" || error.code === "ENOTDIR") return null; throw error; }
      if (entry.isSymbolicLink() || path.relative(absolute, fsApi.realpathSync(absolute))) throw new Error("linked Cargo discovery input");
      return entry;
    };
    const read = (absolute) => {
      const key = within(absolute, base) ? snapshotName(absolute) : absolute;
      const entry = stat(absolute);
      if (entry && !entry.isFile()) throw new Error("Cargo discovery input is not a regular file");
      const bytes = entry ? fsApi.readFileSync(absolute) : null;
      files.set(key, bytes ? createHash("sha256").update(bytes).digest("hex") : null);
      return bytes;
    };
    const local = (at, value) => {
      if (typeof value !== "string" || !value || /[:\\\0]/.test(value) || path.isAbsolute(value)) throw new Error("nonportable Cargo path");
      const resolved = path.resolve(at, value);
      if (!within(resolved, base)) throw new Error("Cargo path escapes snapshot");
      return resolved;
    };
    const inspectConfig = (configDir) => {
      for (const name of ["config", "config.toml"]) {
        const bytes = read(path.join(configDir, name));
        if (!bytes) continue;
        const config = parse(String(bytes));
        if (config.paths !== undefined || config.patch !== undefined || config.include !== undefined) throw new Error("Cargo config extends the discovery read set");
      }
    };
    for (let at = workspace; ; at = path.dirname(at)) {
      inspectConfig(path.join(at, ".cargo"));
      read(path.join(at, "rust-toolchain"));
      read(path.join(at, "rust-toolchain.toml"));
      if (within(at, base)) read(path.join(at, "Cargo.lock"));
      else if (read(path.join(at, "Cargo.toml"))) return null;
      if (path.dirname(at) === at) break;
    }
    inspectConfig(cargoHome);
    const target = (absolute) => {
      const entry = stat(absolute);
      targetDirs.set(snapshotName(absolute), entry ? (entry.isDirectory() ? "dir" : entry.isFile() ? "file" : "other") : null);
      return entry;
    };
    const manifests = new Set();
    const includeAncestors = (start) => {
      for (let at = start; within(at, base); at = path.dirname(at)) {
        manifests.add(path.join(at, "Cargo.toml"));
        if (at === base) break;
      }
    };
    includeAncestors(workspace);
    for (const absolute of manifests) {
      const bytes = read(absolute);
      if (!bytes) {
        if (absolute === path.join(workspace, "Cargo.toml")) return null;
        continue;
      }
      const at = path.dirname(absolute);
      const doc = parse(String(bytes));
      const follow = (parent, value) => {
        const member = local(parent, value);
        if (!stat(path.join(member, "Cargo.toml"))?.isFile()) throw new Error("Cargo dependency or member manifest is missing");
        includeAncestors(member);
      };
      for (const name of ["members", "exclude", "default-members"]) {
        const entries = doc.workspace?.[name] ?? [];
        if (!Array.isArray(entries) || entries.some((item) => typeof item !== "string" || /[*?\[\]{}]/.test(item))) return null;
        if (name !== "exclude") for (const member of entries) follow(at, member);
        else for (const member of entries) local(at, member);
      }
      const dependencyTables = [doc.workspace?.dependencies, doc.replace, ...Object.values(doc.patch ?? {})];
      for (const holder of [doc, ...Object.values(doc.target ?? {})]) {
        for (const name of ["dependencies", "build-dependencies", "dev-dependencies", "build_dependencies", "dev_dependencies"]) dependencyTables.push(holder[name]);
      }
      for (const dependencies of dependencyTables) for (const spec of Object.values(dependencies ?? {})) {
        if (spec?.path !== undefined) follow(at, spec.path);
      }
      if (!doc.package) continue;
      if (doc.package.workspace !== undefined) follow(at, doc.package.workspace);
      if (typeof doc.package.build === "string") target(local(at, doc.package.build));
      for (const name of ["lib", "bin", "test", "bench", "example"]) {
        for (const declared of [].concat(doc[name] ?? [])) if (declared.path !== undefined) target(local(at, declared.path));
      }
      for (const name of ["build.rs", "src/lib.rs", "src/main.rs"]) target(path.join(at, name));
      for (const name of ["src/bin", "tests", "benches", "examples"]) {
        const location = path.join(at, name);
        if (!target(location)?.isDirectory()) continue;
        for (const entry of fsApi.readdirSync(location, { withFileTypes: true })) {
          const candidate = path.join(location, entry.name);
          if (entry.name.endsWith(".rs")) target(candidate);
          else if (entry.isDirectory() || entry.isSymbolicLink()) target(path.join(candidate, "main.rs"));
        }
      }
    }
    const sorted = (entries) => [...entries].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
    return createHash("sha256").update(JSON.stringify([sorted(files), sorted(targetDirs)])).digest("hex");
  } catch {
    return null;
  }
}

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
    const shapeOk = f !== null && f.has("--locked") && f.get("--prefix") === "none" && ["normal,build", "normal,build,dev"].includes(f.get("-e") ?? "") && f.get("-f") === "{p}\t{f}" && packages >= 1;
    return shapeOk ? null : "`tree` is admitted as `tree --locked --prefix none -e normal,build[,dev] -f '{p}<tab>{f}' [--all-features] [--no-default-features] [--features <list>] [--target <triple>]... -p <crate>...` only (the feature-equivalence partition)";
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
/**
 * `served({ cwd, args })`: optional; asks the resident gate for a `cargo metadata` answer it holds for this snapshot while the snapshot is unchanged
 * (gate-serve.mjs snapshotCargoMetadata). Returns `{ status, stdout, stderr }` when the resident gate answered, else null, and the call runs here as
 * before. NN10: each warm run asked Cargo for every workspace's metadata again (6 `cargo metadata` starts per warm run of the witness fixture,
 * 2026-11-17) although the resident gate keeps those answers.
 */
export function directReadSpawn({ spawn, cargoCommand, snapshotRoot, directPath, pathKey = "PATH", tmpRoot, env, err = () => {}, memo = null, isAsync = false, served = null }) {
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
    if (typeof served === "function" && words[0] === "metadata" && opts.stdoutFile === undefined) {
      let answer = null; try { answer = served({ cwd: opts.cwd, args: words }); } catch { answer = null; }
      if (answer && answer.status === 0 && typeof answer.stdout === "string") { const r = remember({ status: 0, signal: null, error: null, stdout: answer.stdout, stderr: answer.stderr ?? "" }); return isAsync ? Promise.resolve(r) : r; }
    }
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
