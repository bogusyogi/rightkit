#!/usr/bin/env node
// Prolonged simulation 2 of 2: the agent hook's one-door policy against what a shell really runs.
//
//   node scripts/sim/door-policy.mjs [--seed <n>] [--reps <n>] [--out <dir>]
//
// Each trial composes a core command (one that builds outside the door, or one that must stay allowed) with a seeded shell mechanism
// (wrappers, chains, subshells, nested shells, substitutions, indirection, ...). The hook's verdict (doorViolation, from the command
// text alone) is compared with an independent oracle: the command is executed by bash with PATH shims for cargo, tauri, right-release,
// pnpm, npx, yarn, node, sudo, rustright and rightkit-gate that log the argv bash actually handed them, and the oracle judges that
// argv. A built command the hook allowed is a false allow; an allowed one it denied is a false deny. Neither is tolerated, except the
// mechanisms listed in PROCESS_BOUNDARY, which no text policy can see (the command that builds is not in the text); they are counted
// and reported separately as the process boundary's share, never as green.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "../../packages/release/background-process.mjs";
import { doorViolation } from "../../packages/release/door-policy.mjs";

const sq = (s) => `'${s.replace(/'/g, `'\\''`)}'`;
const dq = (s) => `"${s.replace(/(["\\$`])/g, "\\$1")}"`;

/** Cores: `build` ones build outside the door; the others must stay allowed. `tool`/`rest` split the first word for tool-level mechanisms. */
export const CORES = [
  ["cargo", "build --release", true], ["cargo", "test -p a", true], ["cargo", "+stable clippy --all-targets", true], ["cargo", "-C crates/x build", true],
  ["cargo", "--config net.offline=true test", true], ["cargo", "b", true], ["cargo", "--frozen check", true], ["cargo", "-q run", true],
  ["tauri", "build", true], ["cargo", "tauri build", true], ["right-release", "build --platform win", true],
  ["right-release", "upload --release r --platform win --tier patch", true], ["right-release", "qa --stage installed", true], ["right-release", "--platform mac", true],
  ["node", "node_modules/@rightkit/release/cli/right-release.mjs build", true], ["pnpm", "exec tauri build", true], ["npx", "tauri dev", true],
  ["pnpm", "--filter app exec tauri build", true], ["yarn", "tauri bundle", true],
  ["rightkit-gate", "--to compile", false], ["rustright", "cargo build", false], ["cargo", "fmt --all -- --check", false], ["cargo", "metadata --no-deps", false],
  ["cargo", "tree", false], ["cargo", "--version", false], ["right-release", "check", false], ["echo", "\"cargo build\"", false], ["printf", "'%s\\n' 'tauri build'", false],
  ["grep", "-c 'cargo test' /dev/null", false], ["pnpm", "install --frozen-lockfile", false], ["pnpm", "run lint", false], ["cargo", "update -p serde", false],
].map(([tool, rest, build]) => ({ tool, rest, build, text: `${tool} ${rest}` }));

/** Shell mechanisms: each wraps a core's text (c) or its tool and rest (t, r). `toolLevel` ones apply only to a single-word tool. */
export const MECHANISMS = {
  plain: (c) => c.text,
  assign: (c) => `FOO=1 ${c.text}`,
  env: (c) => `env FOO=1 ${c.text}`,
  timeout: (c) => `timeout 60 ${c.text}`,
  nice: (c) => `nice -n 5 ${c.text}`,
  nohup: (c) => `nohup ${c.text} >/dev/null`,
  time: (c) => `time ${c.text}`,
  command: (c) => `command ${c.text}`,
  exec: (c) => `exec ${c.text}`,
  sudo: (c) => `sudo ${c.text}`,
  "and-chain": (c) => `true && ${c.text}`,
  "or-chain": (c) => `false || ${c.text}`,
  "pipe-into": (c) => `true | ${c.text}`,
  subshell: (c) => `(${c.text})`,
  group: (c) => `{ ${c.text}; }`,
  background: (c) => `${c.text} & wait`,
  newline: (c) => `cd .\n${c.text}`,
  "bash-c": (c) => `bash -c ${sq(c.text)}`,
  "sh-c": (c) => `sh -c ${dq(c.text)}`,
  eval: (c) => `eval ${sq(c.text)}`,
  "nested-shells": (c) => `bash -c ${sq(`sh -c ${sq(c.text)}`)}`,
  "command-subst": (c) => `echo "$(${c.text})"`,
  backticks: (c) => `echo \`${c.text}\``,
  "if-then": (c) => `if true; then ${c.text}; fi`,
  "for-do": (c) => `for x in 1; do ${c.text}; done`,
  "while-do": (c) => `while true; do ${c.text}; break; done`,
  "case-arm": (c) => `case x in x) ${c.text};; esac`,
  "function-call": (c) => `f() { ${c.text}; }; f`,
  "abs-path": (c, shims) => `${path.join(shims, c.tool)} ${c.rest}`,
  "quoted-tool": (c) => `"${c.tool}" ${c.rest}`,
  "escaped-tool": (c) => `${c.tool[0]}\\${c.tool.slice(1)} ${c.rest}`,
  "var-indirection": (c) => `T=${c.tool}; $T ${c.rest}`,
  "braced-var": (c) => `T=${c.tool}; \${T} ${c.rest}`,
  "xargs-stdin": (c) => `echo ${c.rest} | xargs ${c.tool}`,
  "heredoc-shell": (c) => `bash <<'EOF'\n${c.text}\nEOF`,
  "script-file": (c) => `printf '%s\\n' ${sq(c.text)} > run.sh && bash run.sh`,
  "source-procsubst": (c) => `source <(printf '%s\\n' ${sq(c.text)})`,
};
const TOOL_LEVEL = new Set(["abs-path", "quoted-tool", "escaped-tool", "var-indirection", "braced-var", "xargs-stdin"]);
/** The command that builds is not in the text the hook sees: only a process boundary (the broker, a sandbox) can hold these. */
export const PROCESS_BOUNDARY = new Set(["script-file", "source-procsubst"]);

/** The oracle: did any logged argv build outside the door? Its own reading of each tool's grammar, not the policy's parser. */
export function oracleBuilt(records) {
  const compiles = new Set(["build", "b", "check", "c", "clippy", "test", "t", "run", "r", "bench", "doc", "d", "rustc", "install"]);
  const tauri = new Set(["build", "dev", "bundle", "android", "ios"]);
  const release = new Set(["dev", "test", "stage-sidecar", "release", "build", "package", "qa", "upload", "publish", "publish-update", "github", "sign-windows", "sign-updater", "create-mac-updater", "web-build", "swift-package", "recipe-build", "sign-candidate", "cargo-target"]);
  for (const [tool, ...a] of records) {
    if (tool === "cargo") {
      let i = 0;
      for (; i < a.length; i++) { if (["-C", "--config", "--color", "-Z", "--manifest-path"].includes(a[i])) { i++; continue; } if (!a[i].startsWith("-") && !a[i].startsWith("+")) break; }
      if (compiles.has(a[i]) || (a[i] === "tauri" && tauri.has(a[i + 1]))) return true;
    }
    if (tool === "tauri" && tauri.has(a[0])) return true;
    if (tool === "right-release" && (release.has(a[0]) || (a[0]?.startsWith("-") && !["-h", "--help", "-v", "--version"].includes(a[0])) || (["dmg", "addon"].includes(a[0]) && a[1] === "build"))) return true;
  }
  return false;
}

function writeShims(dir, realNode) {
  const log = `printf '%s' "$(basename "$0")" >> "$RK_SIM_LOG"; for a in "$@"; do printf '\\037%s' "$a" >> "$RK_SIM_LOG"; done; printf '\\n' >> "$RK_SIM_LOG"`;
  const runner = `#!/bin/bash
out=(); skip=0
for a in "$@"; do if [ $skip = 1 ]; then skip=0; continue; fi; case "$a" in --filter|-F|--dir|-C|--prefix) skip=1;; -*) ;; *) out+=("$a");; esac; done
case "\${out[0]}" in exec|dlx|x) out=("\${out[@]:1}");; run|install|i|add|run-script) ${log}; exit 0;; esac
[ \${#out[@]} -eq 0 ] && exit 0
exec "\${out[@]}"
`;
  const files = {
    cargo: `#!/bin/bash\n${log}\n`, tauri: `#!/bin/bash\n${log}\n`, "right-release": `#!/bin/bash\n${log}\n`,
    rustright: `#!/bin/bash\n${log}\n`, "rightkit-gate": `#!/bin/bash\n${log}\n`,
    pnpm: runner, npx: runner, yarn: runner, sudo: `#!/bin/bash\nexec "$@"\n`,
    node: `#!/bin/bash\ncase "$1" in *right-release|*right-release.mjs) shift; exec right-release "$@";; *build-release.mjs) shift; exec right-release build "$@";; esac\nexec ${sq(realNode)} "$@"\n`,
  };
  fs.mkdirSync(dir, { recursive: true });
  for (const [name, body] of Object.entries(files)) { fs.writeFileSync(path.join(dir, name), body); fs.chmodSync(path.join(dir, name), 0o755); }
}

export function rng(seed) {
  let a = seed >>> 0;
  return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
const pick = (r, xs) => xs[Math.floor(r() * xs.length)];

export function trial({ core, mechanism, outer = null, shims, work, judge = doorViolation }) {
  const inner = MECHANISMS[mechanism](core, shims);
  // An optional second, outer mechanism wraps the first one's whole command (never a tool-level one: it needs a bare tool).
  const command = outer ? MECHANISMS[outer]({ text: inner }, shims) : inner;
  const log = path.join(work, "argv.log");
  fs.writeFileSync(log, "");
  const ran = spawnSync("bash", ["-c", command], { cwd: work, encoding: "utf8", timeout: 15000, env: { PATH: `${shims}:/usr/local/bin:/usr/bin:/bin`, HOME: work, RK_SIM_LOG: log } });
  const records = fs.readFileSync(log, "utf8").split("\n").filter(Boolean).map((l) => l.split("\x1f"));
  const built = oracleBuilt(records);
  const denied = Boolean(judge(command));
  const verdict = built === denied ? "agree" : built ? "false-allow" : "false-deny";
  // Valid: the shell really ran the core (a shim logged it, or a builtin core printed its output). A composition bash cannot run as
  // written (`nohup FOO=1 cargo ...` runs a program named FOO=1) is reported as invalid and never counted, green or red.
  const printed = { echo: "cargo build", printf: "tauri build", grep: "0" }[core.tool];
  const valid = printed ? String(ran.stdout).includes(printed) : records.length > 0;
  return { command, built, denied, verdict, records, status: ran.status, valid };
}

const MUTANTS = { "allow-all": () => null, "deny-all": () => "mutant" };

async function cli(argv) {
  const opt = (n, d) => { const i = argv.indexOf(n); return i < 0 ? d : argv[i + 1]; };
  const seed = Number(opt("--seed", "1")); const per = Number(opt("--per", "100")); const mutant = opt("--mutant", null);
  const here = path.dirname(fileURLToPath(import.meta.url)); const repo = path.resolve(here, "..", "..");
  const identity = Object.fromEntries([path.join(repo, "packages", "release", "door-policy.mjs"), fileURLToPath(import.meta.url)].map((f) => [path.relative(repo, f), crypto.createHash("sha256").update(fs.readFileSync(f)).digest("hex")]));
  const codeId = crypto.createHash("sha256").update(JSON.stringify(identity)).digest("hex").slice(0, 12);
  const out = path.resolve(opt("--out", path.join(os.tmpdir(), "rk-sim")), `door-policy-seed${seed}-per${per}-${codeId}${mutant ? `-mutant-${mutant}` : ""}`);
  if (fs.existsSync(out)) { process.stderr.write(`door-policy: ${out} exists; a run is never overwritten\n`); return 2; }
  fs.mkdirSync(out, { recursive: true });
  const judge = mutant ? MUTANTS[mutant] : doorViolation;
  const work = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sim-policy-")));
  const shims = path.join(work, "shims");
  writeShims(shims, process.execPath);
  const r = rng(seed);
  const names = Object.keys(MECHANISMS);
  const outers = names.filter((m) => !TOOL_LEVEL.has(m) && m !== "plain");
  // `per` seeded trials of every mechanism: a random core (build or allowed) and, half the time, a random outer mechanism.
  const plan = [];
  for (const m of names) for (let i = 0; i < per; i++) {
    const pool = TOOL_LEVEL.has(m) ? CORES.filter((c) => !c.tool.includes(" ")) : CORES;
    plan.push({ m, c: pick(r, pool), outer: r() < 0.5 ? pick(r, outers) : null });
  }
  const tally = {}; const firstFailures = {}; const oracleBlind = [];
  const results = fs.openSync(path.join(out, "results.jsonl"), "w");
  try {
    for (const { m, c, outer } of plan) {
      const res = trial({ core: c, mechanism: m, outer, shims, work, judge });
      fs.writeSync(results, `${JSON.stringify({ mechanism: m, outer, core: c.text, build: c.build, command: res.command, valid: res.valid, oracleBuilt: res.built, denied: res.denied, verdict: res.verdict })}\n`);
      // Process boundary when either layer hides the command from the text.
      const key = PROCESS_BOUNDARY.has(outer) && !PROCESS_BOUNDARY.has(m) ? `${outer} (outer)` : m;
      const t = (tally[key] ??= { trials: 0, invalid: 0, buildTrials: 0, allowedTrials: 0, falseAllow: 0, falseDeny: 0, distinctBuildCommands: new Set(), distinctAllowedCommands: new Set() });
      if (!res.valid) { t.invalid++; continue; }
      // On a valid trial the oracle normally sees what the core is. A difference is listed for review: either the composition
      // changed what runs (`nohup T=rustright; $T cargo build` runs cargo itself) or the oracle is wrong. The verdict stays the oracle's.
      if (c.build !== res.built) oracleBlind.push({ mechanism: m, outer, command: res.command, expectedBuild: c.build, oracle: res.built, status: res.status });
      t.trials++; if (c.build) { t.buildTrials++; t.distinctBuildCommands.add(res.command); } else { t.allowedTrials++; t.distinctAllowedCommands.add(res.command); }
      if (res.verdict === "false-allow") t.falseAllow++;
      if (res.verdict === "false-deny") t.falseDeny++;
      if (res.verdict !== "agree") firstFailures[key] ??= { seed, outer, ...res };
    }
  } finally { fs.closeSync(results); fs.rmSync(work, { recursive: true, force: true }); }
  for (const t of Object.values(tally)) { t.distinctBuildCommands = t.distinctBuildCommands.size; t.distinctAllowedCommands = t.distinctAllowedCommands.size; }
  const isBoundary = (m) => PROCESS_BOUNDARY.has(m) || m.endsWith(" (outer)");
  const negMechs = Object.keys(tally).filter((m) => m !== "plain");
  const red = Object.entries(tally).filter(([m, t]) => (t.falseAllow || t.falseDeny) && !isBoundary(m)).map(([m]) => m);
  const boundary = Object.entries(tally).filter(([m, t]) => isBoundary(m) && (t.falseAllow || t.falseDeny)).map(([m]) => m);
  const sum = (f) => negMechs.reduce((n, m) => n + f(tally[m]), 0);
  const summary = {
    suite: "door-policy", seed, per, mutant, identity, codeId,
    positives: { plainBuildDenied: tally.plain.buildTrials, allowedCommands: Object.values(tally).reduce((n, t) => n + t.allowedTrials, 0) },
    distinctNegativeMechanisms: { defined: names.length - 1, textVisible: names.filter((m) => m !== "plain" && !PROCESS_BOUNDARY.has(m)).length, processBoundary: PROCESS_BOUNDARY.size },
    negativeTrials: { executed: sum((t) => t.buildTrials), distinctCommands: sum((t) => t.distinctBuildCommands), textVisibleExecuted: negMechs.filter((m) => !isBoundary(m)).reduce((n, m) => n + tally[m].buildTrials, 0), textVisibleDistinct: negMechs.filter((m) => !isBoundary(m)).reduce((n, m) => n + tally[m].distinctBuildCommands, 0), invalidExcluded: Object.values(tally).reduce((n, t) => n + t.invalid, 0) },
    tally, red, processBoundary: boundary, oracleBlind,
  };
  fs.writeFileSync(path.join(out, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
  fs.writeFileSync(path.join(out, "first-failures.json"), `${JSON.stringify(firstFailures, null, 2)}\n`);
  for (const [m, t] of Object.entries(tally)) process.stdout.write(`${t.falseAllow || t.falseDeny ? (isBoundary(m) ? "BOUNDARY" : "FAIL") : "pass"} ${m}: ${t.trials} trials (${t.invalid} invalid; ${t.buildTrials} build / ${t.distinctBuildCommands} distinct, ${t.allowedTrials} allowed / ${t.distinctAllowedCommands} distinct), ${t.falseAllow} false allow, ${t.falseDeny} false deny\n`);
  const n = summary.negativeTrials;
  process.stdout.write(`door-policy ${codeId}${mutant ? ` MUTANT ${mutant}` : ""}: seed ${seed}, ${per} per mechanism; negative mechanisms ${summary.distinctNegativeMechanisms.defined} (${summary.distinctNegativeMechanisms.textVisible} text-visible, ${PROCESS_BOUNDARY.size} process-boundary); negative trials ${n.executed} executed (${n.distinctCommands} distinct; text-visible ${n.textVisibleExecuted} / ${n.textVisibleDistinct} distinct; ${n.invalidExcluded} invalid compositions excluded); positives ${summary.positives.plainBuildDenied} plain-build + ${summary.positives.allowedCommands} allowed; expectation differs from oracle ${oracleBlind.length} (listed for review); ${red.length ? `RED: ${red.join(", ")}` : "green"}${boundary.length ? `; process boundary only: ${boundary.join(", ")}` : ""}; ${out}\n`);
  return red.length ? 1 : 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await cli(process.argv.slice(2));
