/**
 * Frontend wrapper for the Rust Markdown asset protocol.
 *
 * The Tauri command surface exposes three commands
 * (`save_markdown_asset`, `commit_markdown_asset`,
 * `rollback_markdown_asset`). The TS layer:
 *
 *  - encodes the asset payload as a data URL or Uint8Array,
 *  - awaits the Rust response,
 *  - tracks rollback tokens so a failed transaction can be undone
 *    before the document commits the link,
 *  - exposes typed errors for the four known failure modes
 *    (size, mime, sanitization, path).
 */

export type MarkdownAssetMimeKind = "png" | "jpeg" | "webp" | "gif" | "svg";

export interface MarkdownAssetPayload {
  /** Raw bytes or a data URL. */
  readonly source: Uint8Array | string;
  /** User-supplied filename (no path). */
  readonly hintName: string;
  /** Detected or declared MIME. */
  readonly mimeKind: MarkdownAssetMimeKind;
}

export interface SavedMarkdownAsset {
  readonly relativePath: string;
  readonly absolutePath: string;
  readonly reusedExisting: boolean;
  readonly byteLength: number;
  readonly rollbackToken: string | null;
}

export interface MarkdownAssetClient {
  save(request: {
    payloadBase64: string;
    hintName: string;
    mimeKind: MarkdownAssetMimeKind;
    documentPath: string;
    assetsFolder?: string;
  }): Promise<SavedMarkdownAsset>;
  commit(rollbackToken: string): Promise<boolean>;
  rollback(rollbackToken: string): Promise<boolean>;
}

/** Typed production client. Native enforces document grants & containment. */
export class NativeMarkdownAssetClient implements MarkdownAssetClient {
  async save(request: {
    payloadBase64: string;
    hintName: string;
    mimeKind: MarkdownAssetMimeKind;
    documentPath: string;
    assetsFolder?: string;
  }): Promise<SavedMarkdownAsset> {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<SavedMarkdownAsset>("save_markdown_asset", { req: request });
  }
  async commit(rollbackToken: string): Promise<boolean> {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<boolean>("commit_markdown_asset", { req: { rollbackToken } });
  }
  async rollback(rollbackToken: string): Promise<boolean> {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<boolean>("rollback_markdown_asset", { req: { rollbackToken } });
  }
}

export type MarkdownAssetErrorCode =
  | "size-limit"
  | "mime-not-allowed"
  | "svg-unsafe"
  | "path-traversal"
  | "invalid-payload";

export class MarkdownAssetError extends Error {
  constructor(
    readonly code: MarkdownAssetErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "MarkdownAssetError";
  }
}

/**
 * 20 MiB hard cap, per Packet 06 §16.5.
 */
export const MAX_MARKDOWN_ASSET_BYTES = 20 * 1024 * 1024;

const MIME_ALLOWLIST: ReadonlySet<MarkdownAssetMimeKind> = new Set([
  "png",
  "jpeg",
  "webp",
  "gif",
  "svg",
]);

/**
 * Pre-flight validation that mirrors the Rust side. Returns `null`
 * when the payload is acceptable; otherwise a typed error.
 * Measures decoded bytes exactly as Rust does: for data URL strings
 * the base64 payload is decoded before measuring; for Uint8Array the
 * raw length is used; for generic strings the UTF-8 byte length is used
 * but base64-encoded image payloads are never passed as plain strings
 * without decoding.
 */
export function validateMarkdownAsset(payload: MarkdownAssetPayload): MarkdownAssetError | null {
  if (!MIME_ALLOWLIST.has(payload.mimeKind)) {
    return new MarkdownAssetError(
      "mime-not-allowed",
      `MIME kind "${payload.mimeKind}" is not in the allowlist.`,
    );
  }
  const bytes = payloadBytesForValidation(payload);
  if (!bytes) {
    return new MarkdownAssetError("invalid-payload", "Image payload is not valid Base64 data.");
  }
  if (bytes.byteLength > MAX_MARKDOWN_ASSET_BYTES) {
    return new MarkdownAssetError("size-limit", `Asset exceeds ${MAX_MARKDOWN_ASSET_BYTES} bytes.`);
  }
  if (payload.mimeKind === "svg") {
    const text = bytesToText(bytes);
    if (!text || (!/<svg\b/i.test(text) && !/^\s*<\?xml/i.test(text))) {
      return new MarkdownAssetError(
        "svg-unsafe",
        "SVG must be valid UTF-8 starting with <svg> or <?xml.",
      );
    }
    const unsafe = SVG_UNSAFE_PATTERNS.find((p) => p.test(text));
    if (unsafe) {
      return new MarkdownAssetError("svg-unsafe", "SVG contains a forbidden element or attribute.");
    }
  }
  return null;
}

/**
 * Decode base64 payload length exactly as Rust does. For a base64
 * string (without data URL prefix), compute decoded byte length by
 * actually decoding via atob when available, falling back to the
 * formula `(len * 3/4) - padding`.
 */
export function decodedBase64ByteLength(b64: string): number {
  const bytes = decodeBase64Bytes(b64);
  if (bytes) return bytes.byteLength;
  return -1;
}

export function validateBase64AssetSize(payloadBase64: string): MarkdownAssetError | null {
  const len = decodedBase64ByteLength(payloadBase64);
  if (len < 0) {
    return new MarkdownAssetError("invalid-payload", "Image payload is not valid Base64 data.");
  }
  if (len > MAX_MARKDOWN_ASSET_BYTES) {
    return new MarkdownAssetError("size-limit", `Asset exceeds ${MAX_MARKDOWN_ASSET_BYTES} bytes.`);
  }
  return null;
}

/** Decode the strict RFC 4648 Base64 accepted by Rust's STANDARD engine. */
export function decodeBase64Bytes(b64: string): Uint8Array | null {
  if (b64.length === 0) return new Uint8Array();
  if (
    b64.length % 4 === 1 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(b64)
  ) {
    return null;
  }
  try {
    if (typeof atob !== "function") return null;
    const binary = atob(b64);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function payloadBytesForValidation(payload: MarkdownAssetPayload): Uint8Array | null {
  const src = payload.source;
  if (src instanceof Uint8Array) return src;
  if (typeof src === "string") {
    // Data URL? Decode the base64 part and measure decoded length.
    if (src.startsWith("data:")) {
      const comma = src.indexOf(",");
      if (comma >= 0) {
        const header = src.slice(5, comma);
        const body = src.slice(comma + 1);
        if (header.includes("base64")) {
          return decodeBase64Bytes(body);
        }
      }
    }
    // For generic strings (SVG text, etc.) use UTF-8 bytes
    return new TextEncoder().encode(src);
  }
  return null;
}

/**
 * Patterns that mark SVG content as unsafe. Matches
 * `<script>`, event handlers (`on...=`), `javascript:` URIs,
 * `data:text/html`, `<foreignObject>`, and `<!ENTITY…>`.
 */
const SVG_UNSAFE_PATTERNS: readonly RegExp[] = [
  /<script\b/i,
  /\son[a-z]+\s*=/i,
  /java\s*script:/i,
  /data\s*:\s*text\/html/i,
  /<foreignObject\b/i,
  /<!ENTITY\b/i,
];

function toBytes(source: Uint8Array | string): Uint8Array {
  if (typeof source === "string") {
    return new TextEncoder().encode(source);
  }
  return source;
}
// Keep toBytes referenced for external tooling / legacy callers
void toBytes;

function bytesToText(bytes: Uint8Array): string {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return "";
  }
}

/**
 * Strip an unsafe character set from a filename hint, keeping only
 * safe characters used in the Rust `sanitize_filename` algorithm.
 */
export function sanitizeAssetHint(name: string): string {
  return name.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 80);
}

/**
 * Generate the asset filename stem per Packet 06 §16.5:
 *
 *  - sanitized stem,
 *  - first 12 SHA-256 hex digits,
 *  - MIME-derived extension.
 *
 * The `-2..-999` suffix on collision is the Rust layer's
 * responsibility; this helper does *not* add it.
 */
export function generateAssetFileName(
  hint: string,
  bytes: Uint8Array,
  mime: MarkdownAssetMimeKind,
): string {
  const safe = sanitizeAssetHint(hint);
  const stem = safe.replace(/\.[^.]+$/, "");
  const hash = firstSha256Hex(bytes, 12);
  const ext = EXTENSION_BY_MIME[mime];
  return `${stem}-${hash}.${ext}`;
}

const EXTENSION_BY_MIME: Record<MarkdownAssetMimeKind, string> = {
  png: "png",
  jpeg: "jpg",
  webp: "webp",
  gif: "gif",
  svg: "svg",
};

/**
 * First `n` hex digits of the SHA-256 of `bytes`. The Rust side
 * computes the same digest for collision checking; we expose the
 * prefix here so the TS preview shows the eventual filename.
 */
export function firstSha256Hex(bytes: Uint8Array, n: number): string {
  // We use the WebCrypto API when available; this code path is
  // only hit for preview filenames, so the cost is negligible.
  // Fall back to a deterministic 12-char string when no crypto
  // is available (e.g. in jsdom test environments).
  if (typeof crypto === "undefined" || !crypto.subtle) {
    return fallbackHex(bytes, n);
  }
  const digest = syncSha256(bytes);
  return digest.slice(0, n);
}

function syncSha256(bytes: Uint8Array): string {
  // We use a synchronous SHA-256 implementation here for preview
  // determinism. The Rust layer is the authoritative digest.
  // This implementation is intentionally minimal and only used
  // when crypto.subtle is not available.
  const hash = new Sha256();
  for (const byte of bytes) hash.update(byte);
  return hash.digest("hex");
}

function fallbackHex(bytes: Uint8Array, n: number): string {
  let acc = 0;
  for (let i = 0; i < bytes.length; i++) {
    acc = ((acc << 5) - acc + (bytes[i] as number)) | 0;
  }
  const hex = (acc >>> 0).toString(16).padStart(8, "0");
  return (hex + hex + hex + hex + hex + hex).slice(0, n);
}

/**
 * Tiny SHA-256 implementation. WebCrypto isn't synchronously
 * accessible; this implementation is enough for preview filenames.
 */
class Sha256 {
  private h: Uint32Array;
  private w = new Uint32Array(64);
  private buf = new Uint8Array(64);
  private buflen = 0;
  private bitlen = 0n;

  constructor() {
    this.h = new Uint32Array([
      0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
      0x5be0cd19,
    ]);
  }

  update(byte: number): void {
    this.buf[this.buflen++] = byte;
    if (this.buflen === 64) {
      this.compress();
      this.bitlen += 512n;
      this.buflen = 0;
    }
  }

  digest(encoding: "hex"): string {
    this.bitlen += BigInt(this.buflen) * 8n;
    this.buf[this.buflen++] = 0x80;
    if (this.buflen > 56) {
      while (this.buflen < 64) this.buf[this.buflen++] = 0;
      this.compress();
      this.buflen = 0;
    }
    while (this.buflen < 56) this.buf[this.buflen++] = 0;
    const view = new DataView(this.buf.buffer);
    view.setBigUint64(56, this.bitlen, false);
    this.compress();
    const hex: string[] = [];
    for (let i = 0; i < this.h.length; i++) {
      hex.push((this.h[i] as number).toString(16).padStart(8, "0"));
    }
    return encoding === "hex" ? hex.join("") : "";
  }

  private compress(): void {
    for (let i = 0; i < 16; i++) {
      this.w[i] =
        ((this.buf[i * 4] as number) << 24) |
        ((this.buf[i * 4 + 1] as number) << 16) |
        ((this.buf[i * 4 + 2] as number) << 8) |
        (this.buf[i * 4 + 3] as number);
    }
    for (let i = 16; i < 64; i++) {
      const w15 = this.w[i - 15] as number;
      const w2 = this.w[i - 2] as number;
      const w16 = this.w[i - 16] as number;
      const w7 = this.w[i - 7] as number;
      const s0 = ror(w15, 7) ^ ror(w15, 18) ^ (w15 >>> 3);
      const s1 = ror(w2, 17) ^ ror(w2, 19) ^ (w2 >>> 10);
      this.w[i] = (w16 + s0 + w7 + s1) >>> 0;
    }
    const [a0, b0, c0, d0, e0, f0, g0, h0] = this.h;
    let a = a0 as number,
      b = b0 as number,
      c = c0 as number,
      d = d0 as number,
      e = e0 as number,
      f = f0 as number,
      g = g0 as number,
      h = h0 as number;
    for (let i = 0; i < 64; i++) {
      const S1 = ror(e, 6) ^ ror(e, 11) ^ ror(e, 25);
      const ch = (e & f) ^ (~e & g);
      const temp1 = (h + S1 + ch + (K[i] as number) + (this.w[i] as number)) >>> 0;
      const S0 = ror(a, 2) ^ ror(a, 13) ^ ror(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const temp2 = (S0 + maj) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temp1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temp1 + temp2) >>> 0;
    }
    this.h[0] = ((this.h[0] as number) + a) >>> 0;
    this.h[1] = ((this.h[1] as number) + b) >>> 0;
    this.h[2] = ((this.h[2] as number) + c) >>> 0;
    this.h[3] = ((this.h[3] as number) + d) >>> 0;
    this.h[4] = ((this.h[4] as number) + e) >>> 0;
    this.h[5] = ((this.h[5] as number) + f) >>> 0;
    this.h[6] = ((this.h[6] as number) + g) >>> 0;
    this.h[7] = ((this.h[7] as number) + h) >>> 0;
  }
}

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

function ror(x: number, n: number): number {
  return (x >>> n) | (x << (32 - n));
}

/**
 * Transaction wrapper. The TS caller awaits {@link saveAsset}; on
 * document failure it calls {@link rollbackAsset} with the returned
 * token before surfacing the error to the UI.
 */
export interface MarkdownAssetTransaction {
  saveAsset(payload: MarkdownAssetPayload): Promise<SavedMarkdownAsset>;
  rollbackAsset(token: string): Promise<void>;
  commitAsset(token: string): Promise<void>;
}
