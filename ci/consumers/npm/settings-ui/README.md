# settings-ui-react-memory-mount

Reusable external R2 React mount fixture for candidate `@rightkit/settings-ui` 0.7.0. Fixture uses public `SettingsPanel`, `SettingsGroup`, `SettingRow` from `/core` & `licenseSection` from `/standard`. No primary source imports or native host claims; PK04-2 service qualification remains separately held. `nativeHostQualified` always stays false. Existing `crates/rightkit-tauri-shell/benches/budget_native.rs` provides native settings/secrets IPC & registered ContentWebview refusal probes through `native-host-wv2-0_38` & `native-host-wv2-0_39` CI workloads. Current Windows & macOS execution receipts remain required; this jsdom fixture & earlier 0.6.1 receipts cannot substitute for them.

Runner copies fixture outside workspace, installs selected exact settings-ui tarball as dependency alongside this manifest, records archive/resolution binding & runs `node consumer.mjs`. Runner owns package installation, native qualification & receipts. Script invokes no package manager or commands. Manifest pins pnpm 11.24.0, React/DOM 19.3.1, Testing Library React 16.3.3/DOM 10.4.2/user-event 14.6.8 & jsdom 30.1.3 matching existing onboarding fixture. RightKit peers explicitly pin react-shell 0.7.2, license 0.3.0 & theme 0.3.1 from current manifests; these must resolve published packages, never workspace/source substitutes. Logs/app-update references in selected public entries are type-only. Runner records transitive/peer resolution.

Required environment:

| Name | Value |
| --- | --- |
| `RIGHTKIT_CONSUMER_ARTIFACT` | Absolute installed `.tgz` path |
| `RIGHTKIT_CONSUMER_ARTIFACT_SHA256` | Exact archive lowercase SHA-256 |
| `RIGHTKIT_CONSUMER_EXPECTED_VERSION` | `0.7.0` for current candidate; explicit exact version for subsequent artifact |
| `RIGHTKIT_CONSUMER_RESOLUTION` | `packed` before upload; `fetched` after registry readback |

Consumer verifies supplied archive digest & installed manifest version, with module resolved inside node_modules. Runner separately proves installation derives from supplied archive; no ambient workspace fallback. Use fresh external installation per packed/fetched artifact on Windows & macOS. Native command/store/license service evidence stays separate from jsdom mount evidence.

Four observable scenarios: settings load/set/multi-key patch/subscription reload; disk-full save failure visible in real panel alert, rollback then retry preserving unrelated keys; real standard license section loads safe LicenseView, activates/deactivates, never receives or persists raw token & clears entry after success; unmount unsubscribes & remount restores saved settings. Memory license port is explicitly an in-process fixture, never native host evidence.

One stdout JSON returns `recipeId`, `schemaVersion`, `ok`, `host`, `artifact`, `scenarios [{name,ok,evidence,error}]`, `errors`, `nativeHostQualified`. Exit 0 requires all four scenarios; failures exit 1. Preserve stdout/stderr with runner installation & artifact evidence. No generated locks or success receipt supplied.


Provenance repair: `provenance.mjs` now checks canonical fixture-root containment before importing selected package exports, then compares SHA-256 of every regular archive file with corresponding installed file. Consumer imports exact checked canonical export URLs. Unexpected archive paths/links/special entries, absent files, changed installed bytes & ancestor fallback fail closed. Result `artifact.installationBinding` must equal `all-archive-files-sha256` & `archiveFileCount` must be positive. Earlier notes assigning all installation binding to runner are superseded by this byte binding; runner still owns fresh isolated installation & trusted artifact digest selection. Copy `provenance.mjs` with fixture.

Closure pin: `@rightkit/platform-ui@0.4.0` is published per frozen `01-packages.md:45,244` & canonical `packages/release/rightkit-versions.json:605`. Registry-readback react-shell 0.7.2 packed manifest declares peer `>=0.2.8 <1` & dev pin `0.4.0`. Fresh installation remains runner-owned; no execution receipt is claimed.
