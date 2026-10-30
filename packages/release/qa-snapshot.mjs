// The QA step of a snapshot stage (rightkit#1301, rustright#190): what `rightkit-qa <args>` may do, and how a compiling `run-tests` step
// becomes two parts. QA runs an already-built artifact outside the broker (a GUI session, the unscrubbed environment), so nothing in the
// step may compile: the gate carries the build itself and then runs QA directly against its listing.
//
//   rightkit-qa run-tests --app a -p crate [QA flags] -- <libtest args>
//     (a) carried:  cargo test -p crate --no-run --message-format=json     stdout -> <listing>   (qa-snapshot-run.mjs)
//     (b) direct:   rightkit-qa run-tests --app a [QA flags] --from-cargo-json <listing> -- <libtest args>
//
// `rightkit-qa journeys run` has no prebuilt form: it spawns `rustright cargo ... --no-run` itself (journeys/build_record.rs spawn_broker)
// and binds the build to the checkout's HEAD, so a snapshot stage cannot carry that build for it. It stays refused, naming the missing mode.
//
// The argument parser of `run-tests` is the Rust CLI's own (crates/rightkit-qa/src/run_tests/cli.rs `parse`): the flags below are its
// VALUE_FLAGS and BOOL_FLAGS, a flag's value is consumed whatever it looks like, and every other word is a cargo argument.
import fs from "node:fs";
import path from "node:path";

/** run-tests flags that take a value (cli.rs VALUE_FLAGS). */
export const RUN_TESTS_VALUE_FLAGS = Object.freeze(["--root", "--qa-config", "--app", "--managed-root", "--run-id", "--parallel", "--binary-timeout-s", "--total-timeout-s", "--compile-timeout-s", "--env", "--unset-env", "--cwd", "--from-cargo-json", "--build-with", "--receipt"]);
/** run-tests flags that take none (cli.rs BOOL_FLAGS). */
export const RUN_TESTS_BOOL_FLAGS = Object.freeze(["--require-broker", "--deny-ignored", "--allow-empty", "--isolate-tmp", "--no-isolate-tmp", "--show-output"]);

/**
 * Every rightkit-qa option that takes the next word as its value, across its subcommands (run-tests, run, journeys, sweep, ...): the
 * run-tests list plus the cargo options run-tests forwards and the flags of the binary's own `Args::positionals`. A word that follows one of
 * these is a value, never a flag.
 */
export const QA_VALUE_OPTIONS = new Set([
  ...RUN_TESTS_VALUE_FLAGS,
  "--manifest-path", "-p", "--package", "--test", "--features",
  "--config", "--app-root", "--tier", "--only", "--engine", "--ui-binary", "--backend", "--dir", "--max-gb", "--young-hours", "--kind", "--tree", "--manifest", "--results", "--evidence-root", "--id", "--platform", "--run-store-cap-bytes",
]);

const splitInline = (word) => {
  const at = word.indexOf("=");
  return at > 0 && word.startsWith("--") ? [word.slice(0, at), word.slice(at + 1)] : [word, undefined];
};

/**
 * `rightkit-qa run-tests`'s arguments, split exactly as the CLI splits them. `own` is the command's own words in order (flag, and its
 * value when separate), `cargo` the words that go to `cargo test --no-run`, `testArgs` what follows the first `--` (null when there is none),
 * `flags` every own flag's values by name. Throws on a value flag with no value (the CLI refuses it too).
 */
export function parseRunTests(words) {
  const own = []; const cargo = []; const flags = new Map();
  let testArgs = null;
  for (let i = 0; i < words.length; i += 1) {
    const arg = words[i];
    if (arg === "--") { testArgs = words.slice(i + 1); break; }
    const [name, inline] = splitInline(arg);
    if (RUN_TESTS_VALUE_FLAGS.includes(name)) {
      let value = inline;
      if (value === undefined) {
        if (i + 1 >= words.length) throw new Error(`${name} needs a value`);
        i += 1; value = words[i];
        own.push(arg, value);
      } else own.push(arg);
      flags.set(name, [...(flags.get(name) ?? []), value]);
    } else if (RUN_TESTS_BOOL_FLAGS.includes(name) && inline === undefined) { own.push(arg); flags.set(name, [...(flags.get(name) ?? []), ""]); }
    else cargo.push(arg);
  }
  return { own, cargo, testArgs, flags };
}

/** The own flags of a non-run-tests QA command (`journeys`, `run`, ...) before `--`, by name, a flag's value consumed so it is never read as a flag. */
function genericFlags(words) {
  const flags = new Map();
  for (let i = 0; i < words.length; i += 1) {
    const arg = words[i];
    if (arg === "--") break;
    const [name, inline] = splitInline(arg);
    if (QA_VALUE_OPTIONS.has(name)) {
      let value = inline;
      if (value === undefined) { i += 1; value = words[i]; }
      if (value !== undefined) flags.set(name, [...(flags.get(name) ?? []), value]);
    } else if (arg.startsWith("-")) flags.set(name, [...(flags.get(name) ?? []), ""]);
  }
  return flags;
}

/**
 * Every value a flag takes anywhere in an argv, `--` included: the Rust `Args` of every command but run-tests reads the WHOLE argv (`has`
 * is any word equal to the flag, `value` the word after the first one), so nothing is skipped as consumed or as after `--`, and a repeat
 * (which Rust ignores) is checked too. A trailing flag with no value yields nothing.
 */
export function everyValue(words, flag) {
  const out = [];
  words.forEach((w, i) => { const [n, inline] = splitInline(w); if (n === flag) { const v = inline ?? words[i + 1]; if (v !== undefined) out.push(v); } });
  return out;
}

/** Whether a word is a flag that makes a QA run build (`--build`, `--build-ui`, `--build-with`, each also as `=value`). */
export const isBuildFlag = (w) => ["--build", "--build-ui", "--build-with"].includes(splitInline(String(w))[0]);

/** The words before the first `--` of an argv. */
export const ownWords = (words) => (words.includes("--") ? words.slice(0, words.indexOf("--")) : words);

/** Whether a QA argv hands `run-tests` an existing cargo listing, read in flag position only (a value that spells the flag is not the flag). */
export function hasListing(args) {
  const words = args.map(String);
  if (words[0] === "run-tests") { try { return parseRunTests(words.slice(1)).flags.has("--from-cargo-json"); } catch { return false; } }
  return genericFlags(ownWords(words)).has("--from-cargo-json");
}

const isMessageFormat = (w) => w === "--message-format" || w.startsWith("--message-format=");

/**
 * What the gate does with a snapshot stage's `rightkit-qa` step that compiles. `null` when it compiles nothing (or is not run-tests);
 * `{ cargoArgs, qaArgs }` for a `run-tests` step with no listing: `cargoArgs` is the carried `cargo` argv (`test ... --no-run
 * --message-format=json`), `qaArgs(listing)` the direct QA argv. `{ refusal }` for a build the gate cannot carry.
 */
export function qaSnapshotPlan(args) {
  const words = args.map(String);
  if (words[0] !== "run-tests") return null;
  let parsed;
  try { parsed = parseRunTests(words.slice(1)); } catch (error) { return { refusal: `rightkit-qa run-tests ${words.slice(1).join(" ")}: ${error.message}` }; }
  if (parsed.flags.has("--from-cargo-json")) return null;
  // --root moves the run's root, but the carried build runs from the step's directory: the two would disagree, and neither app uses it.
  if (parsed.flags.has("--root")) return { code: "RK_QA_SNAPSHOT_ROOT_UNSUPPORTED", refusal: "RK_QA_SNAPSHOT_ROOT_UNSUPPORTED: a carried run-tests build runs from the step's directory, so --root is not supported on a snapshot stage; drop it" };
  const cargo = parsed.cargo.filter((w) => w !== "--no-run");
  const formats = cargo.map((w, i) => (w === "--message-format" ? cargo[i + 1] : w.startsWith("--message-format=") ? w.slice("--message-format=".length) : null)).filter((v) => v !== null);
  if (formats.some((v) => v !== "json")) return { refusal: `rightkit-qa run-tests: a carried build lists its tests as --message-format=json, not ${JSON.stringify(formats.find((v) => v !== "json"))}` };
  const cargoArgs = ["test", ...cargo, "--no-run", ...(cargo.some(isMessageFormat) ? [] : ["--message-format=json"])];
  return { cargoArgs, qaArgs: (listing) => ["run-tests", ...parsed.own, "--from-cargo-json", listing, ...(parsed.testArgs === null ? [] : ["--", ...parsed.testArgs])] };
}

/** `p` resolved as far as it exists (symlinks followed), the rest appended, so a path that does not exist yet still has a real location. */
export function realish(p) {
  let current = path.resolve(p); const rest = [];
  for (;;) {
    try { return path.join(fs.realpathSync.native(current), ...rest); } catch { /* not there yet */ }
    const parent = path.dirname(current);
    if (parent === current) return path.join(current, ...rest);
    rest.unshift(path.basename(current)); current = parent;
  }
}
export const isInside = (child, parent) => { const rel = path.relative(realish(parent), realish(child)); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };

/**
 * Why a QA step's paths are refused on a snapshot stage, or null. The step runs from the commit's snapshot: its working directory must be
 * inside it, and no input it names (`--from-cargo-json`, `--root`, `--cwd <dir>`, `--manifest-path`) may resolve into the primary checkout
 * (the gate never reads or runs a working tree). `cwd` is the step's directory; `snapshot` and `checkouts` come from the gate's carried map.
 */
export function qaPathReason(args, { cwd, snapshot, checkouts, env = {} }) {
  const words = args.map(String);
  const parsedTests = words[0] === "run-tests" ? (() => { try { return parseRunTests(words.slice(1)); } catch { return null; } })() : null;
  // run-tests has its own parser (own flags before `--`); every other command's Args reads the whole argv, so every value counts.
  const values = (flag) => (parsedTests ? parsedTests.flags.get(flag) ?? [] : words[0] === "run-tests" ? genericFlags(ownWords(words)).get(flag) ?? [] : everyValue(words, flag));
  const cwdReal = realish(cwd);
  if (!snapshot || !isInside(cwdReal, snapshot)) return `its working directory ${cwd} is outside the commit's snapshot${snapshot ? ` (${snapshot})` : ""}`;
  const roots = values("--root");
  const named = [];
  for (const r of roots) named.push(["--root", path.resolve(cwdReal, r)]);
  const root = path.resolve(cwdReal, roots[0] ?? ".");
  for (const v of values("--from-cargo-json")) if (v !== "-") named.push(["--from-cargo-json", path.resolve(cwdReal, v)]);
  for (const flag of ["--qa-config", ...(words[0] === "run-tests" ? [] : ["--app-root", "--config"])]) for (const v of values(flag)) named.push([flag, path.resolve(cwdReal, v)]);
  if (typeof env.RIGHTKIT_QA_CONFIG === "string" && env.RIGHTKIT_QA_CONFIG) named.push(["RIGHTKIT_QA_CONFIG", path.resolve(cwdReal, env.RIGHTKIT_QA_CONFIG)]);
  for (const mode of values("--cwd")) if (mode !== "package" && mode !== "workspace") named.push(["--cwd", path.resolve(root, mode)]);
  const manifests = [];
  if (parsedTests) parsedTests.cargo.forEach((w, i, all) => { const [name, inline] = splitInline(w); if (name === "--manifest-path") manifests.push(inline ?? all[i + 1]); });
  else manifests.push(...values("--manifest-path"));
  for (const m of manifests) if (typeof m === "string" && m) named.push(["--manifest-path", path.resolve(root, m)]);
  for (const [flag, resolved] of named) for (const checkout of checkouts) if (isInside(resolved, checkout)) return `${flag} resolves to ${resolved}, inside the primary checkout ${checkout}; a snapshot stage's QA step reads only the snapshot and managed output`;
  return null;
}
