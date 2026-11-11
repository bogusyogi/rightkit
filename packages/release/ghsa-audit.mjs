// Class A `ghsa-audit`: the GitHub advisory database, beside cargo-audit's RustSec (rightkit#1461).
//
// cargo-audit and cargo-deny read the pinned RustSec database only. A GitHub advisory RustSec has not yet copied (zip 9.0.4, GHSA-m665-v7pm-7grj,
// critical, 2026-10-28; Pulse 5d8dac65 passed every cargo-audit scan the week GitHub flagged it) is invisible to both. This scan asks OSV
// (api.osv.dev, which serves the GitHub advisory database as GHSA-* records for the crates.io ecosystem) about every crates.io package of a
// workspace's Cargo.lock at the locked version, and a GHSA advisory fails the scan exactly like a RustSec vulnerability does.
//
// - A GHSA record that lists a RUSTSEC alias is RustSec's too: cargo-audit and cargo-deny judge it (and the repository's reviewed ignore ledger
//   decides it), so this scan never double-reports it. A withdrawn record is skipped. Every other GHSA record fails.
// - Cached like the other advisory scans (GATE-CONTRACT GC-A19): by the lockfile's bytes and the UTC day, under the advisory cache root. A
//   run with no cache (the door's `live-scans` refresh) asks again.
// - Offline follows cargo-audit's rule: no same-day cached answer and no network means the scan fails (code ADVISORY_DB); it never passes
//   unchecked and never falls back to another source.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const OSV = "https://api.osv.dev/v1";
const REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
const BATCH = 500;
const TIMEOUT_MS = 30_000;

/** The `[[package]]` entries of a Cargo.lock that come from crates.io: [{ name, version }], sorted and unique. Path and git packages have no advisory here. */
export function registryPackages(lockText) {
  const out = new Map();
  for (const block of String(lockText ?? "").split(/^\[\[package\]\]\s*$/m).slice(1)) {
    const name = /^name\s*=\s*"([^"]+)"/m.exec(block)?.[1];
    const version = /^version\s*=\s*"([^"]+)"/m.exec(block)?.[1];
    const source = /^source\s*=\s*"([^"]+)"/m.exec(block)?.[1];
    if (name && version && source === REGISTRY_SOURCE) out.set(`${name}@${version}`, { name, version });
  }
  return [...out.values()].sort((a, b) => (a.name === b.name ? (a.version < b.version ? -1 : 1) : a.name < b.name ? -1 : 1));
}

const utcDay = (now) => new Date(now()).toISOString().slice(0, 10);

// A network failure or a 429/5xx answer is retried (three attempts, 1 s then 3 s apart): parallel qualification jobs each ask
// OSV for the whole root lock, and a single dropped connection refused whole units (qualify-direct runs 43071676354,
// 43077177456: "fetch failed"). Any other answer, and a third failure, still throws: the audit never passes unasked.
const RETRY_DELAYS_MS = [1_000, 3_000];
const retryable = (status) => status === 429 || status >= 500;
async function osvJson(fetchFn, url, init, { sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)) } = {}) {
  for (let attempt = 0; ; attempt += 1) {
    let r;
    try {
      r = await fetchFn(url, { ...init, signal: AbortSignal.timeout(TIMEOUT_MS) });
    } catch (error) {
      if (attempt >= RETRY_DELAYS_MS.length) throw error;
      await sleep(RETRY_DELAYS_MS[attempt]);
      continue;
    }
    if (r.ok) return r.json();
    if (!retryable(r.status) || attempt >= RETRY_DELAYS_MS.length) throw new Error(`${url} answered HTTP ${r.status}`);
    await sleep(RETRY_DELAYS_MS[attempt]);
  }
}

/** One OSV batch query over `packages`, following page tokens: a Map of "name@version" to the ids OSV lists for it. */
async function queryIds(fetchFn, packages) {
  const found = new Map();
  for (let i = 0; i < packages.length; i += BATCH) {
    let pending = packages.slice(i, i + BATCH).map((p) => ({ p, token: null }));
    while (pending.length) {
      const body = { queries: pending.map(({ p, token }) => ({ package: { name: p.name, ecosystem: "crates.io" }, version: p.version, ...(token ? { page_token: token } : {}) })) };
      const answer = await osvJson(fetchFn, `${OSV}/querybatch`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
      if (!Array.isArray(answer?.results) || answer.results.length !== pending.length) throw new Error("OSV returned a batch answer that does not match the query");
      const next = [];
      answer.results.forEach((res, k) => {
        const key = `${pending[k].p.name}@${pending[k].p.version}`;
        for (const v of res?.vulns ?? []) if (typeof v?.id === "string") found.set(key, [...(found.get(key) ?? []), v.id]);
        if (res?.next_page_token) next.push({ p: pending[k].p, token: res.next_page_token });
      });
      pending = next;
    }
  }
  return found;
}

/**
 * The GitHub-advisory findings for a Cargo.lock: `{ findings: ["vulnerable: zip 9.0.4 GHSA-... (summary)", ...], checked }`. Throws when OSV cannot be asked.
 * `fetchFn` is fetch; `cacheDir` (optional) holds the same-UTC-day answer for these exact lock bytes.
 */
export async function ghsaFindings({ lockText, fetchFn = globalThis.fetch, cacheDir = null, now = () => Date.now() }) {
  const packages = registryPackages(lockText);
  const cacheFile = cacheDir ? path.join(cacheDir, `${crypto.createHash("sha256").update(String(lockText)).digest("hex")}-${utcDay(now)}.json`) : null;
  if (cacheFile) {
    try { const hit = JSON.parse(fs.readFileSync(cacheFile, "utf8")); if (Array.isArray(hit?.findings) && Number.isInteger(hit.checked)) return { findings: hit.findings, checked: hit.checked, cached: true }; } catch { /* a miss */ }
  }
  if (!packages.length) return { findings: [], checked: 0, cached: false };
  if (typeof fetchFn !== "function") throw new Error("no fetch is available to ask the GitHub advisory database");
  const ids = await queryIds(fetchFn, packages);
  const details = new Map();
  const findings = [];
  for (const [key, list] of [...ids.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
    for (const id of [...new Set(list)].sort()) {
      if (id.startsWith("RUSTSEC-")) continue; // RustSec's own record: cargo-audit and cargo-deny judge it, with the repository's ignore ledger
      if (!details.has(id)) details.set(id, await osvJson(fetchFn, `${OSV}/vulns/${encodeURIComponent(id)}`, { method: "GET" }));
      const v = details.get(id);
      if (v?.withdrawn) continue;
      if ((v?.aliases ?? []).some((a) => String(a).startsWith("RUSTSEC-"))) continue;
      const [name, version] = [key.slice(0, key.lastIndexOf("@")), key.slice(key.lastIndexOf("@") + 1)];
      const summary = typeof v?.summary === "string" ? ` (${v.summary.replace(/\s+/g, " ").trim().slice(0, 120)})` : "";
      findings.push(`vulnerable: ${name} ${version} ${id}${summary}`);
    }
  }
  if (cacheFile) {
    try { fs.mkdirSync(cacheDir, { recursive: true }); fs.writeFileSync(cacheFile, `${JSON.stringify({ findings, checked: packages.length })}\n`); } catch { /* never fails the scan */ }
  }
  return { findings, checked: packages.length, cached: false };
}
