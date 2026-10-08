import { setTimeout as sleep } from "node:timers/promises";
import { PublicationError } from "./publication-artifacts.mjs";

// https://doc.rust-lang.org/cargo/reference/registry-index.html#index-files
export function cargoIndexUrl(name) {
  if (!/^[a-z0-9_-]+$/i.test(name)) throw new Error(`invalid crate name: ${name}`);
  const lower = name.toLowerCase();
  const prefix = lower.length === 1 ? "1" : lower.length === 2 ? "2"
    : lower.length === 3 ? `3/${lower[0]}` : `${lower.slice(0, 2)}/${lower.slice(2, 4)}`;
  return `https://index.crates.io/${prefix}/${lower}`;
}

export async function verifyCargoRegistry({ name, version, sha256 }, deps = {}) {
  const fetchImpl = deps.fetch ?? fetch;
  const wait = deps.sleep ?? sleep;
  const now = deps.now ?? (() => performance.now());
  const timeoutMs = deps.timeoutMs ?? 300_000;
  const deadline = now() + timeoutMs;
  const url = cargoIndexUrl(name);
  let delayMs = 1_000;
  while (now() < deadline) {
    let entry;
    try {
      const response = await fetchImpl(url, {
        headers: { Accept: "text/plain", "Cache-Control": "no-cache", "User-Agent": "rightkit-release" },
        signal: AbortSignal.timeout(Math.max(1, Math.ceil(Math.min(30_000, deadline - now())))), redirect: "error",
      });
      if (response.ok) {
        entry = (await response.text()).split(/\r?\n/).filter((line) => line.trim())
          .map((line) => JSON.parse(line)).find((record) => record.name === name && record.vers === version);
      }
    } catch {
      // Index propagation, HTTP errors & transient transport failures share this bounded retry.
    }
    if (entry) {
      if (entry.cksum !== sha256) {
        const error = new PublicationError("PUBLICATION_REGISTRY_MISMATCH",
          `${name}@${version}: verified sha256 ${sha256}, registry sha256 ${entry.cksum ?? "<missing>"}. Operator must yank this version with cargo yank --vers ${version} ${name}; no automatic yank was performed.`);
        error.expectedSha256 = sha256;
        error.registrySha256 = entry.cksum;
        throw error;
      }
      return { name, version, sha256 };
    }
    const remainingMs = deadline - now();
    if (remainingMs <= 0) break;
    await wait(Math.min(delayMs, remainingMs));
    delayMs = Math.min(delayMs * 2, 15_000);
  }
  throw new PublicationError("PUBLICATION_REGISTRY_TIMEOUT", `${name}@${version} checksum was not visible at ${url} within ${timeoutMs}ms`);
}
