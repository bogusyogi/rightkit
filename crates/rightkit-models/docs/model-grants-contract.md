# Model grants contract (`POST /v1/model-grants`)

Status of each statement is marked: **[existing]** is what `rightkit-models` does today
(`src/suite/grant.rs`, `src/suite/install.rs`, `src/download.rs`); **[proposal]** is what the
worker should do and the crate does not yet depend on.

**Worker status [existing source, deployment unverified].** The worker source exists at
`rightsites/workers/heardright-models/src/index.ts`. `issueModelGrant` (line 258) implements
`POST /v1/model-grants`, routed at line 545. It serves two Parakeet routes only (`MODEL_SPECS`,
lines 60-74: `/parakeet/windows/tdt.tar.gz` and `/parakeet/macos/tdt.tar.gz`), and its `model` type
is `parakeet-unified | parakeet-tdt | parakeet-realtime-eou`. Whether that build is deployed to the
live worker has not been verified. Any new route or model name needs a change to that source, and
an upload and deploy that are Adrian's to authorise.

The machine-readable vectors are `tests/vectors/model-grants.json`; the crate's tests
(`tests/grant_contract.rs`) consume them, and the worker's tests should load the same file.

## Purpose

A grant is a short-lived bearer token for downloading one archive from the worker: bound to one
route and one SHA-256, issued to one device. It replaces a long-lived app token on the download.
Entitlement-gated add-ons (for example Whisper large-v3-turbo) do **not** use grants (see
"Never accept").

## Request **[existing]**

`POST {worker_base}/v1/model-grants`, `Content-Type: application/json`, no `Authorization`
header. The crate pins the connection to the host of `worker_base`: HTTPS only, that one host,
public addresses only, no redirects followed, `User-Agent: rightkit-models/<version>`.

Body (camelCase; exactly these five fields; vector `request`):

| Field | Meaning |
|---|---|
| `route` | Archive path on the worker, the spec's archive URL with `worker_base` removed, e.g. `/parakeet/macos/tdt.tar.gz` |
| `model` | Worker model name from `GrantConfig.model`, e.g. `parakeet-tdt` |
| `platform` | `GrantConfig.platform`: `macos` or `windows` |
| `sha256` | Lowercase hex SHA-256 of the whole archive, from the crate's trusted catalog pin |
| `deviceId` | Stable random per-install id (32 hex chars from `device_id_get_or_create`); never a hardware identifier |

The crate sends no app id. **[proposal]** If the worker wants one for quotas, add an optional
`appId` string; the crate would then add it from `GrantConfig`. Until then the worker must not
require it.

## Success response **[existing]**

`200`, JSON object (vector `accepted`):

| Field | Meaning |
|---|---|
| `token` | Opaque non-empty string. Sent back verbatim on the download. |
| `expiresAt` | Unix seconds. Must be in the future. The crate refreshes 60 s before expiry. |
| `route` | The route the grant is bound to. Must equal the requested `route`. |
| `sha256` | The digest the grant is bound to. Must equal the requested `sha256` (compared ASCII case-insensitively). |

The crate rejects the response (no download is attempted) when `token` is blank, `expiresAt` is not
in the future, `route` differs, or `sha256` differs, and when the body is not this JSON object
(vector `rejected`). Extra fields are ignored. **[proposal]** The worker should keep the lifetime
short (minutes to an hour; the largest model needs the time to download, and a refresh mid-flight
is supported).

## Presenting the token **[existing]**

On the archive download the crate sends `Authorization: Bearer <token>` with `Accept-Encoding:
identity` and, when resuming, `Range` and `If-Range` (strong ETag). Redirects are followed by the
crate's vetted client, and the `Authorization` header is dropped when a redirect leaves the host.
A `401` or `403` on the download makes the crate request one fresh grant and retry once; a second
rejection fails the download with "download authorization rejected". `404` is "not available on
the download server".

## Status codes

The crate treats every non-2xx response to the grant request as failure ("model grant request
failed: HTTP N"); it does not branch on the code, and it does not retry the grant request on
`429`. The meanings below are **[proposal]** for the worker, so that logs and later client
behaviour agree:

| Status | Meaning | Body (proposal) |
|---|---|---|
| `400` | Malformed request, missing or extra-typed field, bad digest format | `{"error":"invalid_request"}` |
| `404` | Unknown `model`, or `route` not served for that `model`/`platform` | `{"error":"unknown_model"}` |
| `409` | `sha256` is not the digest of the object the worker serves at `route` | `{"error":"digest_mismatch"}` |
| `429` | Rate limit per `deviceId` and per source address; send `Retry-After` | `{"error":"rate_limited"}` |
| `5xx` | Worker or storage failure | any |

An expired or revoked grant is detected at download time, not at issue time: the archive route
answers `401` (token unknown, expired or revoked) or `403` (valid token, wrong route or digest).

## What the worker must verify **[proposal]**

1. `route`, `model` and `platform` name one object the worker serves; the pair is looked up in the
   worker's own table, never built from request text.
2. `sha256` equals the digest of that object as the worker records it (the crate pins the same
   value; a mismatch means a stale client or a tampered catalog).
3. The grant it issues is bound, server-side, to exactly `(route, sha256, deviceId)` and expiry.
4. On the download, the route and `Range` target are checked against the grant's route; the object
   served must have the grant's digest.
5. Rate limiting and abuse controls on `deviceId` and address.

## What must never be accepted **[proposal]**

- A download whose route differs from the grant's route, or that targets an object whose digest
  differs from the grant's digest, even with an otherwise valid token. (Answer `403`.)
- A grant request for an entitlement-gated model. Add-ons such as Whisper large-v3-turbo are
  downloaded with the app's signed Pro entitlement as the bearer; the crate never asks for a grant
  for them (`ModelSpec::requires_entitlement()`), so the worker should answer such a request `404`
  rather than issue a grant that would bypass the entitlement check.
- A response to the crate that echoes a different route or digest than was requested (the crate
  refuses it; the worker must not produce it).
- Reuse of a token after `expiresAt`, or for a different `deviceId`.

## Test vectors

`tests/vectors/model-grants.json`:

- `request`: the exact JSON body the crate sends. The crate's test fails if a field is added,
  renamed or changed in value.
- `accepted`: responses the crate turns into a grant.
- `rejected`: responses the crate refuses, each with the error text it contains.

Placeholders `"$FUTURE"` and `"$PAST"` in `expiresAt` mean "now + 1 h" and "now - 1 h"; a worker's
test substitutes its own clock the same way. Keep the file in step with this document: a change to
either goes through both.
