# qa-chrome-launch-close

Standalone R2 recipe; source fixture only. No qualification receipt exists until runner executes it.

Runner owns installation, browser provisioning, invocation & evidence capture. Copy fixture outside workspace into fresh scratch consumer directory. Use repository-pinned pnpm; install exact packed tarball as consumer's only dependency, with no workspace/Git/link overrides. Repeat in fresh directory against registry-fetched exact version, retaining fetched tarball. Record installation output, resolved version/integrity & tarball SHA-256 alongside consumer result. Consumer checks declared artifact hash & installed version; runner must bind installed bytes to that artifact.

Required environment:

| Name | Value |
| --- | --- |
| `RIGHTKIT_CONSUMER_ARTIFACT` | Absolute `.tgz` path installed by runner |
| `RIGHTKIT_CONSUMER_ARTIFACT_SHA256` | Actual lowercase archive SHA-256 |
| `RIGHTKIT_CONSUMER_EXPECTED_VERSION` | Exact package version |
| `RIGHTKIT_CONSUMER_RESOLUTION` | `packed` before upload; `fetched` after registry readback |
| `RIGHTKIT_CHROME_FOR_TESTING_ROOT` | Required absolute existing managed CfT root; runner provisions `current` with package's exported `installChromeForTesting()` default pin or CLI `install` |

Run `node consumer.mjs` from consumer directory. Assets must already exist; recipe downloads nothing, invokes no package manager & fails if managed pinned browser is absent. Explicit CFT executable fallback is intentionally unused. Live DevTools version must equal installed package's exported pin.

Required hosts: Windows, macOS & Linux (PK14-2). Retain each host's stdout JSON & exit code; no host exclusions or skip flags. Run both packed & fetched modes per host. Existing package checks remain separate & unchanged.

Exit 0 requires both scenarios: normal launch → real DevTools → close → strict clean report; zero removal phase → visible incomplete-cleanup report → explicit fresh close → clean profile/process/clone assertions. Repeated public `browser.close()` must return identical report object even after incomplete cleanup. Recovery uses original owned ChildProcess; no saved PID/PGID probing, global process kills or unsafe POSIX orphan cleanup.

JSON return structure: `recipeId`, `schemaVersion`, `ok`, `host`, `artifact {path,sha256,version,resolution,modulePath}`, `browser {executablePath,pinnedVersion}`, `scenarios [{name,ok,close,recovery,error,cleanupError}]`, `errors`. Failure exits 1 with original failure & any recovery failure visible. `close`/`recovery` retain actual public reports without synthesized pass receipts. Failed or unknown profile scans never count as clean.


Provenance repair: `provenance.mjs` now checks canonical fixture-root containment before importing selected package exports, then compares SHA-256 of every regular archive file with corresponding installed file. Consumer imports exact checked canonical export URLs. Unexpected archive paths/links/special entries, absent files, changed installed bytes & ancestor fallback fail closed. Result `artifact.installationBinding` must equal `all-archive-files-sha256` & `archiveFileCount` must be positive. Earlier notes assigning all installation binding to runner are superseded by this byte binding; runner still owns fresh isolated installation & trusted artifact digest selection. Copy `provenance.mjs` with fixture.

Dispatch binding: current named npm runner admits native Windows/macOS only. Forward explicit `RIGHTKIT_CHROME_FOR_TESTING_ROOT` through sanitized runtime environment after validating existing absolute root; isolated HOME must not select browser assets. Consumer requires `current/install.json` matching exported platform/version/archive pins & executable inside canonical current directory, then checks real DevTools version. Missing root/assets/marker fails. No root value is fabricated: local readback found only legacy version folder, no managed current/install.json. Runner must provide independently provisioned root on each native host. Linux remains REQUIRED by frozen PK14-2:329 & matrix:401; absent Linux execution is missing evidence, never skipped success. `nativeHostQualified:false` excludes native host-group qualification; real Chrome assertions still run.
