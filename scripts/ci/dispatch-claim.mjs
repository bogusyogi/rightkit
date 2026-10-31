// Non-pinned re-export of the shared candidate claim (GB-47; GC-T21). The implementation lives INSIDE scripts/ci/rightkit-dispatch.mjs: the broker pins that one
// file by digest, so its executable closure must be that file alone (a static import of this sibling would run before any digest check could cover it, and an
// installed policy copy of the dispatcher has no checkout siblings). The dispatcher never imports this file; it exists only so tools and tests can name the
// claim API without importing the dispatcher's CLI surface, and it carries no logic of its own.
export { CLAIM_SCHEMA, CLAIM_STALE_MS, claimDigest, claimIdentity, claimKey, listRuns, readClaim, reserveClaim } from "./rightkit-dispatch.mjs";
