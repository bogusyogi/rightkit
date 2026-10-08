# Promoted runtime artifact delivery — 2026-10-08

Implemented locally in RightKit & RightSites. No commit, publication, deployment,
production request or operational key inspection. Registry versions remain unchanged;
`rightapps` becomes available to registry consumers when this source is published.

## Findings

- HeardRight's source-wired TDT download path starts in
  [`download_shared_tdt`](../../../heardright/tauri-app-next/src-tauri/src/commands/models.rs:60).
  It posts `{route, model, platform, sha256, deviceId}` to
  `https://heardright-models.adrdsouza.workers.dev/v1/model-grants`.
  This requires no license: Worker rate-limits device/IP & accepts only its exact
  allowlisted tuple. It returns a ten-minute HMAC-SHA256 grant. App presents
  `Authorization: Bearer <grant>` to `/parakeet/macos/tdt.tar.gz` or
  `/parakeet/windows/tdt.tar.gz`; Worker maps tuple to private R2 key, verifies
  grant & streams Range-compatible bytes. App checks pinned archive SHA-256,
  extracts & activates via shared store. This path does **not** verify a promoted
  Ed25519 runtime envelope.
  Sources: [grant client](../crates/rightkit-models/src/suite/grant.rs:69),
  [Worker](../../../rightsites/workers/heardright-models/src/index.ts:277).
- License-gate separately accepts suite Ed25519 v2 Pro `signedToken` via Bearer
  or `?token=`, checks expiry/app prefix & cached revocations, then streams
  `GET https://<configured-license-gate-host>/<private-object-key>`. It resolves
  by key; no OS/arch promotion lookup or runtime-envelope verification existed
  there. Historical Whisper routes on heardright-models are removed.
- Before this change, promoted runtime envelopes were verified only on
  registration: `POST /v1/admin/apps/runtime-artifacts`, authenticated with
  dedicated release/admin authority & configured runtime Ed25519 public key/ID.
  No entitled API resolve/download-grant endpoint existed.
- Promotion rows now use **store/app/kind/artifactId/OS/arch**. Optional
  `artifactId` is an exact lowercase slug (`^[a-z0-9]+(?:-[a-z0-9]+)*$`,
  max 128 characters). Absence preserves legacy single-slot identity & signed
  manifest bytes; `null` is invalid in manifests. Immutable R2 objects still
  use **app/SHA-256/filename**.
- Schema has unique `runtime_artifact_promotion_current` constraint, despite
  its OS/arch fields being text. New journaled migration
  [`0102_runtime_artifact_identity.sql`](../../../rightsites/packages/api/drizzle/0102_runtime_artifact_identity.sql)
  adds nullable `artifact_id` & replaces slot uniqueness with PostgreSQL 15+
  `UNIQUE NULLS NOT DISTINCT` over all six dimensions. Legacy HeardRight &
  ScrapeRight rows, envelopes, pointers & IDs remain untouched. NULL keeps one
  legacy slot per store/app/kind/target.
  Source: [schema](../../../rightsites/packages/api/src/db/schema-licensing.ts:11).
- Paired **`any/any`** target serves concrete Mac/Windows targets from one slot.
  Six Qwen3 1.7B files now retain six independent `tts-model` slots, using
  IDs documented in [MODEL-PROMOTION.md](../packages/release/MODEL-PROMOTION.md:177).

## API & diff

New `POST /v1/apps/:appKey/runtime-artifacts/resolve` accepts:

```json
{"artifactId":"qwen3-tts-1p7b-customvoice-talker","target":{"os":"darwin","arch":"aarch64"}}
```

Use `Authorization: Bearer <signedToken>`, **not activationToken**. Send exactly
one selector: `artifactId` accepts an ID slug; backward-compatible `artifact`
accepts a slug, current row UUID or exact pointer. A scoped pointer is
`genright/runtime-artifacts/tts-model/qwen3-tts-1p7b-customvoice-talker/any/any/current/manifest.json`.
ID resolution prefers exact target, then `any/any`; an ID reused across kinds
at selected target fails closed, so use a pointer to disambiguate. Server
verifies token signature,
app/Pro/time, current store-scoped license status/expiry/tier, active device &
mobile/full scope. Model entitlement is independent of update-window expiry.
It re-verifies envelope signature & persisted identity, then returns
`{envelope, downloadUrl, expiresAt}`. URL carries a five-minute Ed25519 grant
scoped to one immutable R2 key, capped by token/license expiry. Worker checks
its signature, expiry, exact key/app & revocations before serving bytes.
Runtime grants fail closed without established revocation data.

RightSites changes: artifact-aware registration/upsert, NULL-safe migration,
resolve route/tests; paired wildcard validation; HTTPS
`RIGHTSITES_RUNTIME_ARTIFACT_DOWNLOAD_BASE_URL`; scoped Worker grants;
HEAD/Range/If-Range/206/416; no-store responses. No heardright-models edits.

RightKit changes: optional `rightapps` feature in existing published crate;
`RuntimeArtifactClient::{new,resolve,resolve_by_artifact_id,verify_envelope,install}`;
optional manifest `artifact_id`; legacy serialization compatibility; typed errors;
strict canonical Ed25519 verification; JS-generated test vectors; mock HTTP
resolve/download tests; grant-independent resume identity; redirect controls.
Store install retains existing locks, hash/size checks & atomic activation.
Raw files install under digest-named revisions. Archives remain raw artifacts.
`rightkit-license` is unchanged: it still owns offline license verification.

Trust ID mirrors [model-promote.mjs:19](../packages/release/model-promote.mjs:19):
`rightkit-runtime-artifacts-2026-07`. App pins its corresponding public key;
resolve never supplies a trusted key. Production public key was not inspected.

## Round 2 verification

- RightKit JS: `TMPDIR=/private/tmp node --test packages/release/runtime-artifact-manifest.test.mjs packages/release/model-promote.test.mjs` — **27 passed**.
- Rust: `rightkit cargo test -p rightkit-models --features rightapps` — **78 passed**;
  managed native macOS run, including signed artifact-ID & retained legacy vectors.
- RightSites focused manifest/registration/resolve unit tests — **45 passed**.
- Full API unit suite — **1107 passed, 66 skipped, 1 failed** (1174 total).
  Existing `telemetry.test.ts` Node listener fails with sandbox `listen EPERM`;
  same exception is reported once as an unhandled error.
- Worker — **38 passed** across 3 files. All six exact-key grants work; all
  30 cross-object substitutions fail before any R2 read. Worker TypeScript passes.
- Migration guard — **58 hand-written migrations verified**, including 12
  preserved legacy byte hashes. `git diff --check` passes in both scopes.
- API TypeScript reports existing `payments/sezzle.ts:84` TS2871 only.

Requested `pnpm --dir packages/api test`, `pnpm test:license-gate` & required
RightKit `pnpm test:release` were attempted; pnpm fails before runners with
`unable to open database file` in its SQLite store. Installed Vitest commands
ran equivalent requested API/Worker suites:

```sh
# From rightsites/packages/api
TMPDIR=/private/tmp ../../node_modules/.pnpm/node_modules/.bin/vitest run --project unit
# From rightsites
TMPDIR=/private/tmp node_modules/.pnpm/node_modules/.bin/vitest run --config workers/license-gate/vitest.config.ts
```

## GenRight adoption steps

1. Lead deploys API/Worker using ordered steps below. Registry adoption follows
   existing RightKit release lanes when separately requested; registry pins
   remain unchanged here.
2. Each of six files uses its own `manifest.artifactId`,
   `artifactKind: "tts-model"` & `target: { os: "any", arch: "any" }`. Run
   `right-release model promote --authority genright --config <config>` per
   reviewed file, or override ID with `--artifact-id <slug>`. Existing
   clean-source/evidence gates apply per promotion. Each creates independent
   pointer & row; object key remains content-addressed.
3. Pin exact registry version containing this change with
   `features = ["rightapps"]`. Embed provisioned runtime public key as raw
   `[u8; 32]`; pass signed license token from activation/refresh.
4. On a Tauri blocking worker, resolve native target & install:

```rust
use rightkit_models::{RuntimeArtifactClient, Store, InstallOptions};
use rightkit_models::rightapps::RuntimeArtifactTarget;

let client = RuntimeArtifactClient::new(api_base, runtime_public_key)?;
let artifact = client.resolve_by_artifact_id(
    "genright",
    "qwen3-tts-1p7b-customvoice-talker",
    signed_token,
    &RuntimeArtifactTarget::current(),
)?;
let store = Store::new(app_models_dir, "genright");
let installed = client.install(
    &artifact, &store, "qwen3-customvoice-talker",
    &InstallOptions::default(), |event| forward_progress(event),
)?;
let model_path = installed.dir.join(&artifact.envelope().manifest.object.filename);
```

5. After grant expiry, refresh license if needed, resolve again & install with
   same model ID. Same digest resumes retained partial bytes with strong ETag
   checks. Read verified `model_path` after atomic activation.

## Ordered lead deployment (instructions only)

1. Make reviewed source available on RightSites `origin/main` through lead's
   normal delivery process. This task leaves both checkouts uncommitted.
   Configure protected API environment by name only:
   `RIGHTSITES_RUNTIME_ARTIFACT_PUBLIC_KEY`, `RIGHTSITES_RUNTIME_ARTIFACT_KEY_ID`,
   `RIGHTSITES_RUNTIME_ARTIFACT_DOWNLOAD_BASE_URL`, `RIGHTSITES_RELEASE_TOKEN_SHA256`,
   `RIGHTSITES_STORE_SLUG`, `RIGHTSITES_R2_PRIVATE_BUCKET`, `RIGHTSITES_R2_PUBLIC_BUCKET`,
   `LICENSE_SIGNING_KEY` or `LICENSE_SIGNING_KEY_FILE`, `DATABASE_URL`,
   `DATABASE_URL_MIGRATE`. Runtime connection stays unprivileged; migration
   connection uses existing migration owner role. No key rotation is needed.
2. API on Hetzner, PM2 **`rightsites-api`**, primary checkout `~/sites/rightsites`.
   Connect from Mac with `ssh vendure-auto`; run in login shell (`bash -l`):

   ```sh
   set -e
   cd ~/sites/rightsites
   git pull --ff-only origin main
   test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
   pnpm --filter @sellright/api... build
   cd packages/api
   node --env-file=.env --input-type=module -e '
   const url = process.env.DATABASE_URL_MIGRATE;
   if (!url) throw new Error("DATABASE_URL_MIGRATE is required");
   process.env.DATABASE_URL = url;
   await import("./dist/scripts/migrate.js");'
   pm2 restart rightsites-api --update-env
   ```

   Migration invokes compiled entrypoint behind `pnpm --dir packages/api
   db:migrate:runtime`; explicit env loading avoids its DEV default & selects
   provisioned owner connection. Journal includes `0102_runtime_artifact_identity`.
   New uniqueness needs PostgreSQL 15+, supported by documented Postgres 17 host.
   Verify with read-only `GET https://heardright.app/v1/health` &
   `GET https://heardright.app/v1/readyz` (200). A resolve POST without license
   should return 403 `NOT_ENTITLED` & `Cache-Control: private, no-store`.
3. Deploy license-gate source to both existing Worker configurations from
   RightSites primary checkout, following
   [cloudflare-access.md](../../../docs/rules/cloudflare-access.md).
   Use provisioned `CLOUDFLARE_API_TOKEN` & `CLOUDFLARE_ACCOUNT_ID`; source
   Mac login config in this shell & strip deprecated auth vars:

   ```sh
   source ~/.zshrc
   unset CLOUDFLARE_API_KEY CLOUDFLARE_EMAIL CF_API_KEY CF_EMAIL
   cd /Volumes/D/claude/rightsites/workers/license-gate
   pnpm dlx wrangler@4 whoami
   pnpm dlx wrangler@4 deploy --config wrangler.toml
   pnpm dlx wrangler@4 deploy --config wrangler.legacy.toml
   ```

   `wrangler.toml` names **`rightsites-license-gate`**, binding `GATED` to
   `rightsites-updates`; legacy config names **`rightapps-license-gate`**,
   binding `GATED` to `rightapps-updates`. RightKit envelopes & uploader use
   `rightapps-updates`/`rightapps-downloads`, so API bucket configuration &
   download base must select matching compatibility bucket/Worker. Deploying
   only current Worker cannot serve RightKit's compatibility-bucket objects.
   Preserve both buckets & already-shipped client URLs.
4. Worker configuration names only: `GATED`, `REVOCATION_LIST_URL`.
   **No new Worker secret or runtime grant public-key variable is required**:
   API signs grants using existing license signer; both Workers verify embedded
   `LICENSE_PUBKEY`. Manifest public key is distinct & stays API/app-pinned.
   Cloudflare auth needs Workers Scripts Edit, User Details Read & Memberships Read.
   Read-only checks: `GET https://heardright.app/v1/pro/revocations` returns
   established revocation data; unauthenticated HEAD to each Worker object's
   route returns 401 with `private, no-store`.
5. Using existing active HeardRight/ScrapeRight Pro `signedToken`, POST its
   resolve endpoint with `artifactId: "round2-readonly-probe"` & concrete target.
   Expect 404 `NOT_FOUND`, proving new column/lookup works without a registration.
   Resolve existing legacy pointer with `artifact` to verify compatibility (200).
   Using existing active GenRight Pro `signedToken` & registered artifact IDs,
   POST `/v1/apps/genright/runtime-artifacts/resolve` for each of six IDs with
   concrete native target. Expect 200, matching signed `artifactId`/pointer &
   immutable key. HEAD returned `downloadUrl` within five minutes: expect 200,
   correct Content-Length/ETag & `private, no-store`; a one-byte GET with
   `Range: bytes=0-0` returns 206. Reuse grant against another immutable key:
   expect 403. Resolve existing HeardRight/ScrapeRight legacy pointers to verify
   compatibility. These requests read state & issue transient grants only;
   they never register, promote, or change licenses.
