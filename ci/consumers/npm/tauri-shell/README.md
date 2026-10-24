# tauri-shell external npm consumer

Recipe ID: `tauri-shell-installed-transport-consumer`. Candidate source contract: `@rightkit/tauri-shell@0.1.2`, `packages/tauri-shell/src/index.ts`. Entry imports only installed public root. Memory invoke/listen transport exercises actual shell wrapper behavior; it does not impersonate Tauri or qualify native PK04/PK08/PK13 host services.

Runner copies this directory to fresh external scratch directory on Windows & macOS for both R2(a) packed archive & R2(b) registry-fetched archive. Add Linux only where locked package matrix requires it. Use provisioned Node & pinned `pnpm@11.25.0`; no ambient workspace resolution, install hooks or self-install in consumer. Install exact archive in separate runner step:

```powershell
pnpm add --save-exact --ignore-scripts "$env:RIGHTKIT_CONSUMER_ARTIFACT"
pnpm run consumer
```

```sh
pnpm add --save-exact --ignore-scripts "$RIGHTKIT_CONSUMER_ARTIFACT"
pnpm run consumer
```

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT` absolute archive path; `RIGHTKIT_CONSUMER_ARTIFACT_SHA256` exact archive lowercase SHA-256 supplied from trusted candidate record; `RIGHTKIT_CONSUMER_EXPECTED_VERSION` exact installed version (`0.1.2` for current candidate); `RIGHTKIT_CONSUMER_RESOLUTION=packed` or `fetched`. Consumer verifies archive digest, manifest identity/version & canonical installed module path inside fixture root. Runner must independently bind installed package bytes to hashed archive & retain separate installation output. No digest is invented here. Runner must refuse absent/mismatched artifact pins before executing installation.

Default `RIGHTKIT_CONSUMER_REQUIREMENT=transport` expects exit 0 & one JSON object with `ok:true`, five successful scenarios, artifact pins, host platform/arch/Node, explicit `qualificationGaps`, `rendererSafetyQualified:false` & `nativeHostQualified:false`. Assertions cover menu/tray/deep-link dispatch & stop cleanup, open-path queue/events, menu state command shape, legal declaration projection without renderer timestamp, update command shape/progress/cancel/stale refusal, unsupported capability guards without side effects. Memory-host answers are fixture data, never evidence of native authority. Output never serializes untrusted transport replies/errors.

`RIGHTKIT_CONSUMER_REQUIREMENT=renderer-safe-host-services` deliberately exits 1 with `unsupported-qualification-scope`, retaining gap evidence. It must never count as full host-service qualification. Package exports no settings/logs/license adapters; those owners have separate external consumers. Raw update/legal wrappers do not validate unknown DTOs; `asShellError` preserves arbitrary typed envelopes. Consumer proves this limitation using private sentinel without printing payload. Use `@rightkit/app-update/native` owner recipe for update renderer validation. Native-host services, secret isolation, signatures, legal manifest authority & native settings CAS remain separately qualified on required Windows/macOS hosts.

No quit-drain subscription cleanup claim: public `onQuitRequested` off removes handler while retaining shared host listener; no shell dispose API exists. Bridge/open-path/update subscriptions above do assert listener removal. No command run, pass receipt or native qualification is supplied by this source fixture.


Provenance repair: `provenance.mjs` now checks canonical fixture-root containment before importing selected package exports, then compares SHA-256 of every regular archive file with corresponding installed file. Consumer imports exact checked canonical export URLs. Unexpected archive paths/links/special entries, absent files, changed installed bytes & ancestor fallback fail closed. Result `artifact.installationBinding` must equal `all-archive-files-sha256` & `archiveFileCount` must be positive. Earlier notes assigning all installation binding to runner are superseded by this byte binding; runner still owns fresh isolated installation & trusted artifact digest selection. Copy `provenance.mjs` with fixture.
