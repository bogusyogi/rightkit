// The Cargo calls of one run (design section 7, docs/design/2026-11-10-gate-fresh-design.md). Pure: selection + config in, call list out.
//
// Crates build together (Adrian, 2026-11-04: "RK has to build all it's necessary crates together. today, the agent built each one
// separately."). A call names every selected crate it applies to; there are no per-crate rows, no batching step and no feature
// partition. A crate-specific feature set is the one thing that is a call of its own, because `--features` names one crate's features.
//
// Input crate: { name, platforms?: ["darwin"|"win32"|"linux"], hasLib: boolean, declaresFeatures: boolean, config?: <ci/gate.config.json rust.<name>> }
// Output call: { id, stage: "lint"|"test-build"|"docs", round, crates: [{ name, targets }], args: [...], cwd?: <dir for a consumer manifest> }
import { featureSetEntries, unlintedFeatures } from "../feature-sets.mjs";

/** process.platform word of a target triple. */
export const osOfTriple = (triple) => (/-apple-/.test(triple) ? "darwin" : /-windows-/.test(triple) ? "win32" : /-linux-/.test(triple) ? "linux" : null);

const runsOn = (crate, os) => !Array.isArray(crate.platforms) || crate.platforms.includes(os);
const packages = (names) => names.flatMap((n) => ["-p", n]);
const key = (list) => list.join(" ");

/** The sets of `list` planned on `host` as [{ features, scoped }]; `scoped` (a host-scoped set) gets no other-OS lint. */
function setsOn(list, host, which, name) {
  if (list === undefined) return undefined;
  return featureSetEntries(list, which, name).filter((s) => s.hosts === null || s.hosts.includes(host)).map((s) => ({ features: s.features, scoped: s.hosts !== null }));
}

/**
 * The lint sets and lint-only sets of one crate on this host, mirroring what the config has always meant:
 *  - `featureSets` absent: default features; and `--all-features` too when the crate declares features and names none unlinted;
 *  - `featureSets` present: exactly those sets (lint, test build and docs each);
 *  - `checkFeatureSets`: lint only; absent means one `--all-features` lint unless a lint set already is `--all-features` or the crate names unlinted features.
 */
export function cratePlan(crate, host) {
  const cfg = crate.config ?? {};
  const declared = setsOn(cfg.featureSets, host, "featureSets", crate.name);
  const noAllFeatures = Object.keys(unlintedFeatures(cfg, crate.name)).length > 0;
  const full = declared ?? [{ features: [], scoped: false }];
  const hasAll = (sets) => sets.some((s) => s.features.includes("--all-features"));
  let lintOnly = setsOn(cfg.checkFeatureSets, host, "checkFeatureSets", crate.name);
  if (lintOnly === undefined) lintOnly = hasAll(full) || noAllFeatures || !crate.declaresFeatures ? [] : [{ features: ["--all-features"], scoped: false }];
  lintOnly = lintOnly.filter((s) => s.features.length > 0 && !full.some((f) => key(f.features) === key(s.features)));
  return { full, lintOnly };
}

/**
 * Every call of a run.
 * `crates`: the selected crates of ONE Cargo workspace. `host`: process.platform. `hostTriple`: this host's target. `otherTriples`: the other OS targets linted here.
 * Order: the together calls first (they do the compile work), then the single-crate feature calls, then consumer manifests.
 */
export function planCalls({ crates, host, hostTriple, otherTriples = [] }) {
  const here = crates.filter((c) => runsOn(c, host)).sort((a, b) => a.name.localeCompare(b.name));
  const plans = new Map(here.map((c) => [c.name, cratePlan(c, host)]));
  const calls = [];
  const lintTargets = (crate, scoped) => [hostTriple, ...(scoped ? [] : otherTriples.filter((t) => runsOn(crate, osOfTriple(t))))];
  const lintCalls = (round, members, features) => {
    // One call per target triple, naming every crate that builds for it. Never several targets in one call: Cargo resolves features for
    // host dependencies and target dependencies, not per triple, so a call naming two targets gives each the union of both targets'
    // features (HeardRight on the Dell, 2026-11-10: the macOS-only `sqlcipher-system` was switched on for the Linux target and its C code
    // then wanted OpenSSL headers). A crate is never asked for a target it cannot build.
    const triples = [...new Set(members.flatMap((m) => m.targets))];
    for (const triple of triples) {
      const group = members.filter((m) => m.targets.includes(triple)).map((m) => ({ name: m.name, targets: [triple] }));
      calls.push({
        id: `lint:${round}:${osOfTriple(triple) ?? triple}`, stage: "lint", round, crates: group,
        args: ["clippy", ...packages(group.map((m) => m.name)), ...features, "--all-targets", "--no-deps", "--target", triple, "--locked", "--message-format=json", "--", "-D", "warnings"],
      });
    }
  };
  const buildCalls = (round, names, features, libs) => {
    if (names.length) calls.push({ id: `test-build:${round}`, stage: "test-build", round, crates: names.map((name) => ({ name, targets: [hostTriple] })), args: ["test", ...packages(names), ...features, "--target", hostTriple, "--locked"] });
    if (libs.length) calls.push({ id: `docs:${round}`, stage: "docs", round, crates: libs.map((name) => ({ name, targets: [hostTriple] })), args: ["doc", ...packages(libs), ...features, "--no-deps", "--lib", "--target", hostTriple, "--locked", "--message-format=json"] });
  };

  // Together rounds: the feature flags that mean the same for every crate (none, --all-features).
  for (const [round, flags] of [["default", []], ["all-features", ["--all-features"]]]) {
    const full = here.filter((c) => plans.get(c.name).full.some((s) => key(s.features) === key(flags)));
    const lintOnly = here.filter((c) => plans.get(c.name).lintOnly.some((s) => key(s.features) === key(flags)));
    const scopedOf = (c) => [...plans.get(c.name).full, ...plans.get(c.name).lintOnly].find((s) => key(s.features) === key(flags)).scoped;
    lintCalls(round, [...full, ...lintOnly].map((c) => ({ name: c.name, targets: lintTargets(c, scopedOf(c)) })), flags);
    buildCalls(round, full.map((c) => c.name), flags, full.filter((c) => c.hasLib).map((c) => c.name));
  }

  // Crate-specific sets: one crate per call.
  for (const c of here) {
    const { full, lintOnly } = plans.get(c.name);
    const specific = (s) => s.features.length > 0 && key(s.features) !== "--all-features";
    full.filter(specific).forEach((s) => {
      const round = `${c.name}[${s.features.slice(1).join(" ") || s.features.join(" ")}]`;
      lintCalls(round, [{ name: c.name, targets: lintTargets(c, s.scoped) }], s.features);
      buildCalls(round, [c.name], s.features, c.hasLib ? [c.name] : []);
    });
    lintOnly.filter(specific).forEach((s) => {
      const round = `${c.name}[${s.features.slice(1).join(" ") || s.features.join(" ")}]`;
      lintCalls(round, [{ name: c.name, targets: lintTargets(c, s.scoped) }], s.features);
    });
  }

  // Consumer manifests: small separate workspaces that exercise a crate the way an app would.
  // The same manifest with the same arguments is one call, whichever crates declare it; each of them gets its result. A consumer call is judged
  // whole (run-calls.mjs): its own package is not one of the named crates, so the call passes for them only when the whole call succeeded.
  const consumers = new Map();
  for (const c of here) {
    for (const row of c.config?.consumerRows ?? []) {
      const dir = String(row.manifestPath).replace(/[\\/]Cargo\.toml$/, "");
      const args = ["clippy", ...(row.args ?? []), "--all-targets", "--no-deps", "--target", hostTriple, ...(row.locked === false ? [] : ["--locked"]), "--message-format=json", "--", "-D", "warnings"];
      const same = `${dir}\0${key(args)}`;
      if (!consumers.has(same)) { const call = { id: `lint:consumer:${dir}${consumers.size ? `:${consumers.size + 1}` : ""}`, stage: "lint", round: `consumer:${dir}`, cwd: dir, consumer: true, crates: [], args }; consumers.set(same, call); calls.push(call); }
      consumers.get(same).crates.push({ name: c.name, targets: [hostTriple], consumer: dir });
    }
  }
  return calls;
}

/** What the plan does not lint, for the run summary: { crate: { feature: reason } } and crates that do not run on this host. */
export function planNotes({ crates, host }) {
  const notLinted = {};
  for (const c of crates) { const u = unlintedFeatures(c.config ?? {}, c.name); if (Object.keys(u).length) notLinted[c.name] = u; }
  return { notLinted, notOnThisHost: crates.filter((c) => !runsOn(c, host)).map((c) => c.name).sort() };
}
