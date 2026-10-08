# RightKit inventory status

Generated: 2026-10-08T18:07:00.395Z. Command: `right-release inventory check`.

Result: 155 action item(s), 96 info item(s).

Sources: crates.io sparse index and search API, npm registry, GitHub tags for RightKitSwift, and the local manifests. No credentials are read. Registry responses are cached for one hour under the OS temp directory.

## Summary

| Section | Action | Info |
| --- | ---: | ---: |
| 1. Registry parity | 7 | 17 |
| 2. Account parity | 4 | 1 |
| 3. Third-party requirements | 33 | 78 |
| 4. App adoption | 108 | 0 |
| 5. Generated docs | 0 | 0 |
| 6. New-package hygiene | 3 | 0 |

## 1. Registry parity

| Ecosystem | Package | Public | Source | Registry latest | Feed recorded | Staged | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| cargo | `agent-room-core` | no | 0.3.0 |  |  | 0.3.0 | private-crate |
| cargo | `rightkit-agent-tools` | no | 0.1.2 |  |  |  | private-crate |
| cargo | `rightkit-align` | no | 0.0.0 |  |  |  | private-crate |
| cargo | `rightkit-asr` | no | 0.0.2 |  |  |  | private-crate |
| cargo | `rightkit-audio` | yes | 0.1.1 | 0.1.1 | 0.1.1 | 0.1.1 | ok |
| cargo | `rightkit-automation` | no | 0.1.0 |  |  | 0.1.0 | private-crate |
| cargo | `rightkit-browser` | yes | 0.2.3 | 0.2.2 | 0.2.2 | 0.2.3 | source-unpublished |
| cargo | `rightkit-cache` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-channel-memory` | no | 0.1.0 |  |  |  | private-crate |
| cargo | `rightkit-compose` | no | 0.0.0 |  |  |  | private-crate |
| cargo | `rightkit-computer` | no | 0.1.0 |  |  |  | private-crate |
| cargo | `rightkit-contracts` | no | 0.1.0 |  |  |  | private-crate |
| cargo | `rightkit-control` | yes | 0.1.9 | 0.1.9 | 0.1.9 | 0.1.9 | ok |
| cargo | `rightkit-crash` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-db` | yes | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | ok |
| cargo | `rightkit-embed` | yes | 0.2.2 | 0.2.2 | 0.2.2 | 0.2.2 | ok |
| cargo | `rightkit-framed-sidecar` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-fs` | yes | 0.2.0 | 0.2.0 | 0.2.0 | 0.2.0 | ok |
| cargo | `rightkit-hotkeys` | yes | 0.1.0 |  |  |  | never-published |
| cargo | `rightkit-http` | yes | 0.2.0 | 0.2.0 | 0.2.0 | 0.2.0 | ok |
| cargo | `rightkit-jobs` | yes | 0.2.4 | 0.2.4 | 0.2.4 | 0.2.4 | ok |
| cargo | `rightkit-license` | yes | 0.1.3 | 0.1.3 | 0.1.3 | 0.1.3 | ok |
| cargo | `rightkit-llm` | yes | 0.1.0 |  |  |  | never-published |
| cargo | `rightkit-logs` | yes | 0.2.0 | 0.2.0 | 0.2.0 | 0.2.0 | ok |
| cargo | `rightkit-mcp` | yes | 0.5.0 | 0.4.0 | 0.4.0 | 0.5.0 | source-unpublished |
| cargo | `rightkit-media` | yes | 0.2.3 | 0.2.3 | 0.2.3 | 0.2.3 | ok |
| cargo | `rightkit-models` | yes | 0.2.3 | 0.2.3 | 0.2.3 | 0.2.3 | ok |
| cargo | `rightkit-oauth` | yes | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | ok |
| cargo | `rightkit-ocr` | no | 0.1.2 |  |  |  | private-crate |
| cargo | `rightkit-ort` | yes | 0.2.1 | 0.2.1 | 0.2.1 | 0.2.1 | ok |
| cargo | `rightkit-overlay` | yes | 0.1.0 |  |  |  | never-published |
| cargo | `rightkit-pdf` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-playback` | yes | 0.1.1 | 0.1.1 | 0.1.1 | 0.1.1 | ok |
| cargo | `rightkit-process` | yes | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | ok |
| cargo | `rightkit-qa` | yes | 0.2.13 | 0.2.12 | 0.2.12 | 0.2.13 | source-unpublished |
| cargo | `rightkit-qts` | yes | 0.2.1 | 0.2.1 | 0.2.1 | 0.2.1 | ok |
| cargo | `rightkit-qts-ggml` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-qts-ggml-sys` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-review` | no | 0.1.0 |  |  |  | private-crate |
| cargo | `rightkit-sandbox` | yes | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | ok |
| cargo | `rightkit-scenario` | no | 0.0.0 |  |  |  | private-crate |
| cargo | `rightkit-search` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-secrets` | yes | 0.1.1 | 0.1.1 | 0.1.1 | 0.1.1 | ok |
| cargo | `rightkit-service` | yes | 0.1.1 | 0.1.1 | 0.1.1 | 0.1.1 | ok |
| cargo | `rightkit-settings` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| cargo | `rightkit-shell` | yes | 0.1.4 | 0.1.4 | 0.1.4 | 0.1.4 | ok |
| cargo | `rightkit-speech-client` | yes | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | ok |
| cargo | `rightkit-sync` | yes | 0.1.0 |  |  | 0.1.0 | never-published |
| cargo | `rightkit-tauri` | yes | 0.1.1 | 0.1.1 | 0.1.1 | 0.1.1 | ok |
| cargo | `rightkit-transcript` | no | 0.0.0 |  |  |  | private-crate |
| cargo | `rightkit-tts` | no | 0.0.0 |  |  |  | private-crate |
| cargo | `rightkit-vad` | no | 0.0.2 |  |  |  | private-crate |
| cargo | `rightkit-watch` | yes | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | ok |
| npm | `@rightkit/agent-room-mcp` | no | 0.1.0 |  |  |  | private-package |
| npm | `@rightkit/app-shell` | yes | 0.2.1 | 0.2.1 | 0.2.1 | 0.2.1 | ok |
| npm | `@rightkit/ax` | yes | 0.2.4 | 0.2.4 | 0.2.4 | 0.2.4 | ok |
| npm | `@rightkit/doc-render` | yes | 0.1.3 | 0.1.3 | 0.1.3 | 0.1.3 | ok |
| npm | `@rightkit/git` | yes | 0.2.26 | 0.2.26 | 0.2.26 | 0.2.26 | ok |
| npm | `@rightkit/hooks` | yes | 0.1.2 | 0.1.2 | 0.1.2 | 0.1.2 | ok |
| npm | `@rightkit/i18n` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| npm | `@rightkit/legal` | yes | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | ok |
| npm | `@rightkit/legal-ui` | yes | 0.1.3 | 0.1.3 | 0.1.3 | 0.1.3 | ok |
| npm | `@rightkit/license` | yes | 0.1.8 | 0.1.8 | 0.1.8 | 0.1.8 | ok |
| npm | `@rightkit/logs` | yes | 0.1.4 | 0.1.4 | 0.1.4 | 0.1.4 | ok |
| npm | `@rightkit/media-ui` | yes | 0.2.6 | 0.2.6 | 0.2.6 | 0.2.6 | ok |
| npm | `@rightkit/onboarding` | yes | 0.2.3 | 0.2.3 | 0.2.3 | 0.2.3 | ok |
| npm | `@rightkit/pdf-ui` | yes | 0.1.2 | 0.1.2 | 0.1.2 | 0.1.2 | ok |
| npm | `@rightkit/platform-ui` | yes | 0.1.2 | 0.1.2 | 0.1.2 | 0.1.2 | ok |
| npm | `@rightkit/qa` | yes | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | ok |
| npm | `@rightkit/qa-chrome` | yes | 0.1.3 | 0.1.3 | 0.1.3 | 0.1.3 | ok |
| npm | `@rightkit/release` | yes | 0.2.135 | 0.2.135 | 0.2.135 | 0.2.135 | ok |
| npm | `@rightkit/settings-ui` | yes | 0.2.7 | 0.2.7 | 0.2.7 | 0.2.7 | ok |
| npm | `@rightkit/shell` | yes | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | ok |
| npm | `@rightkit/tauri` | yes | 0.1.1 | 0.1.1 | 0.1.1 | 0.1.1 | ok |
| npm | `@rightkit/theme` | yes | 0.2.1 | 0.2.1 | 0.2.1 | 0.2.1 | ok |
| npm | `@rightkit/updates` | yes | 0.2.5 | 0.2.5 | 0.2.5 | 0.2.5 | ok |
| swift | `RightKitSwift` | yes | 0.3.8 |  | 0.1.1 | 0.3.1 | unverified |

## 2. Account parity

Crates owned by the `rightkit-fs` owner: 34. npm @rightkit packages listed: 22.

## 2. Account parity

| Severity | Subject | Kind | Detail |
| --- | --- | --- | --- |
| info | npm | unverified | scope:rightkit search returned nothing; listed via %40rightkit instead |
| action | crate rightkit-hotkeys | missing-from-registry | not listed under our account; never published |
| action | crate rightkit-llm | missing-from-registry | not listed under our account; never published |
| action | crate rightkit-overlay | missing-from-registry | not listed under our account; never published |
| action | crate rightkit-sync | missing-from-registry | not listed under our account; never published |

## 3. Third-party requirements

Declarations checked: 137. Unique external packages: 114.

## 3. Third-party requirements

| Severity | Subject | Kind | Detail |
| --- | --- | --- | --- |
| action | cargo rand | outdated-requirement | "0.9" in crates/agent-room-core/Cargo.toml excludes latest 0.10.3 (minor behind) |
| action | cargo sha2 | outdated-requirement | "0.10" in crates/agent-room-core/Cargo.toml, crates/rightkit-jobs/Cargo.toml, crates/rightkit-mcp/Cargo.toml, crates/rightkit-ocr/Cargo.toml, crates/rightkit-qa/Cargo.toml, crates/rightkit-tts/Cargo.toml excludes latest 0.11.0 (minor behind) |
| action | cargo sha2 | outdated-requirement | "0.10.9" in crates/rightkit-embed/Cargo.toml, crates/rightkit-http/Cargo.toml, crates/rightkit-oauth/Cargo.toml excludes latest 0.11.0 (minor behind) |
| action | cargo sha2 | outdated-requirement | "=0.10.9" in crates/rightkit-media/Cargo.toml, crates/rightkit-playback/Cargo.toml excludes latest 0.11.0 (minor behind) |
| info | cargo ort | unverified | no stable version found on the registry (prerelease or empty) |
| action | cargo tokenizers | outdated-requirement | "0.22.2" in crates/rightkit-asr/Cargo.toml, crates/rightkit-embed/Cargo.toml excludes latest 0.23.2 (minor behind) |
| action | cargo windows-core | outdated-requirement | "0.62.2" in crates/rightkit-audio/Cargo.toml excludes latest 0.100.0 (minor behind) |
| action | cargo windows-implement | outdated-requirement | "0.60" in crates/rightkit-audio/Cargo.toml excludes latest 0.100.0 (minor behind) |
| action | cargo webview2-com | outdated-requirement | "0.38" in crates/rightkit-control/Cargo.toml, crates/rightkit-shell/Cargo.toml excludes latest 0.39.1 (minor behind) |
| action | cargo zip | outdated-requirement | "2" in crates/rightkit-crash/Cargo.toml, crates/rightkit-models/Cargo.toml excludes latest 8.6.0 (major behind) |
| action | cargo nix | outdated-requirement | "=0.30.1" in crates/rightkit-fs/Cargo.toml, crates/rightkit-jobs/Cargo.toml, crates/rightkit-process/Cargo.toml excludes latest 0.31.3 (minor behind) |
| action | cargo getrandom | outdated-requirement | "0.3" in crates/rightkit-http/Cargo.toml, crates/rightkit-mcp/Cargo.toml, crates/rightkit-models/Cargo.toml, crates/rightkit-oauth/Cargo.toml, crates/rightkit-secrets/Cargo.toml excludes latest 0.4.3 (minor behind) |
| action | cargo getrandom | outdated-requirement | "=0.2.17" in crates/rightkit-tauri/Cargo.toml excludes latest 0.4.3 (minor behind) |
| action | cargo ed25519-dalek | outdated-requirement | "2" in crates/rightkit-license/Cargo.toml, crates/rightkit-models/Cargo.toml excludes latest 3.0.0 (major behind) |
| action | cargo base64 | outdated-requirement | "0.22" in crates/rightkit-license/Cargo.toml, crates/rightkit-models/Cargo.toml, crates/rightkit-oauth/Cargo.toml, crates/rightkit-ocr/Cargo.toml, crates/rightkit-secrets/Cargo.toml excludes latest 0.23.1 (minor behind) |
| action | cargo windows | outdated-requirement | "0.61" in crates/rightkit-automation/Cargo.toml, crates/rightkit-control/Cargo.toml, crates/rightkit-hotkeys/Cargo.toml, crates/rightkit-process/Cargo.toml, crates/rightkit-qa/Cargo.toml, crates/rightkit-sandbox/Cargo.toml, crates/rightkit-service/Cargo.toml, crates/rightkit-shell/Cargo.toml, crates/rightkit-speech-client/Cargo.toml excludes latest 0.62.2 (minor behind) |
| action | cargo lopdf | outdated-requirement | "0.44.0" in crates/rightkit-pdf/Cargo.toml excludes latest 0.45.0 (minor behind) |
| action | cargo trybuild | outdated-requirement | "=1.0.116" in crates/rightkit-process/Cargo.toml excludes latest 1.0.122 (patch behind) |
| action | cargo toml | outdated-requirement | "0.8" in crates/rightkit-qa/Cargo.toml excludes latest 1.1.7+spec-1.1.0 (major behind) |
| action | cargo reqwest | outdated-requirement | "0.12" in crates/rightkit-mcp/Cargo.toml excludes latest 0.13.5 (minor behind) |
| action | cargo prost-build | outdated-requirement | "0.13" in crates/rightkit-qts/Cargo.toml excludes latest 0.14.4 (minor behind) |
| action | cargo bindgen | outdated-requirement | "0.72" in crates/rightkit-qts-ggml-sys/Cargo.toml excludes latest 0.73.2 (minor behind) |
| action | cargo tauri-winrt-notification | outdated-requirement | "0.7" in crates/rightkit-shell/Cargo.toml excludes latest 0.8.1 (minor behind) |
| action | npm katex | outdated-requirement | "^0.18.7" in packages/doc-render/package.json excludes latest 0.19.0 (minor behind) |
| action | npm vitest | outdated-requirement | "^2.1.8" in packages/app-shell/package.json, packages/doc-render/package.json, packages/i18n/package.json, packages/legal-ui/package.json, packages/legal/package.json, packages/license/package.json, packages/logs/package.json, packages/media-ui/package.json, packages/onboarding/package.json, packages/pdf-ui/package.json, packages/platform-ui/package.json, packages/qa-chrome/package.json, packages/qa/package.json, packages/settings-ui/package.json, packages/tauri/package.json, packages/theme/package.json, packages/updates/package.json excludes latest 5.0.3 (major behind) |
| action | npm mermaid | outdated-requirement | "^11.17.2" in packages/doc-render/package.json excludes latest 12.1.0 (major behind) |
| action | npm @testing-library/jest-dom | outdated-requirement | "^6.6.3" in packages/i18n/package.json, packages/legal-ui/package.json, packages/onboarding/package.json, packages/settings-ui/package.json excludes latest 7.0.1 (major behind) |
| action | npm vite | outdated-requirement | "^6.0.0" in packages/app-shell/package.json, packages/onboarding/package.json, packages/settings-ui/package.json excludes latest 8.3.4 (major behind) |
| action | npm jsdom | outdated-requirement | "^25.0.1" in packages/i18n/package.json, packages/legal-ui/package.json, packages/media-ui/package.json, packages/onboarding/package.json, packages/settings-ui/package.json excludes latest 30.1.2 (major behind) |
| action | npm smol-toml | outdated-requirement | "1.7.0" in packages/release/package.json excludes latest 1.9.0 (minor behind) |
| action | npm @types/node | outdated-requirement | "^22.10.0" in packages/legal/package.json, packages/license/package.json excludes latest 26.6.4 (major behind) |
| action | npm @types/node | outdated-requirement | "^24.0.0" in packages/qa-chrome/package.json, packages/qa/package.json, packages/theme/package.json excludes latest 26.6.4 (major behind) |
| action | npm typescript | outdated-requirement | "^5.7.0" in packages/app-shell/package.json, packages/doc-render/package.json, packages/i18n/package.json, packages/legal-ui/package.json, packages/legal/package.json, packages/license/package.json, packages/logs/package.json, packages/media-ui/package.json, packages/onboarding/package.json, packages/pdf-ui/package.json, packages/platform-ui/package.json, packages/qa-chrome/package.json, packages/qa/package.json, packages/settings-ui/package.json, packages/shell/package.json, packages/tauri/package.json, packages/theme/package.json, packages/updates/package.json excludes latest 7.0.2 (major behind) |
| action | npm axe-core | outdated-requirement | "4.12.1" in packages/legal-ui/package.json excludes latest 4.14.0 (minor behind) |
| info | crate aes | duplicate-lock-versions | Cargo.lock resolves 0.8.4, 0.9.3 |
| info | crate base64 | duplicate-lock-versions | Cargo.lock resolves 0.13.1, 0.21.7, 0.22.1, 0.23.1 |
| info | crate bitflags | duplicate-lock-versions | Cargo.lock resolves 1.3.2, 2.13.0 |
| info | crate block-buffer | duplicate-lock-versions | Cargo.lock resolves 0.10.4, 0.12.1 |
| info | crate block-padding | duplicate-lock-versions | Cargo.lock resolves 0.3.3, 0.4.2 |
| info | crate cbc | duplicate-lock-versions | Cargo.lock resolves 0.1.2, 0.2.1 |
| info | crate cfg_aliases | duplicate-lock-versions | Cargo.lock resolves 0.1.1, 0.2.1 |
| info | crate cipher | duplicate-lock-versions | Cargo.lock resolves 0.4.4, 0.5.2 |
| info | crate const-oid | duplicate-lock-versions | Cargo.lock resolves 0.10.2, 0.9.6 |
| info | crate cpufeatures | duplicate-lock-versions | Cargo.lock resolves 0.2.17, 0.3.1 |
| info | crate crypto-common | duplicate-lock-versions | Cargo.lock resolves 0.1.7, 0.2.2 |
| info | crate darling | duplicate-lock-versions | Cargo.lock resolves 0.20.11, 0.23.0 |
| info | crate darling_core | duplicate-lock-versions | Cargo.lock resolves 0.20.11, 0.23.0 |
| info | crate darling_macro | duplicate-lock-versions | Cargo.lock resolves 0.20.11, 0.23.0 |
| info | crate der | duplicate-lock-versions | Cargo.lock resolves 0.7.10, 0.8.1 |
| info | crate digest | duplicate-lock-versions | Cargo.lock resolves 0.10.7, 0.11.3 |
| info | crate dirs | duplicate-lock-versions | Cargo.lock resolves 6.0.0, 7.0.0 |
| info | crate ecb | duplicate-lock-versions | Cargo.lock resolves 0.1.2, 0.2.1 |
| info | crate euclid | duplicate-lock-versions | Cargo.lock resolves 0.20.14, 0.22.14 |
| info | crate foreign-types | duplicate-lock-versions | Cargo.lock resolves 0.3.2, 0.5.0 |
| info | crate foreign-types-shared | duplicate-lock-versions | Cargo.lock resolves 0.1.1, 0.3.1 |
| info | crate getrandom | duplicate-lock-versions | Cargo.lock resolves 0.2.17, 0.3.4, 0.4.3 |
| info | crate hashbrown | duplicate-lock-versions | Cargo.lock resolves 0.12.3, 0.16.1, 0.17.1 |
| info | crate heck | duplicate-lock-versions | Cargo.lock resolves 0.4.1, 0.5.0 |
| info | crate indexmap | duplicate-lock-versions | Cargo.lock resolves 1.9.3, 2.14.0 |
| info | crate inout | duplicate-lock-versions | Cargo.lock resolves 0.1.4, 0.2.2 |
| info | crate itertools | duplicate-lock-versions | Cargo.lock resolves 0.13.0, 0.14.0 |
| info | crate jni | duplicate-lock-versions | Cargo.lock resolves 0.21.1, 0.22.4 |
| info | crate jni-sys | duplicate-lock-versions | Cargo.lock resolves 0.3.1, 0.4.1 |
| info | crate libloading | duplicate-lock-versions | Cargo.lock resolves 0.7.4, 0.8.9, 0.9.0 |
| info | crate lopdf | duplicate-lock-versions | Cargo.lock resolves 0.42.0, 0.44.0 |
| info | crate md-5 | duplicate-lock-versions | Cargo.lock resolves 0.10.6, 0.11.0 |
| info | crate nix | duplicate-lock-versions | Cargo.lock resolves 0.28.0, 0.30.1 |
| info | crate nom | duplicate-lock-versions | Cargo.lock resolves 7.1.3, 8.0.0 |
| info | crate png | duplicate-lock-versions | Cargo.lock resolves 0.17.16, 0.18.1 |
| info | crate proc-macro-crate | duplicate-lock-versions | Cargo.lock resolves 1.3.1, 2.0.2 |
| info | crate prost | duplicate-lock-versions | Cargo.lock resolves 0.13.5, 0.14.4 |
| info | crate prost-derive | duplicate-lock-versions | Cargo.lock resolves 0.13.5, 0.14.4 |
| info | crate r-efi | duplicate-lock-versions | Cargo.lock resolves 5.3.0, 6.0.0 |
| info | crate rand | duplicate-lock-versions | Cargo.lock resolves 0.10.3, 0.9.5 |
| info | crate rand_core | duplicate-lock-versions | Cargo.lock resolves 0.10.1, 0.6.4, 0.9.5 |
| info | crate reqwest | duplicate-lock-versions | Cargo.lock resolves 0.12.28, 0.13.5 |
| info | crate schemars | duplicate-lock-versions | Cargo.lock resolves 0.8.22, 0.9.0, 1.2.2 |
| info | crate serde_spanned | duplicate-lock-versions | Cargo.lock resolves 0.6.9, 1.1.1 |
| info | crate sha2 | duplicate-lock-versions | Cargo.lock resolves 0.10.9, 0.11.0 |
| info | crate shlex | duplicate-lock-versions | Cargo.lock resolves 1.3.0, 2.0.1 |
| info | crate syn | duplicate-lock-versions | Cargo.lock resolves 1.0.109, 2.0.118, 3.0.6 |
| info | crate thiserror | duplicate-lock-versions | Cargo.lock resolves 1.0.69, 2.0.21 |
| info | crate thiserror-impl | duplicate-lock-versions | Cargo.lock resolves 1.0.69, 2.0.21 |
| info | crate toml | duplicate-lock-versions | Cargo.lock resolves 0.8.2, 0.9.12+spec-1.1.0, 1.1.2+spec-1.1.0 |
| info | crate toml_datetime | duplicate-lock-versions | Cargo.lock resolves 0.6.3, 0.7.5+spec-1.1.0, 1.1.1+spec-1.1.0 |
| info | crate toml_edit | duplicate-lock-versions | Cargo.lock resolves 0.19.15, 0.20.2 |
| info | crate wasm-streams | duplicate-lock-versions | Cargo.lock resolves 0.4.2, 0.5.0 |
| info | crate webview2-com | duplicate-lock-versions | Cargo.lock resolves 0.38.2, 0.39.1 |
| info | crate webview2-com-sys | duplicate-lock-versions | Cargo.lock resolves 0.38.2, 0.39.1 |
| info | crate weezl | duplicate-lock-versions | Cargo.lock resolves 0.1.12, 0.2.1 |
| info | crate windows | duplicate-lock-versions | Cargo.lock resolves 0.61.1, 0.62.2 |
| info | crate windows_aarch64_gnullvm | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows_aarch64_msvc | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows_i686_gnu | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows_i686_gnullvm | duplicate-lock-versions | Cargo.lock resolves 0.52.6, 0.53.1 |
| info | crate windows_i686_msvc | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows_x86_64_gnu | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows_x86_64_gnullvm | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows_x86_64_msvc | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.1 |
| info | crate windows-collections | duplicate-lock-versions | Cargo.lock resolves 0.2.0, 0.3.2 |
| info | crate windows-core | duplicate-lock-versions | Cargo.lock resolves 0.61.2, 0.62.2 |
| info | crate windows-future | duplicate-lock-versions | Cargo.lock resolves 0.2.1, 0.3.2 |
| info | crate windows-link | duplicate-lock-versions | Cargo.lock resolves 0.1.3, 0.2.1 |
| info | crate windows-numerics | duplicate-lock-versions | Cargo.lock resolves 0.2.0, 0.3.1 |
| info | crate windows-result | duplicate-lock-versions | Cargo.lock resolves 0.3.4, 0.4.1 |
| info | crate windows-strings | duplicate-lock-versions | Cargo.lock resolves 0.4.2, 0.5.1 |
| info | crate windows-sys | duplicate-lock-versions | Cargo.lock resolves 0.45.0, 0.52.0, 0.59.0, 0.60.2, 0.61.2 |
| info | crate windows-targets | duplicate-lock-versions | Cargo.lock resolves 0.42.2, 0.52.6, 0.53.5 |
| info | crate windows-threading | duplicate-lock-versions | Cargo.lock resolves 0.1.0, 0.2.1 |
| info | crate winnow | duplicate-lock-versions | Cargo.lock resolves 0.5.40, 0.7.15, 1.0.4 |
| info | crate winreg | duplicate-lock-versions | Cargo.lock resolves 0.10.1, 0.55.0 |

Multiple versions in the root Cargo.lock:

- `aes`: 0.8.4, 0.9.3
- `base64`: 0.13.1, 0.21.7, 0.22.1, 0.23.1
- `bitflags`: 1.3.2, 2.13.0
- `block-buffer`: 0.10.4, 0.12.1
- `block-padding`: 0.3.3, 0.4.2
- `cbc`: 0.1.2, 0.2.1
- `cfg_aliases`: 0.1.1, 0.2.1
- `cipher`: 0.4.4, 0.5.2
- `const-oid`: 0.10.2, 0.9.6
- `cpufeatures`: 0.2.17, 0.3.1
- `crypto-common`: 0.1.7, 0.2.2
- `darling`: 0.20.11, 0.23.0
- `darling_core`: 0.20.11, 0.23.0
- `darling_macro`: 0.20.11, 0.23.0
- `der`: 0.7.10, 0.8.1
- `digest`: 0.10.7, 0.11.3
- `dirs`: 6.0.0, 7.0.0
- `ecb`: 0.1.2, 0.2.1
- `euclid`: 0.20.14, 0.22.14
- `foreign-types`: 0.3.2, 0.5.0
- `foreign-types-shared`: 0.1.1, 0.3.1
- `getrandom`: 0.2.17, 0.3.4, 0.4.3
- `hashbrown`: 0.12.3, 0.16.1, 0.17.1
- `heck`: 0.4.1, 0.5.0
- `indexmap`: 1.9.3, 2.14.0
- `inout`: 0.1.4, 0.2.2
- `itertools`: 0.13.0, 0.14.0
- `jni`: 0.21.1, 0.22.4
- `jni-sys`: 0.3.1, 0.4.1
- `libloading`: 0.7.4, 0.8.9, 0.9.0
- `lopdf`: 0.42.0, 0.44.0
- `md-5`: 0.10.6, 0.11.0
- `nix`: 0.28.0, 0.30.1
- `nom`: 7.1.3, 8.0.0
- `png`: 0.17.16, 0.18.1
- `proc-macro-crate`: 1.3.1, 2.0.2
- `prost`: 0.13.5, 0.14.4
- `prost-derive`: 0.13.5, 0.14.4
- `r-efi`: 5.3.0, 6.0.0
- `rand`: 0.10.3, 0.9.5
- `rand_core`: 0.10.1, 0.6.4, 0.9.5
- `reqwest`: 0.12.28, 0.13.5
- `schemars`: 0.8.22, 0.9.0, 1.2.2
- `serde_spanned`: 0.6.9, 1.1.1
- `sha2`: 0.10.9, 0.11.0
- `shlex`: 1.3.0, 2.0.1
- `syn`: 1.0.109, 2.0.118, 3.0.6
- `thiserror`: 1.0.69, 2.0.21
- `thiserror-impl`: 1.0.69, 2.0.21
- `toml`: 0.8.2, 0.9.12+spec-1.1.0, 1.1.2+spec-1.1.0
- `toml_datetime`: 0.6.3, 0.7.5+spec-1.1.0, 1.1.1+spec-1.1.0
- `toml_edit`: 0.19.15, 0.20.2
- `wasm-streams`: 0.4.2, 0.5.0
- `webview2-com`: 0.38.2, 0.39.1
- `webview2-com-sys`: 0.38.2, 0.39.1
- `weezl`: 0.1.12, 0.2.1
- `windows`: 0.61.1, 0.62.2
- `windows_aarch64_gnullvm`: 0.42.2, 0.52.6, 0.53.1
- `windows_aarch64_msvc`: 0.42.2, 0.52.6, 0.53.1
- `windows_i686_gnu`: 0.42.2, 0.52.6, 0.53.1
- `windows_i686_gnullvm`: 0.52.6, 0.53.1
- `windows_i686_msvc`: 0.42.2, 0.52.6, 0.53.1
- `windows_x86_64_gnu`: 0.42.2, 0.52.6, 0.53.1
- `windows_x86_64_gnullvm`: 0.42.2, 0.52.6, 0.53.1
- `windows_x86_64_msvc`: 0.42.2, 0.52.6, 0.53.1
- `windows-collections`: 0.2.0, 0.3.2
- `windows-core`: 0.61.2, 0.62.2
- `windows-future`: 0.2.1, 0.3.2
- `windows-link`: 0.1.3, 0.2.1
- `windows-numerics`: 0.2.0, 0.3.1
- `windows-result`: 0.3.4, 0.4.1
- `windows-strings`: 0.4.2, 0.5.1
- `windows-sys`: 0.45.0, 0.52.0, 0.59.0, 0.60.2, 0.61.2
- `windows-targets`: 0.42.2, 0.52.6, 0.53.5
- `windows-threading`: 0.1.0, 0.2.1
- `winnow`: 0.5.40, 0.7.15, 1.0.4
- `winreg`: 0.10.1, 0.55.0

## 4. App adoption

| App | Present | Duplicate copies in lock files |
| --- | --- | ---: |
| cutright | yes | 1 |
| genright | yes | 0 |
| scraperight | yes | 1 |
| viewright | yes | 1 |
| heardright | yes | 1 |
| coderight | yes | 4 |
| mailright | yes | 1 |
| postright | yes | 0 |
| cockpit | yes | 0 |
| workright | yes | 0 |
| sellright | yes | 0 |
| rightsites | yes | 0 |
| voiceright | yes | 0 |
| legion | yes | 0 |

## 4. App adoption

| Severity | Subject | Kind | Detail |
| --- | --- | --- | --- |
| action | rightkit-control (cockpit) | outdated-pin | cockpit/hub/src-tauri/Cargo.toml pins "=0.1.6"; latest published 0.1.9 |
| action | rightkit-db (coderight) | outdated-pin | coderight/context/engine/crates/cortex-store/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (coderight) | outdated-pin | coderight/context/engine/crates/cortex/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (coderight) | outdated-pin | coderight/context/engine/crates/membrane-blueprint/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (coderight) | outdated-pin | coderight/context/engine/crates/membrane-runtime/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (coderight) | outdated-pin | coderight/context/engine/crates/membrane-transcript/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.2.1"; latest published 0.3.1 |
| action | rightkit-db (genright) | outdated-pin | genright/crates/genright-gen/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (genright) | outdated-pin | genright/crates/genright-local-image/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-db (mailright) | outdated-pin | mailright/src-tauri/Cargo.toml pins "=0.2.1"; latest published 0.3.1 |
| action | rightkit-db (scraperight) | outdated-pin | scraperight/crates/store/Cargo.toml pins "=0.2.1"; latest published 0.3.1 |
| action | rightkit-db (viewright) | outdated-pin | viewright/src-tauri/Cargo.toml pins "=0.2.1"; latest published 0.3.1 |
| action | rightkit-embed (mailright) | outdated-pin | mailright/src-tauri/Cargo.toml pins "=0.2.1"; latest published 0.2.2 |
| action | rightkit-fs (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-http (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-http (mailright) | outdated-pin | mailright/src-tauri/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-jobs (cutright) | outdated-pin | cutright/crates/cutright-engine/Cargo.toml pins "=0.2.1"; latest published 0.2.4 |
| action | rightkit-jobs (genright) | outdated-pin | genright/crates/genright-gen/Cargo.toml pins "=0.2.2"; latest published 0.2.4 |
| action | rightkit-jobs (genright) | outdated-pin | genright/crates/genright-local-image/Cargo.toml pins "=0.2.2"; latest published 0.2.4 |
| action | rightkit-logs (heardright) | outdated-pin | heardright/tauri-app-next/heardright-telemetry/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-logs (heardright) | outdated-pin | heardright/tauri-app-next/src-tauri/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-logs (scraperight) | outdated-pin | scraperight/src-tauri/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-logs (viewright) | outdated-pin | viewright/src-tauri/Cargo.toml pins "=0.1.0"; latest published 0.2.0 |
| action | rightkit-mcp (coderight) | outdated-pin | coderight/context/engine/crates/membrane-mcp/Cargo.toml pins "=0.3.1"; latest published 0.4.0 |
| action | rightkit-mcp (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.3.1"; latest published 0.4.0 |
| action | rightkit-models (cutright) | outdated-pin | cutright/crates/cutright-model-store/Cargo.toml pins "=0.2.1"; latest published 0.2.3 |
| action | rightkit-models (genright) | outdated-pin | genright/crates/genright-local-image/Cargo.toml pins "=0.2.2"; latest published 0.2.3 |
| action | rightkit-models (genright) | outdated-pin | genright/crates/genright-protocol/Cargo.toml pins "=0.2.2"; latest published 0.2.3 |
| action | rightkit-models (genright) | outdated-pin | genright/crates/genright-qts/Cargo.toml pins "=0.2.2"; latest published 0.2.3 |
| action | rightkit-models (genright) | outdated-pin | genright/crates/genright-studio/Cargo.toml pins "=0.2.2"; latest published 0.2.3 |
| action | rightkit-oauth (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.2.0"; latest published 0.3.0 |
| action | rightkit-oauth (mailright) | outdated-pin | mailright/src-tauri/Cargo.toml pins "=0.2.0"; latest published 0.3.0 |
| action | rightkit-ort (coderight) | outdated-pin | coderight/apps/coderight-tauri/src-tauri/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (coderight) | outdated-pin | coderight/context/engine/crates/membrane-blueprint/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (coderight) | outdated-pin | coderight/context/engine/crates/membrane-runtime/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (cutright) | outdated-pin | cutright/crates/cutright-reframe/Cargo.toml pins "=0.1.4"; latest published 0.2.1 |
| action | rightkit-ort (cutright) | outdated-pin | cutright/crates/cutright-speech/Cargo.toml pins "=0.1.4"; latest published 0.2.1 |
| action | rightkit-ort (genright) | outdated-pin | genright/crates/genright-e2e/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (genright) | outdated-pin | genright/crates/genright-engine/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (genright) | outdated-pin | genright/crates/genright-gen/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (genright) | outdated-pin | genright/crates/genright-qts/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (scraperight) | outdated-pin | scraperight/crates/engine/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (scraperight) | outdated-pin | scraperight/crates/ocr/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (scraperight) | outdated-pin | scraperight/crates/ort_common/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (scraperight) | outdated-pin | scraperight/crates/speech/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (scraperight) | outdated-pin | scraperight/src-tauri/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-ort (scraperight) | outdated-pin | scraperight/vendor/parakeet-rs/Cargo.toml pins "=0.2.0"; latest published 0.2.1 |
| action | rightkit-process (cutright) | outdated-pin | cutright/Cargo.toml pins "=0.1.0"; latest published 0.3.1 |
| action | rightkit-process (genright) | outdated-pin | genright/crates/genright-e2e/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-process (genright) | outdated-pin | genright/crates/genright-gen/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-process (genright) | outdated-pin | genright/crates/genright-local-image/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-process (genright) | outdated-pin | genright/crates/genright-qts/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-process (genright) | outdated-pin | genright/src-tauri/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-sandbox (coderight) | outdated-pin | coderight/engine/crates/tools/Cargo.toml pins "=0.2.4"; latest published 0.3.1 |
| action | rightkit-secrets (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-secrets (cutright) | outdated-pin | cutright/crates/cutright-shorts/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-secrets (genright) | outdated-pin | genright/crates/genright-gen/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-secrets (mailright) | outdated-pin | mailright/src-tauri/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-secrets (scraperight) | outdated-pin | scraperight/crates/engine/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-secrets (scraperight) | outdated-pin | scraperight/src-tauri/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-service (heardright) | outdated-pin | heardright/tauri-app-next/heardright-runtime/Cargo.toml pins "=0.1.0"; latest published 0.1.1 |
| action | rightkit-shell (coderight) | outdated-pin | coderight/apps/coderight-tauri/src-tauri/Cargo.toml pins "=0.1.2"; latest published 0.1.4 |
| action | rightkit-shell (mailright) | outdated-pin | mailright/src-tauri/Cargo.toml pins "=0.1.2"; latest published 0.1.4 |
| action | rightkit-shell (viewright) | outdated-pin | viewright/src-tauri/Cargo.toml pins "=0.1.3"; latest published 0.1.4 |
| action | rightkit-watch (coderight) | outdated-pin | coderight/context/engine/crates/membrane-blueprint/Cargo.toml pins "=0.3.0"; latest published 0.3.1 |
| action | rightkit-watch (coderight) | outdated-pin | coderight/engine/Cargo.toml pins "=0.1.0"; latest published 0.3.1 |
| action | @rightkit/app-shell (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.2.0"; latest published 0.2.1 |
| action | @rightkit/app-shell (heardright) | outdated-pin | heardright/tauri-app-next/package.json pins "0.1.0"; latest published 0.2.1 |
| action | @rightkit/app-shell (scraperight) | outdated-pin | scraperight/package.json pins "0.2.0"; latest published 0.2.1 |
| action | @rightkit/git (coderight) | outdated-pin | coderight/context/package.json pins "0.2.24"; latest published 0.2.26 |
| action | @rightkit/git (legion) | outdated-pin | legion/package.json pins "0.2.24"; latest published 0.2.26 |
| action | @rightkit/hooks (coderight) | outdated-pin | coderight/context/package.json pins "0.1.1"; latest published 0.1.2 |
| action | @rightkit/hooks (legion) | outdated-pin | legion/package.json pins "0.1.1"; latest published 0.1.2 |
| action | @rightkit/legal (scraperight) | outdated-pin | scraperight/package.json pins "0.3.0"; latest published 0.3.1 |
| action | @rightkit/license (scraperight) | outdated-pin | scraperight/package.json pins "0.1.6"; latest published 0.1.8 |
| action | @rightkit/logs (scraperight) | outdated-pin | scraperight/package.json pins "0.1.3"; latest published 0.1.4 |
| action | @rightkit/media-ui (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.2.0"; latest published 0.2.6 |
| action | @rightkit/media-ui (scraperight) | outdated-pin | scraperight/package.json pins "0.2.5"; latest published 0.2.6 |
| action | @rightkit/onboarding (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.2.0"; latest published 0.2.3 |
| action | @rightkit/onboarding (heardright) | outdated-pin | heardright/tauri-app-next/package.json pins "0.1.0"; latest published 0.2.3 |
| action | @rightkit/onboarding (scraperight) | outdated-pin | scraperight/package.json pins "0.2.2"; latest published 0.2.3 |
| action | @rightkit/platform-ui (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.1.1"; latest published 0.1.2 |
| action | @rightkit/qa-chrome (scraperight) | outdated-pin | scraperight/package.json pins "0.1.0"; latest published 0.1.3 |
| action | @rightkit/qa-chrome (viewright) | outdated-pin | viewright/package.json pins "0.1.2"; latest published 0.1.3 |
| action | @rightkit/release (coderight) | outdated-pin | coderight/apps/coderight-tauri/package.json pins "0.2.133"; latest published 0.2.135 |
| action | @rightkit/release (coderight) | outdated-pin | coderight/context/package.json pins "0.2.121"; latest published 0.2.135 |
| action | @rightkit/release (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.2.133"; latest published 0.2.135 |
| action | @rightkit/release (legion) | outdated-pin | legion/package.json pins "0.2.109"; latest published 0.2.135 |
| action | @rightkit/release (scraperight) | outdated-pin | scraperight/package.json pins "0.2.133"; latest published 0.2.135 |
| action | @rightkit/release (voiceright) | outdated-pin | voiceright/package.json pins "0.2.109"; latest published 0.2.135 |
| action | @rightkit/settings-ui (heardright) | outdated-pin | heardright/tauri-app-next/package.json pins "0.1.0"; latest published 0.2.7 |
| action | @rightkit/tauri (scraperight) | outdated-pin | scraperight/package.json pins "0.1.0"; latest published 0.1.1 |
| action | @rightkit/theme (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.2.0"; latest published 0.2.1 |
| action | @rightkit/theme (heardright) | outdated-pin | heardright/tauri-app-next/package.json pins "0.1.0"; latest published 0.2.1 |
| action | @rightkit/theme (scraperight) | outdated-pin | scraperight/package.json pins "0.2.0"; latest published 0.2.1 |
| action | @rightkit/updates (cutright) | outdated-pin | cutright/apps/cutright/package.json pins "0.2.4"; latest published 0.2.5 |
| action | rightkit-watch (coderight) | split-app-pins | cargo pins differ within one app: =0.1.0; =0.3.0 |
| action | @rightkit/release (coderight) | split-app-pins | npm pins differ within one app: 0.2.121; 0.2.133 |
| action | rightkit-process (cutright) | duplicate-app-copies | Cargo.lock resolves 0.2.1, 0.3.0 |
| action | @rightkit/updates (scraperight) | duplicate-app-copies | pnpm-lock.yaml resolves 0.2.3, 0.2.5 |
| action | @rightkit/updates (viewright) | duplicate-app-copies | pnpm-lock.yaml resolves 0.2.4, 0.2.5 |
| action | @rightkit/updates (heardright) | duplicate-app-copies | tauri-app-next/pnpm-lock.yaml resolves 0.2.4, 0.2.5 |
| action | rightkit-db (coderight) | duplicate-app-copies | engine/Cargo.lock resolves 0.2.1, 0.3.0 |
| action | rightkit-process (coderight) | duplicate-app-copies | engine/Cargo.lock resolves 0.2.1, 0.3.1 |
| action | rightkit-watch (coderight) | duplicate-app-copies | engine/Cargo.lock resolves 0.1.0, 0.3.0 |
| action | @rightkit/updates (coderight) | duplicate-app-copies | apps/coderight-tauri/pnpm-lock.yaml resolves 0.2.4, 0.2.5 |
| action | @rightkit/updates (mailright) | duplicate-app-copies | pnpm-lock.yaml resolves 0.2.4, 0.2.5 |

## 5. Generated docs

No findings.

## 6. New-package hygiene

| Severity | Subject | Kind | Detail |
| --- | --- | --- | --- |
| action | crate rightkit-channel-memory | missing-readme | no README.md |
| action | crate rightkit-review | missing-readme | no README.md |
| action | crate rightkit-scenario | missing-readme | no README.md |

