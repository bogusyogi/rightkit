import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { appliesOn, groupUnits, parseSelectTokens, parseSteps, planHost, planHygiene, rustWeight, sanitizeId, shardsConfig, summaryVerdict, evidenceVerdict, requiredConsumerLockCommands, requiredShardCommands, SWIFT_ROOT_MANIFEST } from "./shards.mjs";
import { scanTargets } from "./secret-scan.mjs";

const crates = ["rightkit-a", "rightkit-b", "rightkit-c", "rightkit-d", "rightkit-e", "rightkit-heavy", "rightkit-win"];
const config = {
  rust: {
    // 3 feature sets, rerun serially: 6 test commands + 3 clippy; --all-features removes extra checks.
    "rightkit-heavy": { featureSets: [["--all-features"], [], ["--features", "x"]], alsoSerial: true },
    "rightkit-win": { os: ["windows"] },
  },
};
const universe = { rust: crates, npm: ["packages/p1", "packages/p2", "packages/p3", "packages/p4"] };
const empty = { rust: { all: false, test: [], check: [] }, npm: { all: false, test: [] }, root: [], swift: [] };
const plan = (selection, host = "windows", shards = {}) => planHost({ host, label: host === "macos" ? "xcode-27" : "windows-2025", universe, selection: { ...empty, ...selection }, config, shards });

test("rustWeight counts the cargo commands a crate runs", () => {
  assert.equal(rustWeight(), 3); // one test run, clippy, the default all-features check
  assert.equal(rustWeight({ checkFeatureSets: [] }), 2);
  assert.equal(rustWeight({ featureSets: [["--all-features"], []], alsoSerial: true }), 6); // 4 tests + 2 clippy, no extra check
  assert.equal(rustWeight({ featureSets: [[], ["--features", "x"]], checkFeatureSets: [["--features", "a"], ["--features", "b"]] }), 6);
  assert.equal(rustWeight({ consumerRows: [{ manifestPath: "ci/consumers/host/Cargo.toml" }, { manifestPath: "ci/consumers/cross/Cargo.toml", targets: ["x86_64-pc-windows-msvc", "aarch64-apple-darwin"] }] }), 6);
});

test("groupUnits is deterministic, keeps heavy and solo units alone and never exceeds the target", () => {
  const units = [{ name: "c", weight: 3 }, { name: "a", weight: 3 }, { name: "b", weight: 3 }, { name: "big", weight: 12 }, { name: "x", weight: 3 }, { name: "y", weight: 3 }];
  const groups = groupUnits(units, 9, ["x"]);
  assert.deepEqual(groups.map((g) => g.members), [["big"], ["a", "b", "c"], ["x"], ["y"]]);
  assert.ok(groups.filter((g) => !["big", "x"].includes(g.anchor)).every((g) => g.weight <= 9));
  assert.deepEqual(groupUnits([...units].reverse(), 9, ["x"]), groups, "input order does not matter");
  assert.deepEqual(groupUnits([], 9), []);
  // every unit in exactly one group
  assert.deepEqual(groups.flatMap((g) => g.members).sort(), units.map((u) => u.name).sort());
});

test("a crate's feature rows stay together: each selected crate is in exactly one shard, whatever else is selected", () => {
  const every = plan({ rust: { all: true, test: crates.filter((c) => c !== "rightkit-win"), check: [] } });
  const seen = every.shards.flatMap((s) => s.tokens.split(" "));
  assert.equal(new Set(seen).size, seen.length);
  assert.deepEqual([...seen].sort(), crates.filter((c) => c !== "rightkit-win").map((c) => `rust:${c}`).sort());
  // The heavy crate weighs 9 (six test commands, three clippy): at target 12 it shares a group with a 3-point crate. With
  // a target of 8 nothing fits beside it. Either way a crate's group does not change with the selection.
  const solo = plan({ rust: { all: false, test: ["rightkit-heavy"], check: [] } }, "windows", { rustTargetWeight: 8 });
  assert.deepEqual(solo.shards.map((s) => [s.id, s.tokens]), [["rust-heavy", "rust:rightkit-heavy"]]);
  const one = plan({ rust: { all: false, test: ["rightkit-b"], check: [] } });
  const both = plan({ rust: { all: false, test: ["rightkit-b", "rightkit-c"], check: [] } });
  const idOfB = one.shards[0].id;
  assert.ok(both.shards.some((s) => s.id === idOfB && s.tokens.includes("rust:rightkit-b")), "same job id, so the same cache key, on every push");
  assert.equal(one.shards[0].cacheKey, `rk-windows-2025-${idOfB}`);
});

test("a crate restricted to another OS is never planned there", () => {
  const selection = { rust: { all: false, test: ["rightkit-a", "rightkit-win"], check: ["rightkit-win"] } };
  const mac = plan(selection, "macos");
  assert.ok(!JSON.stringify(mac).includes("rightkit-win"));
  const win = plan(selection, "windows");
  assert.ok(JSON.stringify(win.shards).includes("rust:rightkit-win"));
  assert.ok(win.closure.some((c) => c.tokens.includes("rust-check:rightkit-win")));
  assert.equal(appliesOn(config, "macos", "rightkit-win"), false);
  assert.equal(appliesOn(config, "macos", "rightkit-a"), true);
});

test("dependents become closure jobs: ceil(n / chunk size) of them, capped, every dependent exactly once", () => {
  const check = (n) => crates.slice(0, n).filter((c) => c !== "rightkit-win");
  assert.deepEqual(plan({ rust: { all: false, test: [], check: [] } }).closure, []);
  const two = plan({ rust: { all: false, test: [], check: check(2) } });
  assert.deepEqual(two.closure.map((c) => c.id), ["closure-1"]);
  assert.equal(two.closure[0].tokens, "rust-check:rightkit-a rust-check:rightkit-b");
  const many = plan({ rust: { all: false, test: [], check: check(6) } }, "windows", { closureChunkSize: 2, closureMaxJobs: 2 });
  assert.deepEqual(many.closure.map((c) => c.id), ["closure-1", "closure-2"]);
  const tokens = many.closure.flatMap((c) => c.tokens.split(" "));
  assert.deepEqual([...tokens].sort(), check(6).map((c) => `rust-check:${c}`).sort());
  assert.ok(many.closure.every((c) => c.rust && c.cacheKey.startsWith("rk-windows-2025-closure-")));
});

test("npm packages are grouped statically; root suites and the Swift manifest share one job; Swift packages are macOS jobs", () => {
  const picked = { npm: { all: false, test: ["packages/p1", "packages/p4"] }, root: ["test:characterization", "scripts"], swift: ["swift/RightKitSwift", SWIFT_ROOT_MANIFEST] };
  const mac = plan(picked, "macos", { npmTargetWeight: 2 });
  assert.deepEqual(mac.shards.map((s) => s.id), ["npm-p1", "npm-p3", "root", "swift-rightkitswift"]);
  const byId = Object.fromEntries(mac.shards.map((s) => [s.id, s]));
  assert.equal(byId["npm-p1"].tokens, "npm:packages/p1");
  assert.equal(byId["npm-p3"].tokens, "npm:packages/p4"); // p3 anchors the second group, p4 is its selected member
  assert.equal(byId.root.tokens, `root:test:characterization root:scripts swift:${SWIFT_ROOT_MANIFEST}`);
  assert.equal(byId["swift-rightkitswift"].swift, true);
  assert.equal(byId.root.rust, false);
  // Windows has no Swift selection, so no Swift job and no manifest token.
  const win = plan({ ...picked, swift: [] }, "windows", { npmTargetWeight: 2 });
  assert.equal(win.shards.find((s) => s.id === "root").tokens, "root:test:characterization root:scripts");
  assert.ok(!win.shards.some((s) => s.kind === "swift"));
});

test("an empty selection plans no jobs; ids are safe in an artifact name", () => {
  assert.deepEqual(plan({}), { shards: [], closure: [] });
  const all = plan({ rust: { all: true, test: crates, check: [] }, npm: { all: true, test: universe.npm }, root: ["scripts"] });
  for (const job of [...all.shards, ...all.closure]) assert.match(job.id, /^[a-z0-9-]+$/);
  assert.equal(sanitizeId("@Rightkit/Qa Chrome"), "rightkit-qa-chrome");
  assert.equal(sanitizeId("///"), "x");
});

test("planner rejects selected units absent from universe rather than silently dropping them", () => {
  assert.throws(() => plan({ rust: { test: ["rightkit-missing"], check: [] } }), /cover every selected unit exactly once/);
  assert.throws(() => plan({ npm: { test: ["packages/missing"] } }), /cover every selected unit exactly once/);
});

test("hygiene covers the union of both OSes' crates and packages", () => {
  const a = { rust: { test: ["rightkit-b", "rightkit-a"] }, npm: { test: ["packages/p2"] } };
  const b = { rust: { test: ["rightkit-a"], check: ["rightkit-c"] }, npm: { test: [] } };
  assert.deepEqual(planHygiene([a, b]), { crates: ["rightkit-a", "rightkit-b", "rightkit-c"], packages: ["packages/p2"], tokens: "rust:rightkit-a rust:rightkit-b rust:rightkit-c" });
  assert.deepEqual(planHygiene([empty]), { crates: [], packages: [], tokens: "" });
});

test("shard settings are validated", () => {
  assert.equal(shardsConfig({}).rustTargetWeight, 12);
  assert.equal(shardsConfig({ closureMaxJobs: 2 }).closureMaxJobs, 2);
  assert.throws(() => shardsConfig({ rustTargetWeight: 0 }), /rustTargetWeight/);
  assert.throws(() => shardsConfig({ closureChunkSize: "x" }), /closureChunkSize/);
  assert.throws(() => shardsConfig({ solo: "rightkit-a" }), /solo/);
});

const known = { rust: ["rightkit-a", "rightkit-b"], npm: ["packages/p1"], root: ["test:characterization", "scripts"], swift: ["swift/RightKitSwift", SWIFT_ROOT_MANIFEST] };

test("--select tokens select exactly the named units and reject everything else", () => {
  const picked = parseSelectTokens("rust:rightkit-a rust-check:rightkit-b npm:packages/p1 root:scripts swift:swift/RightKitSwift rust:rightkit-a", known);
  assert.deepEqual(picked, { rust: { all: false, test: ["rightkit-a"], check: ["rightkit-b"] }, npm: { all: false, test: ["packages/p1"] }, root: ["scripts"], swift: ["swift/RightKitSwift"] });
  assert.deepEqual(parseSelectTokens("", known), { rust: { all: false, test: [], check: [] }, npm: { all: false, test: [] }, root: [], swift: [] });
  assert.throws(() => parseSelectTokens("rust:rightkit-zzz", known), /not a known rust unit/);
  assert.throws(() => parseSelectTokens("rust-check:packages/p1", known), /not a known rust unit/);
  assert.throws(() => parseSelectTokens("cargo:rightkit-a", known), /unknown selection token/);
  assert.throws(() => parseSelectTokens("rightkit-a", known), /unknown selection token/);
  assert.throws(() => parseSelectTokens("rust:", known), /unknown selection token/);
  assert.throws(() => parseSelectTokens("rust:rightkit-a rust-check:rightkit-a", known), /both as rust: and rust-check:/);
  assert.throws(() => parseSelectTokens("swift:swift/nope", known), /not a known swift unit/);
});

test("--steps names what runs; the default is everything", () => {
  assert.deepEqual([...parseSteps(undefined)].sort(), ["closure", "fmt", "lock", "test"]);
  assert.deepEqual([...parseSteps("lock,fmt")], ["lock", "fmt"]);
  assert.deepEqual([...parseSteps("test closure")], ["test", "closure"]);
  assert.throws(() => parseSteps("lock,build"), /unknown step/);
  assert.throws(() => parseSteps(" , "), /unknown step/);
});

test("summary requires every prerequisite & permits only explicitly empty matrix skips", () => {
  const counts = { "shards-windows": 2, "closure-windows": 0 };
  const options = { required: ["plan", "hygiene"], counts };
  const green = { plan: { result: "success" }, hygiene: { result: "success" }, "shards-windows": { result: "success" }, "closure-windows": { result: "skipped" } };
  assert.equal(summaryVerdict(green, options).ok, true);
  for (const result of ["failure", "cancelled", "skipped", undefined]) assert.equal(summaryVerdict({ ...green, "shards-windows": { result } }, options).ok, false);
  assert.equal(summaryVerdict({ ...green, hygiene: { result: "skipped" } }, options).ok, false);
  assert.equal(summaryVerdict({ plan: { result: "success" } }, options).ok, false);
  assert.equal(summaryVerdict(green, { ...options, counts: { ...counts, "closure-windows": 1 } }).ok, false);
  assert.equal(summaryVerdict(green, { ...options, counts: { ...counts, "shards-windows": NaN } }).ok, false);
});

test("required evidence freezes every Rust feature, serial, clippy & check command", () => {
  const commands = requiredShardCommands("rust:rightkit-a", { config: { rust: { "rightkit-a": { featureSets: [[], ["--features", "native"]], alsoSerial: true, checkFeatureSets: [["--all-features"], ["--no-default-features"]] } } } });
  assert.deepEqual(commands.map((row) => row.command), [
    "cargo test -p rightkit-a --no-fail-fast -- --show-output",
    "cargo test -p rightkit-a --no-fail-fast -- --test-threads=1 --show-output",
    "cargo test -p rightkit-a --features native --no-fail-fast -- --show-output",
    "cargo test -p rightkit-a --features native --no-fail-fast -- --test-threads=1 --show-output",
    "cargo clippy -p rightkit-a --no-deps --all-targets -- -D warnings",
    "cargo clippy -p rightkit-a --no-deps --all-targets --features native -- -D warnings",
    "cargo check -p rightkit-a --all-targets --all-features",
    "cargo check -p rightkit-a --all-targets --no-default-features",
  ]);
  assert.equal(requiredShardCommands("rust:rightkit-a", { config: { rust: { "rightkit-a": { featureSets: [["--all-features"]] } } } }).length, 2);
  assert.deepEqual(requiredShardCommands("rust-check:rightkit-a").map((row) => row.command), ["cargo check -p rightkit-a --all-targets", "cargo check -p rightkit-a --all-targets --all-features"]);
});

test("required evidence includes npm build prerequisites, root & Swift command coverage", () => {
  const packages = [{ dir: "packages/p1", name: "@rightkit/p1", scripts: { test: "node --test" } }, { dir: "packages/p2", name: "@rightkit/p2", scripts: {} }];
  assert.deepEqual(requiredShardCommands("npm:packages/p1 npm:packages/p2", { packages }), [
    { unit: "npm:build", command: "pnpm --workspace-concurrency=1 --if-present --filter @rightkit/p1... --filter @rightkit/p2... run build" },
    { unit: "npm:packages/p1", command: "pnpm --dir packages/p1 run test" },
  ]);
  assert.deepEqual(requiredShardCommands("root:scripts root:test:characterization swift:swift/root-manifest swift:swift/RightKitSwift", { swiftTests: ["swift/RightKitSwift"] }).map((row) => row.command), ["node --test scripts/**/*.test.mjs", "pnpm run test:characterization", "swift package describe --type json", "swift build", "swift test"]);
});

test("consumer rows freeze each target after per-set lint & feature checks; missing or substituted commands fail evidence", () => {
  const entry = {
    featureSets: [[], ["--features", "native"]],
    consumerRows: [
      { manifestPath: "ci/consumers/host/Cargo.toml", args: ["--no-default-features", "--features", "native"] },
      { manifestPath: "ci/consumers/cross/Cargo.toml", kind: "check", targets: ["x86_64-pc-windows-msvc", "aarch64-apple-darwin"], args: ["--all-features"], locked: true },
    ],
  };
  const options = { config: { rust: { "rightkit-a": entry } } };
  const commands = requiredShardCommands("rust:rightkit-a", options);
  assert.deepEqual(commands.slice(-3).map((row) => row.command), [
    "cargo check --manifest-path ci/consumers/host/Cargo.toml --locked --no-default-features --features native",
    "cargo check --manifest-path ci/consumers/cross/Cargo.toml --locked --target x86_64-pc-windows-msvc --all-features",
    "cargo check --manifest-path ci/consumers/cross/Cargo.toml --locked --target aarch64-apple-darwin --all-features",
  ]);
  assert.equal(commands.at(-4).command, "cargo check -p rightkit-a --all-targets --all-features");
  assert.ok(commands.every((row) => row.unit === "rust:rightkit-a"));
  assert.equal(commands.length, rustWeight(entry));
  const closure = requiredShardCommands("rust-check:rightkit-a", options);
  assert.deepEqual(closure.slice(-3), commands.slice(-3), "closure jobs require identical consumer checks");
  assert.ok(closure.every((row) => row.command.startsWith("cargo check ")));
  const job = { artifact: "qa-evidence-consumers", host: "windows", tokens: "rust:rightkit-a", steps: "test", commands };
  const reportFor = (actual) => ({
    sha: "abc", host: job.host, selection: job.tokens, steps: job.steps,
    results: actual.map(({ unit, command }) => ({ unit, command, ok: true, code: 0, timedOut: false, seconds: 1, isTest: command.startsWith("cargo test "), executed: command.startsWith("cargo test ") ? 1 : 0, failed: 0, ignored: 0, skipped: 0, binaries: command.startsWith("cargo test ") ? 1 : 0 })),
    rows: [{ id: "rust:rightkit-a", ok: true, seconds: actual.length, skipped: 0, failed: [], known: false }],
  });
  const verdict = (actual) => evidenceVerdict([job], { [job.artifact]: reportFor(actual) }, "abc");
  assert.equal(verdict(commands).ok, true);
  for (const omitted of commands.filter((row) => row.command.includes("--manifest-path") || row.command.includes("clippy"))) {
    const failed = verdict(commands.filter((row) => row !== omitted));
    assert.equal(failed.ok, false, omitted.command);
    assert.ok(failed.failures.some((failure) => failure.includes("command coverage differs from plan")));
  }
  assert.equal(verdict(commands.map((row) => row.command.includes("--manifest-path") ? { ...row, command: row.command.replace("--locked", "--help") } : row)).ok, false);
});

test("gate consumer validation refuses no-op flags & unselected consumer paths without executing gate effects", () => {
  const source = readFileSync(new URL("./gate.mjs", import.meta.url), "utf8");
  const start = source.indexOf("const CONSUMER_ROW_KEYS =");
  const end = source.indexOf("/** One Cargo step per target", start);
  assert.ok(start >= 0 && end > start);
  const validate = vm.runInNewContext(`${source.slice(start, end)}\nconsumerRows;`, { path }, { timeout: 1000 });
  const row = { manifestPath: "ci/consumers/native/Cargo.toml", args: ["--features", "native"] };
  const entry = { paths: ["ci/consumers/native"], consumerRows: [row] };
  assert.equal(validate("rightkit-a", entry).length, 1);
  assert.equal(validate("rightkit-a", { ...entry, paths: ["ci/consumers"] }).length, 1);
  for (const paths of [undefined, [], ["ci/consumers/other"], ["ci/consumers/native-suffix"]]) assert.throws(() => validate("rightkit-a", { ...entry, paths }), /consumer directory .* must be listed/);
  for (const args of [["--help"], ["-h"], ["--target-dir=other"], ["--features", "--help"], ["--features=--help"]]) assert.throws(() => validate("rightkit-a", { ...entry, consumerRows: [{ ...row, args }] }), /args may not contain|needs a feature list/);
});

test("realistic crate evidence requires exact successful commands & consistent aggregate rows", () => {
  const commands = requiredShardCommands("rust:rightkit-a", { config: { rust: { "rightkit-a": { alsoSerial: true } } } });
  const job = { artifact: "qa-evidence-windows-2025-rust-a", host: "windows", tokens: "rust:rightkit-a", steps: "test", commands };
  const results = commands.map(({ unit, command }) => ({ unit, command, ok: true, code: 0, timedOut: false, seconds: 2, isTest: command.startsWith("cargo test "), executed: command.startsWith("cargo test ") ? 17 : 0, failed: 0, ignored: 0, skipped: 0, binaries: command.startsWith("cargo test ") ? 2 : 0 }));
  const report = { sha: "abc", host: job.host, selection: job.tokens, steps: job.steps, rows: [{ id: "rust:rightkit-a", ok: true, seconds: results.length * 2, skipped: 0, failed: [], known: false }], results };
  const verdict = (value) => evidenceVerdict([job], { [job.artifact]: value }, "abc");
  assert.equal(verdict(report).ok, true);
  assert.equal(evidenceVerdict([job], {}, "abc").ok, false);
  assert.equal(evidenceVerdict(null, {}, "abc").ok, false);
  assert.equal(evidenceVerdict([job, job], { [job.artifact]: report }, "abc").ok, false);
  for (const key of ["sha", "host", "selection", "steps"]) assert.equal(verdict({ ...report, [key]: "wrong" }).ok, false);
  for (const mutation of [
    { rows: [], results: [] }, { rows: [] }, { results: [] },
    { results: results.slice(1) },
    { results: [...results, results[0]] },
    { results: results.map((row, index) => index ? row : { ...row, command: "cargo test -p wrong" }) },
    { rows: [{ ...report.rows[0], ok: false, failed: ["failed step"] }] },
    { rows: [{ ...report.rows[0], seconds: 0 }] },
    { rows: [{ ...report.rows[0], id: "rust:rightkit-other" }] },
    { rows: [report.rows[0], report.rows[0]] },
  ]) assert.equal(verdict({ ...report, ...mutation }).ok, false);
  for (const mutation of [{ ok: false }, { code: 1 }, { timedOut: true }, { failed: 1 }, { executed: undefined }, { isTest: undefined }, { seconds: undefined }, { binaries: -1 }, { skipped: "0" }]) {
    assert.equal(verdict({ ...report, results: results.map((row, index) => index ? row : { ...row, ...mutation }) }).ok, false);
  }
  assert.equal(evidenceVerdict([{ ...job, commands: [] }], { [job.artifact]: report }, "abc").ok, false);
});

test("secret scan targets are repository-relative and refuse anything that could leave the repository", () => {
  assert.deepEqual(scanTargets(["rightkit-fs", "rightkit-fs"], ["packages/qa"]), ["crates/rightkit-fs", "packages/qa"]);
  assert.throws(() => scanTargets(["../etc"], []), /refusing crate name/);
  assert.throws(() => scanTargets([], ["packages/../x"]), /refusing package directory/);
  assert.throws(() => scanTargets([], ["/abs"]), /refusing package directory/);
});


test("consumer lock hygiene checks each manifest once across feature rows & owners", () => {
  const manifestPath = "ci/consumers/native/Cargo.toml";
  assert.deepEqual(requiredConsumerLockCommands([{ manifestPath }, { manifestPath, args: ["--features", "engine"] }]), [{
    unit: "lock", command: `cargo metadata --manifest-path ${manifestPath} --locked --format-version 1`,
    args: ["metadata", "--manifest-path", manifestPath, "--locked", "--format-version", "1"],
  }]);
  assert.deepEqual(requiredConsumerLockCommands(), []);
});

test("closure-only consumers require tracked manifest & lock before any gate effect", () => {
  const source = readFileSync(new URL("./gate.mjs", import.meta.url), "utf8");
  const start = source.indexOf("const CONSUMER_ROW_KEYS =");
  const end = source.indexOf("async function rustGate", start);
  const manifestPath = "ci/consumers/native/Cargo.toml";
  const selection = { config: { rust: { dependent: { paths: ["ci/consumers/native"], consumerRows: [{ manifestPath }] } } }, rust: { test: [], check: ["dependent"] } };
  const files = [];
  let missing;
  let untracked;
  const refusal = [];
  const validate = vm.runInNewContext(`${source.slice(start, end)}\nvalidatedConsumerRows;`, {
    path, ROOT: path.resolve("fixture"), HOST: "windows",
    fs: { realpathSync: (file) => file, existsSync: (file) => !file.endsWith(missing ?? "NEVER") },
    git: (...args) => { files.push(args.at(-1)); return { status: args.at(-1) === untracked ? 1 : 0 }; },
    console: { error: (message) => refusal.push(message) },
    process: { exit: (code) => { throw new Error(`exit ${code}`); } },
  });
  assert.equal(validate(selection).get("dependent").length, 1);
  assert.deepEqual(files, [manifestPath, manifestPath.replace("Cargo.toml", "Cargo.lock")]);
  missing = "Cargo.lock";
  assert.throws(() => validate(selection), /exit 2/);
  assert.match(refusal.at(-1), /consumer row needs .*Cargo.lock/);
  missing = undefined;
  untracked = manifestPath;
  assert.throws(() => validate(selection), /exit 2/);
  assert.match(refusal.at(-1), /is not tracked by git/);
});

test("consumer lock repair emits patch while retaining failed locked proof", async () => {
  const source = readFileSync(new URL("./gate.mjs", import.meta.url), "utf8");
  const start = source.indexOf("async function rustGate");
  const end = source.indexOf("async function npmGate", start);
  const manifestPath = "ci/consumers/native/Cargo.toml";
  const actual = [];
  const patches = [];
  const gate = vm.runInNewContext(`${source.slice(start, end)}\nrustGate;`, {
    path, HOST: "windows", STEPS: new Set(["lock"]), requiredConsumerLockCommands,
    run: async (unit, command, args) => { const ok = !args.includes("--manifest-path") || !args.includes("--locked"); actual.push({ unit, command, args: [...args], ok }); return ok; },
    savePatch: (name, files) => patches.push({ name, files: [...files] }),
  });
  await gate({ rust: { test: [], check: ["dependent"] } }, {}, new Map([["dependent", [{ manifestPath }, { manifestPath, args: ["--features", "engine"] }]]]));
  assert.equal(actual.filter((step) => step.args[0] === "metadata" && step.args.includes(manifestPath)).length, 1);
  assert.equal(actual.find((step) => step.args.includes(manifestPath) && step.args.includes("--locked")).ok, false);
  assert.deepEqual(actual.at(-1).args, ["update", "--manifest-path", manifestPath, "--workspace"]);
  assert.deepEqual(patches, [{ name: "ci-consumers-native-Cargo.lock.patch", files: ["ci/consumers/native/Cargo.lock"] }]);
});
