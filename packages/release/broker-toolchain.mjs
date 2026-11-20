// The compiler a broker host compiles with, read in this process from the broker's own host configuration (GATE-CONTRACT NN09, NN25).
//
// The gate needs to know which cargo and rustc the broker runs: it is part of every compile row's cache key and of the receipt's binding. Until
// 2026-11-19 it asked by starting processes: `rustright cargo --version` once per cache store and `rustright rustc -V` on every run, warm ones
// included (witness fixture, Dell, 2026-11-19: both in the spawn ledger; NN09 allows the first on no run, NN25 allows no version probe on a warm
// run). Both questions travel through the client to the broker and back, and the answer is a version text that says nothing about the bytes.
//
// The broker does not choose its compiler per request. `<managed root>/config.json` registers each tool once, by absolute path and sha256
// (`tools[]`), and names which registered tool is cargo and which is rustc (`shims.cargo`, `defaults.rustcToolId` or `shims.rustc`). This module
// reads that file, checks that the two programs on disk are the registered bytes, and reads the versions from the toolchain's own channel
// manifest. Nothing is started. Any doubt (no such file, a tool not registered, bytes that differ, no channel manifest) answers `ok: false` with
// the reason, and the caller asks the client as before.
//
// Residual, stated in the evidence sheet: a broker still running with an older configuration compiles with its old tools until it is restarted,
// and nothing here can see that. The same was true of an answer remembered from an earlier `cargo --version`.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";

export const BROKER_TOOLCHAIN_SCHEMA = "rightkit.broker-toolchain/1";
const isSha = (s) => typeof s === "string" && /^[0-9a-f]{64}$/.test(s);
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** `<managed root>/config.json` for a managed client launcher `<managed root>/<dir>/rustright[.cmd]`, or null when `launcher` is not an absolute path. */
export function hostConfigFor(launcher, p = path) {
  if (typeof launcher !== "string" || !p.isAbsolute(launcher)) return null;
  return p.join(p.dirname(p.dirname(launcher)), "config.json");
}

/** The version text of `[pkg.<name>]` in a rustup channel manifest (`version = "1.99.1 (58cf8f9c3 2026-10-13)"`), or null. */
export function channelVersion(manifestText, name) {
  const lines = String(manifestText ?? "").split(/\r?\n/);
  const at = lines.indexOf(`[pkg.${name}]`);
  if (at < 0) return null;
  for (let i = at + 1; i < lines.length && !lines[i].startsWith("["); i += 1) {
    const m = /^version\s*=\s*"([^"\r\n]+)"\s*$/.exec(lines[i]);
    if (m) return m[1];
  }
  return null;
}

/**
 * The text `cargo --version` prints for the cargo of a toolchain, from the two channel-manifest versions: `cargo 1.99.1 (5f94df478 2026-08-27)`.
 * The manifest's own cargo version is the Cargo crate's (`0.100.0 (5f94df478 2026-08-27)`), which is not what cargo calls itself: cargo prints
 * the Rust release it ships with (the first word of the rustc version) and its own commit and date. Named as cargo names itself, so a reader
 * never takes the crate version for another cargo (2026-11-20: the first form of this module printed `cargo 0.100.0 …`). null when either
 * text is not of that shape.
 */
export function cargoVersionText(cargoChannel, rustcChannel) {
  const release = /^(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?) \(/.exec(String(rustcChannel ?? ""));
  const commit = /^\S+ (\([0-9a-f]{7,40} \d{4}-\d{2}-\d{2}\))$/.exec(String(cargoChannel ?? ""));
  return release && commit ? `cargo ${release[1]} ${commit[1]}` : null;
}

// One answer per process for one state of the files it was read from (their size and modification time): a run asks several times.
const kept = new Map();
const statMark = (stat, file) => { try { const st = stat(file); return st.isFile() ? `${st.size}:${Math.trunc(st.mtimeMs)}` : null; } catch { return null; } };

/**
 * `{ ok: true, schema, config, configSha256, toolchain, cargo: { path, sha256, version }, rustc: { path, sha256, version }, files }` or
 * `{ ok: false, why }`. `cargo.version` is `<what cargo --version prints> sha256:<16 hex>` (cargoVersionText) and `rustc.version` is
 * `rustc <channel version> sha256:<16 hex>`: the text a person reads and the bytes that decide. `files` are the files the answer was read from.
 */
export function brokerToolchain({ launcher, configFile = hostConfigFor(launcher), readFile = (f) => fs.readFileSync(f), stat = (f) => fs.statSync(f), remember = true } = {}) {
  if (!configFile) return { ok: false, why: "the client launcher is not an absolute path, so the host configuration beside it cannot be named" };
  let configBytes;
  try { configBytes = readFile(configFile); } catch (e) { return { ok: false, why: `cannot read ${configFile}: ${e?.code ?? e?.message}` }; }
  const configSha256 = sha256(configBytes);
  let config;
  try { config = JSON.parse(String(configBytes)); } catch { return { ok: false, why: `${configFile} is not JSON` }; }
  const tools = Array.isArray(config?.tools) ? config.tools : null;
  if (!tools) return { ok: false, why: `${configFile} registers no tools` };
  const idOf = { cargo: config?.shims?.cargo, rustc: config?.defaults?.rustcToolId ?? config?.shims?.rustc };
  const entry = {};
  for (const name of ["cargo", "rustc"]) {
    const id = idOf[name];
    if (typeof id !== "string" || !id) return { ok: false, why: `${configFile} does not name which registered tool is ${name}` };
    const found = tools.filter((t) => t && t.id === id);
    if (found.length !== 1) return { ok: false, why: `${configFile} registers ${found.length} tools with the id ${id}` };
    const t = found[0];
    if (typeof t.path !== "string" || !path.isAbsolute(t.path) || !isSha(t.sha256)) return { ok: false, why: `${configFile}: the registered ${name} has no absolute path with a sha256` };
    entry[name] = { path: t.path, sha256: t.sha256 };
  }
  const root = path.dirname(path.dirname(entry.cargo.path));
  if (path.dirname(path.dirname(entry.rustc.path)) !== root) return { ok: false, why: "the registered cargo and rustc are not of one toolchain directory" };
  const manifest = path.join(root, "lib", "rustlib", "multirust-channel-manifest.toml");
  const files = [configFile, entry.cargo.path, entry.rustc.path, manifest];
  const marks = files.map((f) => statMark(stat, f));
  if (marks.some((m) => m === null)) return { ok: false, why: `not a readable file: ${files[marks.indexOf(null)]}` };
  const keptKey = `${configSha256}\u0000${marks.join("\u0000")}`;
  if (remember && kept.has(keptKey)) return kept.get(keptKey);
  for (const name of ["cargo", "rustc"]) {
    let actual;
    try { actual = sha256(readFile(entry[name].path)); } catch (e) { return { ok: false, why: `cannot read ${entry[name].path}: ${e?.code ?? e?.message}` }; }
    if (actual !== entry[name].sha256) return { ok: false, why: `${entry[name].path} is sha256 ${actual}, the host configuration registers ${entry[name].sha256}` };
  }
  let manifestText;
  try { manifestText = String(readFile(manifest)); } catch (e) { return { ok: false, why: `cannot read ${manifest}: ${e?.code ?? e?.message}` }; }
  const versions = { cargo: channelVersion(manifestText, "cargo"), rustc: channelVersion(manifestText, "rustc") };
  for (const name of ["cargo", "rustc"]) if (!versions[name]) return { ok: false, why: `${manifest} names no version for ${name}` };
  const cargoText = cargoVersionText(versions.cargo, versions.rustc);
  if (!cargoText) return { ok: false, why: `${manifest}: the cargo version "${versions.cargo}" and the rustc version "${versions.rustc}" do not give the text cargo prints for itself` };
  // The files are read; had one changed meanwhile, the answer could mix two states.
  if (files.some((f, i) => statMark(stat, f) !== marks[i])) return { ok: false, why: "a toolchain file changed while it was read" };
  const answer = {
    ok: true, schema: BROKER_TOOLCHAIN_SCHEMA, config: configFile, configSha256, toolchain: typeof config?.defaults?.toolchain === "string" ? config.defaults.toolchain : path.basename(root),
    cargo: { ...entry.cargo, version: `${cargoText} sha256:${entry.cargo.sha256.slice(0, 16)}` },
    rustc: { ...entry.rustc, version: `rustc ${versions.rustc} sha256:${entry.rustc.sha256.slice(0, 16)}` },
    files,
  };
  if (remember) kept.set(keptKey, answer);
  return answer;
}
