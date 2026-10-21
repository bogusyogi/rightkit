// CI-only runtime evidence. Harness comes from dispatched main; measured source comes from sealed candidate.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { archiveTool } from "./registry-consumer.mjs";
import { assertEnvNames, provisionWorkloadEnv, workloadEnvironment } from "./runtime-workload-env.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
// The crate is one validated run input. Its name is a single path segment: no separators, dots or case.
const CRATE_NAME = /^rightkit-[a-z0-9-]+$/;
export function isCrateName(value) { return typeof value === "string" && CRATE_NAME.test(value); }
export function assertCrateName(value) { if (!isCrateName(value)) throw new Error("crate must match ^rightkit-[a-z0-9-]+$"); return value; }
// Trusted coverage, pinned in dispatched main per crate. A candidate's crates/<crate>/benches/workloads.json
// must equal this exactly, so a candidate cannot shrink its own required coverage. A crate without an
// entry here is refused until its coverage is pinned.
// `features` is a Cargo feature list, or null to build the crate's default feature set with no --features flag.
// `trailing` are the bench's own arguments after `--`; each is a number or a fixed long flag, pinned here.
const FEATURES = /^[a-z0-9_-]+(?:,[a-z0-9_-]+)*$/;
const TRAILING = /^(?:[0-9]+|--[a-z][a-z-]*)$/;
// `noDefaultFeatures` adds --no-default-features before --features (the shell rows select exactly one native binding).
const cargoBench = (crate, features, bench, trailing = [], noDefaultFeatures = false) => {
  if (features !== null && !FEATURES.test(features)) throw new Error("trusted workload features are malformed");
  if (!trailing.every((arg) => TRAILING.test(arg))) throw new Error("trusted workload trailing arguments are malformed");
  return Object.freeze(["test", "--locked", "--profile", "release-iterate", "-p", crate, ...(noDefaultFeatures ? ["--no-default-features"] : []), ...(features === null ? [] : ["--features", features]), "--bench", bench, ...(trailing.length ? ["--", ...trailing] : [])]);
};
// A workload may take a measurement profile: the ceilings and required scale of that named profile in the crate's
// runtime-budget.json govern it instead of the top-level ones. The profile's features and scale are pinned here.
const profileOf = (name, features, unit, minCount) => Object.freeze({ name, features: Object.freeze([...features]), requiredScale: Object.freeze({ unit, minCount }) });
const TRUSTED_COVERAGE = Object.freeze({
  "rightkit-logs": Object.freeze({
    defaultFeatures: true,
    coverage: Object.freeze({ default: "http and detached API", tracing: "tracing writer", "log-bridge": "compiled in detached workload; no additional background worker", testing: "test-only; excluded" }),
    workloads: Object.freeze([
      Object.freeze({ id: "tracing", features: "tracing", bench: "budget", intervalMs: null, description: "CI isolated tracing writer: 200000 lines then default idle window" }),
      Object.freeze({ id: "detached", features: "tracing,log-bridge", bench: "budget_detached", intervalMs: 2000, description: "CI isolated detached drainer: 200000 lines then idle >=60 s at the chosen 2000 ms interval; proof covers only this 2000 ms workload, not other intervals or an API default" }),
    ]),
    // Source the candidate must keep bound beyond the generic crate and bench inventory.
    requiredSources: Object.freeze(["crates/rightkit-logs/src/detached.rs"]),
  }),
  "rightkit-job-queue": Object.freeze({
    defaultFeatures: false,
    coverage: Object.freeze({
      default: "no workload: default features are empty (Cargo.toml [features] default = []); the default build has only JobError and InlineCancelRegistry (src/lib.rs, src/error.rs, src/inline.rs) and starts no thread or timer",
      store: "covered by pool_budget: StagedPool starts rk-pool-lane and rk-pool-dispatch threads (src/pool.rs:590-611); finite synthetic jobs exercise store, dispatcher, job workers, compute lane, lane idle callback, and shutdown",
      protocol: "covered transitively by supervisor (Cargo.toml: supervisor = [protocol, doctor, zeroize]); wire constants only, no thread or timer (src/wire.rs)",
      contract: "StagedPool store threads covered by pool_budget; contract feature's src/contract.rs:543 serves long-running requests on demand, event-driven, and is not exercised by this pool workload",
      supervisor: "covered by workload budget: an EngineSupervisor with a live engine, 100 event subscribers, and the default 2 s HealthCheck ping then idle (benches/budget.rs:55-90); the monitor thread (src/supervisor.rs:1703 boot, 1806 monitor), the health probe schedule (src/supervisor.rs:1820-1838), the event forwarder (src/supervisor.rs:1791) and the restart thread (src/supervisor.rs:1985, only when a restart is scheduled) all run under this feature",
      doctor: "covered transitively by supervisor; src/doctor_probe.rs:101-121 is a bounded probe (sleep of at most 5 ms inside its timeout), not a background thread",
      service: "StagedPool store threads covered by pool_budget; src/service.rs has no thread, sleep or timer, and service transport is not exercised by this pool workload",
      crash: "NOT COVERED, no workload needed for continuous work: src/crash_observer.rs (25 lines) contains no thread, sleep, interval or timer; it only records an abnormal engine exit",
      ffi: "NOT COVERED, one-shot only: spawn_reaper starts an rkp-reap thread (src/ffi.rs:786-803) that retries shutdown with a doubling pause from 250 ms for a bounded number of attempts; per-call threads at src/ffi.rs:659 and src/ffi.rs:849 are event-driven. No continuous timer, so a gap of lower priority than store",
    }),
    workloads: Object.freeze([
      Object.freeze({ id: "budget", features: "supervisor", bench: "budget", intervalMs: null, description: "EngineSupervisor with a live engine, 100 event subscribers and the default health ping, then idle" }),
      Object.freeze({ id: "pool_budget", features: "store,supervisor", bench: "pool_budget", trailing: Object.freeze(["10000", "30"]), env: Object.freeze(["RIGHTKIT_RUNTIME_SCRATCH_ROOT"]), intervalMs: null, description: "10000 synthetic staged jobs through scratch SQLite store, then 30 s live dispatcher/lane idle and shutdown" }),
    ]),
  }),
  "rightkit-daemon": Object.freeze({
    defaultFeatures: true,
    coverage: Object.freeze({
      default: "local native socket/named-pipe echo: 64 held connections, 20000 verified requests, 30 seconds idle",
      remote: "real loopback TLS listener, production-default durable daemon, ephemeral signed device enrollment, 10000 signed job writes, listener held idle; requires producer-provisioned per-run localhost TLS material",
      "remote-loopback": "real loopback HTTP listener and bearer authorization: 64 held TCP connections, 20000 verified echo requests, 30 seconds idle; token bucket retained with finite-load burst, no presence reaper configured",
      testing: "test-only; excluded",
      "test-mode": "test-only; excluded",
    }),
    workloads: Object.freeze([
      Object.freeze({ id: "local", features: null, bench: "budget", trailing: Object.freeze(["64", "20000", "30"]), env: Object.freeze(["RIGHTKIT_RUNTIME_SCRATCH_ROOT"]), intervalMs: 1000, description: "CI isolated native daemon echo: 64 held connections, 20000 verified requests, then 30 s idle" }),
      Object.freeze({ id: "remote", features: "remote", bench: "remote_budget", trailing: Object.freeze(["10000", "30"]), env: Object.freeze(["RIGHTKIT_RUNTIME_SCRATCH_ROOT", "RIGHTKIT_DAEMON_BUDGET_TLS_CERT", "RIGHTKIT_DAEMON_BUDGET_TLS_KEY"]), profile: profileOf("remote", ["remote"], "submissions", 10000), intervalMs: null, description: "CI isolated TLS daemon: 10000 signed job submissions over a harness-provisioned ephemeral localhost certificate, then 30 s idle" }),
      Object.freeze({ id: "remote-loopback", features: "remote-loopback", bench: "budget", trailing: Object.freeze(["--loopback", "64", "20000", "30"]), env: Object.freeze([]), intervalMs: null, description: "CI isolated loopback HTTP daemon: 64 held connections, 20000 verified echo requests, then 30 s idle" }),
    ]),
    requiredSources: Object.freeze(["crates/rightkit-daemon/src/server.rs", "crates/rightkit-daemon/src/remote/http.rs", "crates/rightkit-daemon/src/loopback/mod.rs"]),
  }),
  // Wake-ups are gated as a delta over a bare Wry loop (runtime-budget.json ceilingBasis); each native bench child pairs
  // bare and measured processes itself, so the RESULT line carries bare_wakeups_per_s and wakeups_delta_per_s.
  "rightkit-tauri-shell": Object.freeze({
    defaultFeatures: true,
    coverage: Object.freeze({
      "wv2-0_38": "native Wry hidden webview, Shell plugin, production presence loop, hidden-webview idle timer, singleton default watchdog; 0.38 native binding on Windows",
      "wv2-0_39": "same activated services with 0.39 native binding on Windows; explicit no-default-features",
      "host-services": "concrete settings/secrets services over isolated backends; CAS, write-only secrets and registered ContentWebview ACL refusal probes before paired observation; settings emitter remains installed throughout paired observation",
      "logs": "concrete LogsRuntime with actual accepted/flush-acknowledged renderer record; writer remains installed throughout paired observation",
      "crash": "real report-only watchdog crash adapter configured with default cooldown; separate native capture/recovery/cancellation functional probes outside measured pairs",
      "engine": "not activated or qualified by these rows; EngineHost sidecar/presence/supervisor requires separate held-resource workload",
      "license,update,update-license,overlay,pdf-host": "not activated or qualified by these rows; final adapter owners must classify extra held-resource work versus focused stateless command qualification",
      "content-devtools": "enabled only in native-host rows to register ContentWebview and exercise settings/secrets ACL refusal; DevTools inspection is not qualified and needs its own allowed-app native qualification",
      "dev-overrides,qa-probes": "development-only; excluded from published runtime workload",
    }),
    workloads: Object.freeze([
      Object.freeze({ id: "native-shell-wv2-0_38", features: "wv2-0_38", noDefaultFeatures: true, bench: "budget_native", trailing: Object.freeze([]), env: Object.freeze([]), intervalMs: 1000, description: "CI isolated native Wry shell, 0.38 binding: paired bare/watchdog children, 5 s settling, then 30 s observation" }),
      Object.freeze({ id: "native-shell-wv2-0_39", features: "wv2-0_39", noDefaultFeatures: true, bench: "budget_native", trailing: Object.freeze([]), env: Object.freeze([]), intervalMs: 1000, description: "CI isolated native Wry shell, 0.39 binding: paired bare/watchdog children, 5 s settling, then 30 s observation" }),
      Object.freeze({ id: "native-host-wv2-0_38", features: "wv2-0_38,host-services,logs,crash,content-devtools", noDefaultFeatures: true, bench: "budget_native", trailing: Object.freeze([]), env: Object.freeze([]), intervalMs: 1000, description: "CI isolated native Wry shell with settings/secrets services, registered ContentWebview ACL refusal, logs runtime and report-only crash adapter, 0.38 binding: paired children, then 30 s observation" }),
      Object.freeze({ id: "native-host-wv2-0_39", features: "wv2-0_39,host-services,logs,crash,content-devtools", noDefaultFeatures: true, bench: "budget_native", trailing: Object.freeze([]), env: Object.freeze([]), intervalMs: 1000, description: "CI isolated native Wry shell with settings/secrets services, registered ContentWebview ACL refusal, logs runtime and report-only crash adapter, 0.39 binding: paired children, then 30 s observation" }),
    ]),
    requiredSources: Object.freeze(["crates/rightkit-tauri-shell/src/watchdog.rs", "crates/rightkit-tauri-shell/src/lib.rs", "crates/rightkit-tauri-shell/src/host/mod.rs", "crates/rightkit-tauri-shell/src/host/policy.rs", "crates/rightkit-tauri-shell/src/logs_commands.rs"]),
  }),
  "rightkit-qa": Object.freeze({
    defaultFeatures: false,
    coverage: Object.freeze({
      default: "covered by workload budget for the engine sidecar path: its reader threads (src/engine.rs:471, 486) and output pumps (src/process.rs:429). NOT COVERED: FocusMonitor, which runs only for a launched-app session (the budget bench never launches an app). Windows is an event-driven SetWinEventHook message-pump thread with no timer (src/control/windows_focus.rs:71, 104; wired src/control.rs:429); macOS is a bounded /usr/bin/osascript helper child plus one reader thread (src/control/macos_focus.rs:103, 143; wired src/control.rs:431); other platforms keep a 20 ms poll thread (src/control.rs:475-480)",
      android: "NOT COVERED: no thread or timer in src/android.rs; it starts an adb logcat child process for the capture (src/android.rs:686-695, ChildLog). That is a background child process, not an in-crate thread, and the 15 ms loop at src/android.rs:645 runs only during shutdown, bounded by LOGCAT_REAP",
      browser: "NOT COVERED: src/browser.rs:145 builds a current-thread tokio runtime and launches a Chrome child process; src/browser.rs has no thread spawn or timer. Chromium and CDP handler threads live in the rightkit-browser and chromiumoxide dependencies, which were not inspected here",
      control: "NOT COVERED, debug builds only (src/lib.rs:49-50 gates embedded on feature control plus debug_assertions): src/embedded/win_view.rs:547-548 runs an infinite thread that sleeps 2 s and probes each WebView2 controller and browser process (Windows, with one wv2-* feature). The macOS and shared loops in src/embedded/mac_view.rs:260, src/embedded/mod.rs:1365 and src/embedded/viewport.rs:189 are bounded by timeouts. src/control.rs:142 and src/control.rs:158 are on-demand threads, event-driven, and ungated",
      "wv2-0_38": "no separate workload: selects the WebView2 binding generation only (Cargo.toml [features]; src/embedded/win_view.rs:22). The 2 s probe thread at src/embedded/win_view.rs:547 exists only with control plus one wv2 feature; see control",
      "wv2-0_39": "no separate workload: selects the WebView2 binding generation only (Cargo.toml [features]; src/embedded/win_view.rs:24). The 2 s probe thread at src/embedded/win_view.rs:547 exists only with control plus one wv2 feature; see control",
      testing: "test-only; excluded: thread spawns at src/embedded/mac_view.rs:1057 and 1130 and src/embedded/viewport.rs:341 are inside #[test] code",
    }),
    workloads: Object.freeze([
      Object.freeze({ id: "budget", features: null, bench: "budget", intervalMs: null, description: "CI isolated QA harness: one --stdio engine sidecar, 10000 echo commands at bench defaults, sidecar held open, then 30 s idle" }),
    ]),
  }),
});
// Every declared env name must have a harness provider; refuse a malformed table at load, never at measurement.
for (const entry of Object.values(TRUSTED_COVERAGE)) for (const w of entry.workloads) { assertEnvNames([...(w.env ?? [])]); cargoBench("rightkit-x", w.features, w.bench, w.trailing ?? [], w.noDefaultFeatures ?? false); }
function trustedFor(crate) {
  assertCrateName(crate);
  if (!Object.hasOwn(TRUSTED_COVERAGE, crate)) throw new Error(`no trusted supported coverage for ${crate}`);
  return TRUSTED_COVERAGE[crate];
}
export function workloadsFor(crate) {
  return trustedFor(crate).workloads.map((w) => Object.freeze({ id: w.id, args: cargoBench(crate, w.features, w.bench, w.trailing ?? [], w.noDefaultFeatures ?? false), bench: w.bench, intervalMs: w.intervalMs, description: w.description, env: w.env ?? Object.freeze([]), profile: w.profile ?? null }));
}
export function trustedRequiredSources(crate) { return [...(trustedFor(crate).requiredSources ?? [])]; }
export function supportedPrescription(crate) {
  const trusted = trustedFor(crate);
  return { schema: 1, crate, defaultFeatures: trusted.defaultFeatures, coverage: { ...trusted.coverage }, workloads: workloadsFor(crate).map((w) => ({ id: w.id, args: [...w.args], bench: w.bench, intervalMs: w.intervalMs })) };
}
export function workloadPrescriptionPath(crate) { return `crates/${assertCrateName(crate)}/benches/workloads.json`; }
export const REQUIRED_HOSTS = Object.freeze(["win32", "darwin"]);
export function validateSourcePrescription(crate, value) {
  const canonical = (item) => Array.isArray(item) ? item.map(canonical) : item && typeof item === "object" ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, canonical(item[key])])) : item;
  if (JSON.stringify(canonical(value)) !== JSON.stringify(canonical(supportedPrescription(crate)))) throw new Error("candidate workload prescription differs from trusted supported coverage");
  // Rehydrate harness-only settings from dispatched-main trust after exact source equality.
  // The candidate prescription authenticates args/bench/interval; env and profile are intentionally
  // omitted from that public file and must come from the trusted inventory.
  return workloadsFor(crate);
}
// Reads the prescription from the authenticated source tree; a missing or non-JSON blob refuses.
export function readSourcePrescription(crate, readBlob) {
  const relative = workloadPrescriptionPath(crate);
  let prescription;
  try { prescription = JSON.parse(Buffer.from(readBlob(relative)).toString("utf8")); } catch { throw new Error(`candidate ${relative} missing or not JSON`); }
  return { prescription, workloads: validateSourcePrescription(crate, prescription) };
}
export function harnessMetadataPaths(crate, prescription) {
  // Two workloads may share one bench source (daemon local and loopback); each blob is bound once.
  return [...new Set([workloadPrescriptionPath(crate), ...validateSourcePrescription(crate, prescription).map((w) => `crates/${crate}/benches/${w.bench}.rs`)])];
}
// Binds the prescription and every declared bench blob at the candidate SHA; any missing blob refuses.
export function bindHarnessMetadata(crate, prescription, readBlob) {
  const bindings = {};
  for (const full of harnessMetadataPaths(crate, prescription)) {
    let bytes;
    try { bytes = readBlob(full); } catch { throw new Error(`candidate harness metadata missing: ${full}`); }
    bindings[full] = hash(bytes);
  }
  return bindings;
}
const SHA = /^[a-f0-9]{40}$/;
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
const MEASURE_ENVIRONMENT = Object.freeze([
  "PATH", "PATHEXT", "COMSPEC", "SystemRoot", "SYSTEMROOT", "WINDIR", "HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR",
  "GITHUB_ACTIONS", "GITHUB_REF", "GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "RUNNER_NAME", "ImageVersion", "RUNTIME_SOURCE", "RUNTIME_PACKED", "RUNTIME_OUT",
]);
export function cleanMeasurementEnvironment(env) {
  return Object.fromEntries(MEASURE_ENVIRONMENT.filter((name) => env[name] !== undefined).map((name) => [name, env[name]]));
}
// The parts of runtime-budget.json a candidate may never change: top-level ceilings & scale, and each declared
// profile's features, ceilings & scale. Measurements, notes and evidence pointers stay candidate-writable.
export function frozenBudgetView(budget) {
  const profiles = budget.profiles && typeof budget.profiles === "object" ? Object.fromEntries(Object.keys(budget.profiles).sort().map((name) => {
    const p = budget.profiles[name] ?? {};
    return [name, { features: p.features, ceilings: p.ceilings, requiredScale: p.requiredScale, ceilingBasis: p.ceilingBasis, deltaCeilings: p.deltaCeilings }];
  })) : budget.profiles;
  return { ceilings: budget.ceilings, requiredScale: budget.requiredScale, ceilingBasis: budget.ceilingBasis, deltaCeilings: budget.deltaCeilings, profiles };
}
export function assertFrozenBudget(budget, trusted) {
  const now = frozenBudgetView(budget), then = frozenBudgetView(trusted);
  for (const key of Object.keys(then)) if (JSON.stringify(now[key]) !== JSON.stringify(then[key])) throw new Error(`candidate changes frozen ${key}`);
}
// The budget that governs one workload: the top-level budget, or the named profile checked against the pinned
// profile (features and required scale) in trusted code. A candidate cannot pick a looser profile or scale.
export function budgetForWorkload(budget, workload) {
  if (!workload.profile) return budget;
  const pinned = workload.profile, declared = budget.profiles?.[pinned.name];
  const featureArg = workload.args.includes("--features") ? [workload.args[workload.args.indexOf("--features") + 1]] : [];
  if (!declared || JSON.stringify(declared.features) !== JSON.stringify(pinned.features) || JSON.stringify(featureArg) !== JSON.stringify(pinned.features)) throw new Error(`runtime budget profile ${pinned.name} differs from trusted profile`);
  if (declared.requiredScale?.unit !== pinned.requiredScale.unit || declared.requiredScale?.minCount !== pinned.requiredScale.minCount) throw new Error(`runtime budget profile ${pinned.name} required scale differs from trusted scale`);
  if (!declared.ceilings || declared.ceilingBasis !== undefined || declared.deltaCeilings !== undefined) throw new Error(`runtime budget profile ${pinned.name} must declare whole-process ceilings only`);
  return { ceilings: declared.ceilings, requiredScale: { unit: pinned.requiredScale.unit, minCount: pinned.requiredScale.minCount } };
}
function command(exe, args, cwd, options = {}) {
  const result = spawnSync(exe, args, { cwd, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...options });
  if (result.error || result.status !== 0) throw new Error(`${exe} failed: ${result.error?.message ?? result.stderr ?? result.status}`);
  return result.stdout;
}
export function validateRun(run, repository, runId) {
  if (String(run.id) !== runId || run.repository?.full_name !== repository || run.head_repository?.full_name !== repository || run.path !== ".github/workflows/publish.yml" || run.head_branch !== "main" || run.event !== "workflow_dispatch" || run.status !== "completed" || !SHA.test(run.head_sha) || !Number.isSafeInteger(run.run_attempt) || run.run_attempt < 1) throw new Error("candidate must be completed publish.yml dispatch from this repository's main");
  return { repository, runId, runAttempt: String(run.run_attempt), sourceSha: run.head_sha };
}
export function selectArtifact(artifacts) {
  const matches = artifacts.filter((entry) => entry.name === "publish-packed" && !entry.expired);
  if (matches.length !== 1 || !Number.isSafeInteger(matches[0].id) || !/^sha256:[a-f0-9]{64}$/.test(matches[0].digest ?? "")) throw new Error("candidate needs exactly one unexpired publish-packed artifact with SHA-256 digest");
  return matches[0];
}
export function validateManifest(manifest, context, packed, crate) {
  assertCrateName(crate);
  if (manifest.schema !== 1 || manifest.registry !== "crates" || manifest.repository !== context.repository || String(manifest.runId) !== context.runId || String(manifest.runAttempt) !== context.runAttempt || manifest.commit !== context.sourceSha || !SHA.test(manifest.commit)) throw new Error("sealed manifest differs from authenticated candidate run");
  const units = manifest.units?.filter((unit) => unit.name === crate) ?? [];
  if (units.length !== 1) throw new Error(`candidate must contain exactly one ${crate}`);
  const unit = units[0];
  if (!/^\d+\.\d+\.\d+$/.test(unit.version) || unit.dir !== `crates/${crate}` || unit.file !== `${crate}-${unit.version}.crate` || !/^[a-f0-9]{64}$/.test(unit.sha256)) throw new Error("candidate crate identity is malformed");
  const bytes = fs.readFileSync(path.join(packed, unit.file));
  if (bytes.length !== unit.size || hash(bytes) !== unit.sha256) throw new Error("sealed crate hash/size mismatch");
  return unit;
}
export function safeArchiveMembers(text) {
  const names = text.trim().split(/\r?\n/);
  if (!names.length || names.some((name) => !name || name.includes("\\") || name.startsWith("/") || name.includes(":") || name.split("/").some((part) => part === ".." || part === "."))) throw new Error("unsafe archive member");
  return names;
}
async function resolveCandidate(env) {
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF !== "refs/heads/main" || !/^[1-9]\d*$/.test(env.RUNTIME_SOURCE_RUN ?? "")) throw new Error("dispatch from main with numeric source_run");
  const crate = assertCrateName(env.RUNTIME_CRATE);
  const repository = env.GITHUB_REPOSITORY;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) throw new Error("invalid repository");
  const api = `https://api.github.com/repos/${repository}`;
  const request = async (url) => {
    const result = await fetch(url, { headers: { Authorization: `Bearer ${env.GITHUB_TOKEN}`, Accept: "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28" } });
    if (!result.ok) throw new Error(`candidate API HTTP ${result.status}`);
    return result;
  };
  const run = await (await request(`${api}/actions/runs/${env.RUNTIME_SOURCE_RUN}`)).json();
  const context = validateRun(run, repository, env.RUNTIME_SOURCE_RUN);
  const listing = await (await request(`${api}/actions/runs/${context.runId}/artifacts?per_page=100`)).json();
  if (listing.total_count > 100) throw new Error("candidate artifact listing exceeds bound");
  const artifact = selectArtifact(listing.artifacts ?? []);
  if (!Number.isSafeInteger(artifact.size_in_bytes) || artifact.size_in_bytes <= 0 || artifact.size_in_bytes > 512 * 1024 * 1024) throw new Error("candidate archive exceeds 512 MiB bound");
  const bytes = Buffer.from(await (await request(`${api}/actions/artifacts/${artifact.id}/zip`)).arrayBuffer());
  if (bytes.length > 512 * 1024 * 1024) throw new Error("candidate archive exceeds 512 MiB bound");
  if (`sha256:${hash(bytes)}` !== artifact.digest) throw new Error("candidate artifact ZIP digest mismatch");
  const packed = env.RUNTIME_PACKED;
  fs.mkdirSync(packed, { recursive: true });
  const zip = path.join(path.dirname(packed), "runtime-candidate.zip");
  fs.writeFileSync(zip, bytes);
  const names = safeArchiveMembers(command("unzip", ["-Z1", zip], ROOT));
  // publish-packed is deliberately a flat set of files, never directories or links.
  if (new Set(names).size !== names.length || names.some((name) => !/^(?:manifest\.json|[\w.-]+\.(?:crate|upload\.json))$/.test(name))) throw new Error("publish-packed must contain unique flat crate files");
  // Read individual entries as bytes; never materialize ZIP symlinks.
  for (const name of names) fs.writeFileSync(path.join(packed, name), command("unzip", ["-p", zip, name], ROOT, { encoding: "buffer" }));
  Object.assign(context, { crate, artifactId: artifact.id, artifactDigest: artifact.digest, harnessSha: env.GITHUB_SHA });
  validateManifest(read(path.join(packed, "manifest.json")), context, packed, crate);
  write(path.join(packed, "runtime-context.json"), context);
  fs.appendFileSync(env.GITHUB_OUTPUT, `source_sha=${context.sourceSha}\n`);
}
export function assertMeasuredSource(source, sourceSha, run = command) {
  if (run("git", ["rev-parse", "HEAD"], source).trim() !== sourceSha) throw new Error("measured checkout differs from candidate source SHA");
  // Both staged & unstaged tracked changes alter bytes Cargo can execute. Git
  // applies checkout normalization when comparing, including Windows CRLF.
  if (run("git", ["status", "--porcelain", "--untracked-files=no"], source).trim()) throw new Error("measured checkout has tracked changes from candidate source SHA");
}
function bindSource(source, packed, env = process.env) {
  const context = read(path.join(packed, "runtime-context.json"));
  const crate = assertCrateName(context.crate);
  const manifestBytes = fs.readFileSync(path.join(packed, "manifest.json"));
  const unit = validateManifest(JSON.parse(manifestBytes), context, packed, crate);
  assertMeasuredSource(source, context.sourceSha);
  const archive = path.join(packed, unit.file);
  const tar = archiveTool(process.platform, env);
  const names = safeArchiveMembers(command(tar, ["-tzf", archive], source));
  const prefix = `${crate}-${unit.version}/`;
  if (names.some((name) => !name.startsWith(prefix))) throw new Error("unexpected crate archive root");
  const bindings = {};
  const sourcePaths = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, `${unit.dir}/src`], source).trim().split(/\r?\n/);
  const sealedSources = names.filter((name) => name.startsWith(`${prefix}src/`) && !name.endsWith("/"));
  if (new Set(names).size !== names.length || sealedSources.length !== sourcePaths.length) throw new Error("sealed source inventory differs from candidate");
  const { prescription, workloads } = readSourcePrescription(crate, (full) => command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" }));
  for (const full of [...sourcePaths, `${unit.dir}/Cargo.toml`]) {
    const rel = full.slice(unit.dir.length + 1);
    const member = `${prefix}${rel === "Cargo.toml" ? "Cargo.toml.orig" : rel}`;
    if (!names.includes(member)) throw new Error(`sealed source missing: ${full}`);
    const sealed = command(tar, ["-xOzf", archive, member], source, { encoding: "buffer" });
    const blob = command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" });
    if (!sealed.equals(blob)) throw new Error(`sealed source differs: ${full}`);
    bindings[full] = hash(blob);
  }
  // Cargo's package include excludes benches/**. Qualification harness metadata is
  // not shipped: bind these exact blobs to authenticated candidate sourceSha only.
  // Missing candidate blobs fail closed; never substitute a newer harness checkout.
  Object.assign(bindings, bindHarnessMetadata(crate, prescription, (full) => command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" })));
  const contextFiles = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", `${unit.dir}/benches`, "crates/rightkit-child-process/src", "crates/rightkit-child-process/Cargo.toml"], source).trim().split(/\r?\n/).filter(Boolean);
  for (const full of contextFiles) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" }));
  return { context, unit, manifestSha256: hash(manifestBytes), bindings, workloads };
}
export function measurementProblems(record, budget, sourceSha, host) {
  const problems = [];
  if (!record?.host || !record.observed || !record.dataScale) return ["missing measurement"];
  if (record.commit !== sourceSha || record.host.os !== host || !["win32", "darwin"].includes(host) || record.scope !== "synthetic") problems.push("measurement source/host/scope mismatch");
  for (const key of ["residentMemoryBytes", "idleCpuPercent", "wakeupsPerSecond", "diskOpsPerSecond"]) {
    const delta = key === "wakeupsPerSecond" && budget.ceilingBasis?.[key] === "delta-over-bare";
    const value = delta ? record.delta?.[key] : record.observed[key];
    const ceiling = delta ? budget.deltaCeilings?.[key] : budget.ceilings?.[key];
    if (delta && (!Number.isFinite(record.baseline?.[key]) || record.baseline[key] < 0 || !Number.isFinite(record.observed[key]) || Math.abs(record.observed[key] - record.baseline[key] - value) > 0.002)) problems.push(`invalid baseline/delta ${key}`);
    if (!Number.isFinite(ceiling) || ceiling <= 0) problems.push(`undeclared ceiling ${key}`);
    if (value === null && host === "darwin" && key === "diskOpsPerSecond") continue;
    if (!Number.isFinite(value) || (!delta && value < 0) || value > ceiling || (key === "residentMemoryBytes" && value === 0)) problems.push(`measurement exceeds or cannot prove ${key}`);
  }
  if (record.dataScale.unit !== budget.requiredScale.unit || !Number.isFinite(record.dataScale.count) || record.dataScale.count < budget.requiredScale.minCount) problems.push("measurement below required scale");
  return problems;
}
async function measure(env) {
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF !== "refs/heads/main") throw new Error("CI main only");
  for (const name of Object.keys(env)) if ((/TOKEN|PASSWORD|SECRET|CREDENTIAL/.test(name) && env[name]) || name.startsWith("ACTIONS_ID_TOKEN")) throw new Error(`credential-like variable ${name} forbidden during measurement`);
  const { context, unit, manifestSha256, bindings, workloads } = bindSource(env.RUNTIME_SOURCE, env.RUNTIME_PACKED, env);
  const crateDir = path.join(env.RUNTIME_SOURCE, unit.dir);
  const budget = read(path.join(crateDir, "runtime-budget.json"));
  const crate = context.crate;
  const original = read(path.join(ROOT, unit.dir, "runtime-budget.json"));
  assertFrozenBudget(budget, original);
  const output = env.RUNTIME_OUT;
  fs.mkdirSync(output, { recursive: true });
  const { recordMeasurement, measuredPathspecs } = await import("../../packages/release/runtime-budgets.mjs");
  const { specs, problems: sourceProblems = [] } = measuredPathspecs(crateDir);
  if (sourceProblems.length) throw new Error(`cannot bind measured dependency scope: ${sourceProblems.join("; ")}`);
  const measuredPaths = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, ...specs], env.RUNTIME_SOURCE).trim().split(/\r?\n/).filter(Boolean);
  for (const full of measuredPaths) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], env.RUNTIME_SOURCE, { encoding: "buffer" }));
  const toolchain = { rustc: command("rustc", ["-Vv"], env.RUNTIME_SOURCE).trim(), cargo: command("cargo", ["-V"], env.RUNTIME_SOURCE).trim() };
  const receipts = [];
  // Separate processes, sequential per host; Cargo reuses this checkout's compiled cache.
  for (const workload of workloads) {
    // Declared variables get harness-created values for this run only; the key and scratch are removed after it.
    let result, provisioned, cleanupProblem = null;
    try {
      provisioned = provisionWorkloadEnv(workload.env);
      result = spawnSync("cargo", workload.args, { cwd: env.RUNTIME_SOURCE, env: { ...workloadEnvironment(env, workload.env, provisioned.values), CARGO_INCREMENTAL: "1" }, windowsHide: true, encoding: "utf8", timeout: 40 * 60 * 1000, maxBuffer: 64 * 1024 * 1024 });
    } catch (error) { result = { error, status: null, signal: null, stdout: "", stderr: `workload environment failed: ${error.message}` }; }
    finally { try { provisioned?.dispose(); } catch (error) { cleanupProblem = error.message; } }
    const raw = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
    const rawFile = `${workload.id}-bench.txt`;
    fs.writeFileSync(path.join(output, rawFile), raw);
    const receipt = { schema: 2, workload: workload.id, intervalMs: workload.intervalMs ?? null, crate, version: unit.version, ...context, qualificationRunId: env.GITHUB_RUN_ID, qualificationRunAttempt: env.GITHUB_RUN_ATTEMPT, manifestSha256, bindings, toolchain, host: { os: process.platform, arch: process.arch, release: os.release(), runner: env.RUNNER_NAME, image: env.ImageVersion }, command: `cargo ${workload.args.join(" ")}`, envNames: [...workload.env], rawFile, rawSha256: hash(Buffer.from(raw)), exitCode: result.status, signal: result.signal, problems: [] };
    if (cleanupProblem) receipt.problems.push(cleanupProblem);
    if (result.error || result.status !== 0) receipt.problems.push(`bench failed: ${result.error?.message ?? result.status}`);
    else {
      try {
        // Owner recorder deliberately aliases same host/version/scope. Snapshot its validated
        // record immediately, then restore budget history before recording another workload.
        const budgetFile = path.join(crateDir, "runtime-budget.json");
        const before = fs.readFileSync(budgetFile);
        const alias = path.join(crateDir, "runtime-budget.evidence", `${process.platform}-${process.arch}-${unit.version}-synthetic.txt`);
        const priorEvidence = fs.existsSync(alias) ? fs.readFileSync(alias) : null;
        try {
          const record = recordMeasurement({ crateDir, resultFile: path.join(output, rawFile), scope: "synthetic", command: receipt.command, description: workload.description });
          receipt.record = preserveWorkloadRecord(record, workload.id, crate);
          fs.copyFileSync(path.join(crateDir, record.evidence.file), path.join(crateDir, receipt.record.evidence.file));
          fs.copyFileSync(path.join(crateDir, receipt.record.evidence.file), path.join(output, `${workload.id}-evidence.txt`));
        } finally {
          fs.writeFileSync(budgetFile, before);
          if (priorEvidence !== null) fs.writeFileSync(alias, priorEvidence);
          else if (fs.existsSync(alias)) fs.unlinkSync(alias);
        }
        receipt.problems.push(...measurementProblems(receipt.record, budgetForWorkload(budget, workload), context.sourceSha, process.platform));
      } catch (error) { receipt.problems.push(`record rejected: ${error.message}`); }
    }
    receipts.push(receipt);
    write(path.join(output, "qualification.json"), { schema: 2, workloads: receipts });
  }
  const problems = receipts.flatMap((r) => r.problems.map((p) => `${r.workload}: ${p}`));
  if (problems.length) throw new Error(problems.join("; "));
}
export function preserveWorkloadRecord(record, workload, crate) {
  if (!workloadsFor(crate).some((w) => w.id === workload) || !/^runtime-budget\.evidence\/[\w.-]+\.txt$/.test(record.evidence.file)) throw new Error("invalid workload/evidence path");
  return { ...record, evidence: { ...record.evidence, file: record.evidence.file.replace(/\.txt$/, `-${workload}.txt`) } };
}
export function validateWorkloadReceipts(receipts, context, unit, manifestSha256, bindings, host, harnessSha, budget, qualificationRun) {
  const crate = assertCrateName(unit.name);
  const trusted = workloadsFor(crate);
  if (context.crate !== crate) throw new Error(`${host}: mixed candidate crate`);
  if (!Array.isArray(receipts) || receipts.length !== trusted.length) throw new Error(`${host}: requires exactly the trusted workloads`);
  const seen = new Set();
  for (const receipt of receipts) {
    const workload = trusted.find((w) => w.id === receipt.workload);
    if (!workload || seen.has(receipt.workload)) throw new Error("unknown/duplicate workload");
    seen.add(receipt.workload);
    if (qualificationRun && (receipt.qualificationRunId !== qualificationRun.id || receipt.qualificationRunAttempt !== qualificationRun.attempt)) throw new Error("mixed qualification run");
    if (receipt.schema !== 2 || receipt.crate !== crate || receipt.version !== unit.version || receipt.host?.os !== host || receipt.exitCode !== 0 || receipt.signal || !Array.isArray(receipt.problems) || receipt.problems.length || receipt.command !== `cargo ${workload.args.join(" ")}` || receipt.intervalMs !== (workload.intervalMs ?? null) || JSON.stringify(receipt.envNames ?? []) !== JSON.stringify(workload.env) || receipt.record?.command !== receipt.command || receipt.record?.crateVersion !== unit.version || receipt.record?.host?.arch !== receipt.host.arch || !receipt.toolchain?.rustc || !receipt.toolchain?.cargo) throw new Error(`${host}: workload identity/status mismatch`);
    for (const key of ["crate", "repository", "runId", "runAttempt", "sourceSha", "artifactId", "artifactDigest"]) if (receipt[key] !== context[key]) throw new Error(`${host}: mixed candidate ${key}`);
    if (receipt.manifestSha256 !== manifestSha256 || receipt.harnessSha !== harnessSha || JSON.stringify(Object.entries(receipt.bindings ?? {}).sort()) !== JSON.stringify(Object.entries(bindings).sort())) throw new Error(`${host}: source/manifest/harness binding mismatch`);
    if (receipt.rawFile !== `${workload.id}-bench.txt` || !/^[a-f0-9]{64}$/.test(receipt.rawSha256 ?? "") || receipt.record.evidence.sha256 !== receipt.rawSha256 || !receipt.record.evidence.file.endsWith(`-${workload.id}.txt`)) throw new Error("workload evidence mismatch");
    const problems = measurementProblems(receipt.record, budgetForWorkload(budget, workload), context.sourceSha, host);
    if (problems.length) throw new Error(problems.join("; "));
  }
  if (new Set(receipts.map((r) => r.record.evidence.file)).size !== receipts.length) throw new Error("workload evidence overwrite");
  return receipts;
}

function measureClean(env) {
  const result = spawnSync(process.execPath, [fileURLToPath(import.meta.url), "measure"], {
    cwd: ROOT, env: cleanMeasurementEnvironment(env), stdio: "inherit", windowsHide: true,
  });
  if (result.error || result.status !== 0) throw new Error(`clean measurement process failed: ${result.error?.message ?? result.status}`);
}
async function combine(env) {
  const { context, unit, manifestSha256, bindings, workloads } = bindSource(env.RUNTIME_SOURCE, env.RUNTIME_PACKED, env);
  const crateDir = path.join(env.RUNTIME_SOURCE, unit.dir);
  const budget = read(path.join(crateDir, "runtime-budget.json"));
  const crate = context.crate;
  assertFrozenBudget(budget, read(path.join(ROOT, unit.dir, "runtime-budget.json")));
  const records = [];
  const profileRecords = {};
  const proofFiles = {};
  fs.mkdirSync(env.RUNTIME_OUT, { recursive: true });
  // Match dependency binding inventory used by each producer.
  const { measuredPathspecs } = await import("../../packages/release/runtime-budgets.mjs");
  const { specs, problems = [] } = measuredPathspecs(crateDir);
  if (problems.length) throw new Error(problems.join("; "));
  for (const full of command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, ...specs], env.RUNTIME_SOURCE).trim().split(/\r?\n/).filter(Boolean)) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], env.RUNTIME_SOURCE, { encoding: "buffer" }));
  for (const host of REQUIRED_HOSTS) {
    const dir = path.join(env.RUNTIME_RESULTS, `runtime-${host}`);
    const envelope = read(path.join(dir, "qualification.json"));
    if (envelope.schema !== 2) throw new Error("legacy single-workload evidence is insufficient");
    for (const receipt of validateWorkloadReceipts(envelope.workloads, context, unit, manifestSha256, bindings, host, env.GITHUB_SHA, budget, { id: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT })) {
      const bytes = fs.readFileSync(path.join(dir, `${receipt.workload}-evidence.txt`));
      if (hash(bytes) !== receipt.record.evidence.sha256 || hash(fs.readFileSync(path.join(dir, receipt.rawFile))) !== receipt.rawSha256 || !/^runtime-budget\.evidence\/[\w.-]+\.txt$/.test(receipt.record.evidence.file)) throw new Error("measurement evidence mismatch");
      fs.mkdirSync(path.join(crateDir, "runtime-budget.evidence"), { recursive: true });
      fs.writeFileSync(path.join(crateDir, receipt.record.evidence.file), bytes);
      records.push(receipt.record);
      // A workload with a pinned profile is held to that profile's ceilings; its record belongs to the profile.
      const profile = workloadsFor(crate).find((w) => w.id === receipt.workload).profile;
      if (profile) (profileRecords[profile.name] ??= []).push(receipt.record);
    }
    // Preserve complete producer envelopes & raw outputs for downstream authentication.
    for (const name of ["qualification.json", ...workloads.flatMap((w) => [`${w.id}-bench.txt`, `${w.id}-evidence.txt`])]) {
      const relative = `${host}/${name}`;
      const bytes = fs.readFileSync(path.join(dir, name));
      fs.mkdirSync(path.join(env.RUNTIME_OUT, host), { recursive: true });
      fs.writeFileSync(path.join(env.RUNTIME_OUT, relative), bytes);
      proofFiles[relative] = { sha256: hash(bytes), size: bytes.length };
    }
  }
  // Replace only this version's synthetic runs on qualified hosts; retain other history.
  const replaceable = (m) => m.crateVersion === unit.version && m.scope === "synthetic" && ["win32", "darwin"].includes(m.host.os);
  const profiled = new Set(Object.values(profileRecords).flat());
  budget.measurements = [...budget.measurements.filter((m) => !replaceable(m)), ...records.filter((record) => !profiled.has(record))];
  for (const [name, rows] of Object.entries(profileRecords)) budget.profiles[name].measurements = [...(budget.profiles[name].measurements ?? []).filter((m) => !replaceable(m)), ...rows];
  write(path.join(crateDir, "runtime-budget.json"), budget);
  const { syncRuntimeBudgetReadme, evaluateRuntimeBudget } = await import("../../packages/release/runtime-budgets.mjs");
  const readme = path.join(crateDir, "README.md");
  fs.writeFileSync(readme, syncRuntimeBudgetReadme(fs.readFileSync(readme, "utf8"), budget));
  const verdict = evaluateRuntimeBudget({ crate, crateDir, version: unit.version });
  fs.mkdirSync(env.RUNTIME_OUT, { recursive: true });
  for (const name of ["runtime-budget.json", "README.md"]) fs.copyFileSync(path.join(crateDir, name), path.join(env.RUNTIME_OUT, name));
  fs.cpSync(path.join(crateDir, "runtime-budget.evidence"), path.join(env.RUNTIME_OUT, "runtime-budget.evidence"), { recursive: true });
  write(path.join(env.RUNTIME_OUT, "qualification.json"), { schema: 2, ...context, manifestSha256, workloads: workloads.map((w) => w.id), proofFiles, records, verdict });
  if (!verdict.ok) throw new Error(verdict.problems.join("; "));
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const action = { resolve: resolveCandidate, measure, "measure-clean": measureClean, combine }[process.argv[2]];
    if (!action) throw new Error("usage: runtime-qualify.mjs resolve|measure|measure-clean|combine");
    await action(process.env);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
