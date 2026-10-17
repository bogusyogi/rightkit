import { createHash } from "node:crypto";
import { materialDigestInput, type LegalManifest } from "./core.js";

export * from "./core.js";
export * from "./defaults.js";

/** Node entry. Browsers and webviews use `@rightkit/legal/browser` (WebCrypto). */
export function materialAcceptanceDigest(manifest: LegalManifest): string {
  return createHash("sha256").update(materialDigestInput(manifest)).digest("hex");
}
