import { materialDigestInput, type LegalManifest } from "./core.js";

// Browser-safe entry: no node:crypto. Everything here uses WebCrypto
// (globalThis.crypto.subtle), available in webviews, workers and Node 20+.
export * from "./core.js";

/** SHA-256 hex of UTF-8 text via WebCrypto. Same output as `sha256Hex` on the Node entry. */
export async function sha256HexAsync(text: string): Promise<string> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) throw new Error("WebCrypto (crypto.subtle) is not available in this context");
  const digest = await subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Same value as `materialAcceptanceDigest` on the Node entry. */
export async function materialAcceptanceDigestAsync(manifest: LegalManifest): Promise<string> {
  return sha256HexAsync(materialDigestInput(manifest));
}
