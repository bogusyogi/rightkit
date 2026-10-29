// The mark of a release source minted by release-source-identity.mjs `resolveReleaseSource` (item 7 row 7.04).
//
// A leaf with no imports, so that the ownership evidence (an enforcement entrypoint whose import closure is pinned into the policy
// bundle) can refuse a hand-built source without pulling the door's modules into that closure. Only the resolver marks a source; a
// plain object with the same fields is not one.
const minted = new WeakSet();

/** Marks `source` as resolved by the identity module and returns it. */
export function brandReleaseSource(source) {
  minted.add(source);
  return source;
}

/** Whether `source` is an object `brandReleaseSource` marked. */
export function isReleaseSource(source) {
  return typeof source === "object" && source !== null && minted.has(source);
}
