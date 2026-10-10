# Changelog

## 0.1.2

Contract changes (`contracts/daemon-api.v1.schema.json`, additive; the valid fixtures follow):

- Provider slug: `{provider}` in `/v1/oauth/{provider}/callback` and `/v1/webhooks/{provider}` must match `^[a-z0-9][a-z0-9-]{0,63}$` (`$defs/providerSlug`). Enforced at route dispatch: a non-matching provider is not a route and answers `404 not_found` before any handler or OAuth flow runs. Previously `Instagram` on a webhook answered 401 and on a callback reached the OAuth handler.
- Error enum now lists every code the daemon returns (adds `precondition_required` 428, `idempotency_key_reuse` 422, and `bad_request`, `method_not_allowed`, `body_too_large`, `internal`, `storage_error`, `already_enrolled`, `too_large`, `too_many_uploads`, `bad_chunk_size`, `bad_chunk_index`, `bad_chunk_length`, `chunk_too_large`, `chunk_digest_mismatch`, `chunk_conflict`, `already_complete`, `finalizing`, `hash_mismatch`, `incomplete`, `oauth_unavailable`, `duplicate_delivery`). The api-v1 fixture lists the two mutation codes.
- `job.update` responses are `[200, 400, 401, 403, 409, 422, 428]` (adds 400 bad body, 422 idempotency-key reuse, 428 missing `If-Match` or `Idempotency-Key`) in the route table, schema example and valid fixtures.
- Lifecycle events: new `job_updated`; `JOB_UPDATED` now maps to it instead of `job_queued`. `job_queued`, `job_published`, `job_outcome_unknown` and `token_refreshed` have no daemon state to emit from (no publish step, no refresh flow), so they stay in the vocabulary and are marked reserved in the schema (`x-reservedEvents`) rather than removed, which would break the existing fixture.
- Drift tests: every error code in the source is in the schema enum; every schema event is emitted or reserved; `job.update` documents its statuses.

## 0.1.1

- Drop the unused `rustls-pki-types` dependency (the code reaches it through `rustls::pki_types`).
- Replace `url` with `form_urlencoded` (one query-string parse); behavior is unchanged.
- Keep `subtle` for `ct_eq`: the only `ring` equivalent, `ring::constant_time::verify_slices_are_equal`, is deprecated ("no promises regarding side channels").
- Depend on `rightkit-contracts` with `default-features = false`: the daemon uses only the typed structs, never `validate`, so `jsonschema` leaves its tree. The conformance tests re-enable `schema` through a dev-dependency.
