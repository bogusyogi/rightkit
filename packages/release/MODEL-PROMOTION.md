# Runtime artifact promotion

`right-release model promote --authority <app-key> --config <promotion.json>`
promotes one runtime artifact. Authority uses `RIGHT_SUITE_APP_KEYS` from
`runtime-artifact-manifest.mjs`: ViewRight, HeardRight, MailRight, ScrapeRight,
CodeRight, ScreenRight, GenRight & VoiceRight. Authority must equal
`manifest.entitlement.appKey`; unknown keys fail with
`RUNTIME_ARTIFACT_UNKNOWN_APP`, mismatches with
`RUNTIME_ARTIFACT_AUTHORITY_MISMATCH`. HeardRight uses `--authority heardright`
with `entitlement.appKey: "heardright"` as before.

## Config schema

JSON config has exactly `artifact`, `evidence` & `manifest`; unknown/missing
fields fail. `artifact.file` & `evidence.file` resolve relative to config.

| Object | Required fields |
| --- | --- |
| `artifact` | `file`, `filename` (safe basename), `sha256` (lowercase hex), `sizeBytes` (positive safe integer) |
| `evidence` | `file`, `sha256` (lowercase hex) |
| `manifest` | `artifactKind`, `entitlement`, `distribution`, `target`, `versions`, `provenance`, `promotion` |
| `entitlement` | `appKey`, `tier`: `pro`, `public`, or `bundled` |
| `distribution` | `delivery`: `private-r2`, `public-r2`, or `bundled`; `bucket`: `rightapps-updates`, `rightapps-downloads`, or `null`, respectively |
| `target` | `os`: `windows`, `darwin`, `linux`, or `ios`; `arch`: `x86_64`, `aarch64`, or `universal` |
| `versions` | `runtime`, `model`, `tokenizer`, `preprocessing`, `license`, `provenance`: exact nonempty strings |
| `provenance` | `source` (HTTPS), `sourceRevision`, `licenseId`, `noticeSha256` |
| `promotion` | `promotionId`, `promotedAt` (exact UTC ISO timestamp, including milliseconds) |

`manifest.artifactId` is optional. When absent, pointer paths keep legacy
layout: `<app>/runtime-artifacts/<kind>/<os>/<arch>/current/manifest.json`.
When present, it must match `^[a-z0-9]+(?:-[a-z0-9]+)*$` & be at most 128
characters; pointer paths include `<artifactId>` after `<kind>`. Use
`--artifact-id <slug>` to override config for one promotion. Immutable object
keys remain content-addressed by digest in both layouts.

Kinds: `asr-model`, `tts-model`, `ocr-model`, `ort-runtime`, `media-runtime`,
`tokenizer`, `preprocessing`. Model & tokenizer kinds require `pro` entitlement
with `private-r2` delivery. Public/bundled lanes require corresponding tiers.
`versions.license` must match `provenance.licenseId` after punctuation/case
normalization. CLI hashes both nonempty files, checks model size & digests,
then constructs schema-1 manifest with immutable object key, current pointer,
`promotion.authorityAppKey` & `promotion.evidenceSha256`. Do not put those
generated fields in config.

## Worked GenRight config

Save as `D:\Claude\genright\model-promotions\customvoice-talker.json`.
Filename, size & model digest below come from
`genright/crates/genright-qts/src/models.rs:311`, `MODE_1P7B_MANIFEST`.
Replace every `<...>` placeholder with recorded evidence before running.
Version/revision/license values must describe actual conversion & reviewed
upstream source; this example makes no claim that those reviews already exist.

```json
{
  "artifact": {
    "file": "D:/model-artifacts/qwen3-tts-1.7b-customvoice-talker-q8_0.gguf",
    "filename": "qwen3-tts-1.7b-customvoice-talker-q8_0.gguf",
    "sha256": "836dcede70c86c6db1873a21644cdf07dec30c09e4f33fe81b542de832efeace",
    "sizeBytes": 2154311328
  },
  "evidence": {
    "file": "customvoice-talker.evidence.json",
    "sha256": "<sha256-of-exact-evidence-file-bytes>"
  },
  "manifest": {
    "artifactKind": "tts-model",
    "entitlement": { "appKey": "genright", "tier": "pro" },
    "distribution": { "delivery": "private-r2", "bucket": "rightapps-updates" },
    "artifactId": "qwen3-tts-1p7b-customvoice-talker",
    "target": { "os": "any", "arch": "any" },
    "versions": {
      "runtime": "<qualified-rightkit-qts-version>",
      "model": "qwen3-tts-1.7b-customvoice-talker-q8_0",
      "tokenizer": "<paired-tokenizer-revision>",
      "preprocessing": "<qualified-preprocessing-version>",
      "license": "<reviewed-license-id>",
      "provenance": "<conversion-and-review-receipt-id>"
    },
    "provenance": {
      "source": "<https-upstream-source-url>",
      "sourceRevision": "<immutable-upstream-revision>",
      "licenseId": "<reviewed-license-id>",
      "noticeSha256": "<sha256-of-reviewed-notice-file-bytes>"
    },
    "promotion": {
      "promotionId": "gr-customvoice-talker-<unique-id>",
      "promotedAt": "<actual-promotion-time-in-YYYY-MM-DDTHH:mm:ss.sssZ>"
    }
  }
}
```

## Evidence file

CLI expects existing, nonempty `customvoice-talker.evidence.json` whose bytes
match `evidence.sha256`. It hashes evidence; it does **not** parse or certify an
evaluation schema. JSON below is recommended review content, not enforced input
fields or proof supplied by this change:

```json
{
  "appKey": "genright",
  "artifact": {
    "filename": "qwen3-tts-1.7b-customvoice-talker-q8_0.gguf",
    "sha256": "836dcede70c86c6db1873a21644cdf07dec30c09e4f33fe81b542de832efeace",
    "sizeBytes": 2154311328
  },
  "source": { "url": "<https-upstream-source-url>", "revision": "<pinned-revision>" },
  "conversion": { "tool": "rightkit-qts/scripts/export_1p7b.py", "commit": "<converter-commit>", "quantization": "q8_0" },
  "license": { "id": "<reviewed-license-id>", "noticeSha256": "<notice-file-sha256>" },
  "qualification": { "receipt": "<existing-load-and-synthesis-test-receipt>", "runtimeVersion": "<tested-runtime-version>", "pairedArtifactDigests": ["<predictor-sha256>", "<tokenizer-sha256>"] }
}
```

Compute hashes over final files, for example in PowerShell:

```powershell
(Get-FileHash D:\Claude\genright\model-promotions\customvoice-talker.evidence.json -Algorithm SHA256).Hash.ToLowerInvariant()
```

## Windows invocation & signing

Run live promotion on Windows, which defaults to protected PEM at
`%APPDATA%\RightKit\runtime-artifact-signing-key.pem`. Alternatively set
`RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE` to an existing protected PEM path.
No other platform has automatic PEM discovery. Do not move, copy, or print keys.
`--dry-run` still hashes files & signs using that key; it needs no release token
& invokes neither uploader nor registration. Live promotion requires durable
`RIGHTAPPS_RELEASE_TOKEN`/`RIGHTAPPS_RELEASE_TOKEN_FILE` (see `release-token.mjs`).

Both modes require clean GenRight checkout; keep model binaries ignored/outside
checkout & config/evidence in its reviewed clean source. Commands below use
updated RightKit source on Windows from GenRight's primary checkout, without
adding parent-path app dependencies or publishing/installing any package.
They select server's default `rightsites` store; use configured store slug if
different. After package adoption, `pnpm exec right-release` exposes same lane.
These commands are instructions only.

```sh
# Dry-run first, from Mac via Windows SSH bridge (PowerShell default shell).
ssh win 'Set-Location D:\Claude\genright; $env:RIGHTAPPS_STORE_SLUG = "rightsites"; node D:\Claude\rightkit\packages\release\cli\right-release.mjs model promote --authority genright --config model-promotions/customvoice-talker.json --dry-run'

# Live, after dry-run succeeds & exact promotion is authorized.
ssh win 'Set-Location D:\Claude\genright; $env:RIGHTAPPS_STORE_SLUG = "rightsites"; node D:\Claude\rightkit\packages\release\cli\right-release.mjs model promote --authority genright --config model-promotions/customvoice-talker.json'
```

## RightApps registration & six-file catalog

Shared client posts signed envelope to `/v1/admin/apps/runtime-artifacts`,
using `RIGHTAPPS_API_URL` (default `https://api.spoares.com`) &
`RIGHTAPPS_STORE_SLUG` (default `rightapps`). Workspace server implementation is
`rightsites/packages/api/src/rightsites-plugin/release-routes.ts`; suite schema
config is `rightsites/packages/api/src/licensing/suite-artifacts.ts`. Server
requires configured trusted Ed25519 public key/key ID, authenticated release
write access, same authority/entitlement app, app present in resolved store
catalog, & newer promotion time (same-envelope replay is idempotent).
Server `RIGHTSITES_RUNTIME_ARTIFACT_KEY_ID` must match CLI's
`rightkit-runtime-artifacts-2026-07`; `RIGHTSITES_RUNTIME_ARTIFACT_PUBLIC_KEY`
must match protected signing PEM. Client `RIGHTAPPS_STORE_SLUG` must match
server `RIGHTSITES_STORE_SLUG` for service-token writes; those defaults are
`rightapps` & `rightsites`, respectively. Set client slug to server's resolved
store before live promotion if defaults differ.

Server bucket defaults are `rightsites-updates` & `rightsites-downloads`, while
RightKit validates `rightapps-updates` & `rightapps-downloads`. Deployment must
already set `RIGHTSITES_R2_PRIVATE_BUCKET=rightapps-updates` &
`RIGHTSITES_R2_PUBLIC_BUCKET=rightapps-downloads` to accept RightKit envelopes.
No live server configuration, catalog entry, or signing key was inspected or
changed here. GenRight/VoiceRight & TTS support are source changes only.

Promotion order: immutable upload → signed-envelope registration → current
pointer replacement. Object key for this example is
`genright/runtime-artifacts/objects/sha256/836dcede70c86c6db1873a21644cdf07dec30c09e4f33fe81b542de832efeace/qwen3-tts-1.7b-customvoice-talker-q8_0.gguf`.
Pointer is `genright/runtime-artifacts/tts-model/qwen3-tts-1p7b-customvoice-talker/any/any/current/manifest.json`.

GenRight's six independent Qwen3 TTS slots use `artifactKind: "tts-model"`,
`target: { "os": "any", "arch": "any" }`, & these artifact IDs:

| Variant | Role | `artifactId` |
| --- | --- | --- |
| CustomVoice | talker | `qwen3-tts-1p7b-customvoice-talker` |
| CustomVoice | predictor | `qwen3-tts-1p7b-customvoice-predictor` |
| CustomVoice | tokenizer | `qwen3-tts-1p7b-customvoice-tokenizer` |
| VoiceDesign | talker | `qwen3-tts-1p7b-voicedesign-talker` |
| VoiceDesign | predictor | `qwen3-tts-1p7b-voicedesign-predictor` |
| VoiceDesign | tokenizer | `qwen3-tts-1p7b-voicedesign-tokenizer` |

Each slot has its own current pointer, so promotions do not overwrite another
role or variant. Bind six consumer catalog entries to immutable keys/digests,
retain signed envelopes/evidence, & use strictly increasing promotion
timestamps within each slot.
