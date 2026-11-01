/**
 * Deterministic FNV-1a 64-bit hash.
 *
 * This is an *internal* comparison key. It is never shown in the UI,
 * never embedded in user-facing IDs, and is *not* a cryptographic
 * hash. Two distinct strings are allowed to collide; consumers must
 * not rely on the hash alone for identity.
 *
 * The function is implemented with `BigInt` and `BigInt.asUintN(64)`
 * so it produces a fixed-width, deterministic 16-character hex
 * string regardless of platform.
 */
export function markdownHash(value: string): string {
  let hash = 0xcbf29ce484222325n;
  for (let i = 0; i < value.length; i++) {
    hash ^= BigInt(value.charCodeAt(i));
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
  }
  return hash.toString(16).padStart(16, "0");
}
