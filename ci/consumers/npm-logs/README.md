# PK11 npm logs R2a

CI-only native Windows & macOS consumer of original sealed `@rightkit/logs` archive.
Run `node scripts/ci/npm-consumer.mjs <sealed-pack-dir> @rightkit/logs <report-file>`.
Runner creates isolated scratch package, pins pnpm from source package manifest,
installs with scripts disabled, then executes `consumer.mjs` against installed exports.
Runtime export inventory deliberately excludes TypeScript-only types. Unknown subpaths
fail closed until this named recipe is reviewed. All 37 source-bound scrub vectors,
renderer persistence/projection & exported crash capture composition must pass.
Reports bind source blobs, recipe bytes, manifest & unchanged archive bytes. Both native
hosts are required by integration. This recipe grants no publication permission;
PK11 group publication remains HOLD. Root owns workflow & semantic admission wiring.
