# onboarding-react-memory-mount

PK10 R2 external React mount fixture. Static candidate; runner owns execution & receipts. This proves renderer behavior through installed public exports with memory backends; native host qualification remains separately required & result always reports `nativeHostQualified: false`.

Runner copies this directory outside workspace, installs supplied exact onboarding tarball as dependency alongside fixture manifest, then runs `node consumer.mjs`. Fixture never invokes package managers or commands. No source imports, workspace paths, fake native command host, generated lock or pass receipt. Record runner installation output, resolved integrity & artifact binding separately.

Candidate: `npm-onboarding-ff65-pins/packed/rightkit-onboarding-0.6.3.tgz`, version `0.6.3`, SHA-256 `5513d5059564f18857b469745cc960ae84f786d06110509bd76788d9a150a4fb`.

Direct dependency versions derive from candidate packed package.json: react-shell 0.7.2, legal-ui 0.4.1; React/React DOM 19.3.1, Testing Library React 16.3.3, DOM 10.4.2, user-event 14.6.8 & jsdom 30.1.3 use candidate's declared devDependency minimum versions. Runner resolves their registry dependencies/peers normally, records full resolution & installs no workspace substitutions. Candidate onboarding itself is intentionally absent from fixture manifest until runner installs selected archive.

Required environment:

| Name | Value |
| --- | --- |
| `RIGHTKIT_CONSUMER_ARTIFACT` | Absolute installed archive path |
| `RIGHTKIT_CONSUMER_ARTIFACT_SHA256` | Actual lowercase SHA-256; packed mode must match candidate above |
| `RIGHTKIT_CONSUMER_EXPECTED_VERSION` | `0.6.3` |
| `RIGHTKIT_CONSUMER_RESOLUTION` | `packed` for R2(a), `fetched` after registry readback |

Run on Windows & macOS. For fetched mode repeat fresh installation of exact registry-fetched version, retain archive & supply its actual digest. Artifact bytes & installed manifest version are checked; runner must establish installation came from declared archive. These fixtures do not replace 112 package tests or native host service qualification.

Each public layout (`window`, `contained`) mounts four scenarios: first run with receipt-save failure then settings-save failure, both keeping gate closed, retry reusing receipt, Back/per-step Skip/Finish & exact callback state; bare legacy acceptance refused; changed digest requires re-acceptance; global Set up later persists skipped state before callback. Real React DOM mounts run in jsdom using installed exports. Layout assertions cover rendered class behavior, not native rendering or screenshots.

One stdout JSON contains `recipeId`, `schemaVersion`, `ok`, `host`, `artifact`, `scenarios [{name,layout,ok,evidence,error}]`, `errors`, `nativeHostQualified`. Exit 0 requires eight passing scenarios; any import, version, archive, mount or behavior failure exits 1 with observed error. Preserve stdout & stderr together in runner evidence.


Provenance repair: `provenance.mjs` now checks canonical fixture-root containment before importing selected package exports, then compares SHA-256 of every regular archive file with corresponding installed file. Consumer imports exact checked canonical export URLs. Unexpected archive paths/links/special entries, absent files, changed installed bytes & ancestor fallback fail closed. Result `artifact.installationBinding` must equal `all-archive-files-sha256` & `archiveFileCount` must be positive. Earlier notes assigning all installation binding to runner are superseded by this byte binding; runner still owns fresh isolated installation & trusted artifact digest selection. Copy `provenance.mjs` with fixture.

Optional separate negative command: set `RIGHTKIT_CONSUMER_NEGATIVE_ROOT` to explicit external scratch directory, then run `node provenance-negative.mjs`. It constructs matching synthetic npm archive plus ancestor-installed export & modified in-fixture export; both must reject before import side-effect marker exists. Expected JSON `recipeId:onboarding-provenance-negative`, `ok:true`, `rejectedBeforeImport:2`. This runner source has not been executed.

Closure pin: `@rightkit/platform-ui@0.4.0` is published per frozen `01-packages.md:45,244` & canonical `packages/release/rightkit-versions.json:605`. Registry-readback react-shell 0.7.2 packed manifest declares peer `>=0.2.8 <1` & dev pin `0.4.0`. Fresh installation remains runner-owned; no execution receipt is claimed.
