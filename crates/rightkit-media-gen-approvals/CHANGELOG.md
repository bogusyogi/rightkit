# Changelog

## 0.1.2

- Recover pending legacy media approval challenges with the original launch-secret proof, input binding, expiry & single-use marker. Preserve previously written receipts ([GenRight #305](https://github.com/bogusyogi/genright/issues/305)).

## 0.1.1

Taken from GenRight's shipped behaviour so it can delete its own copy of `approvals.rs` and `eyes.rs`. Every default keeps RightKit's behaviour; each option is additive and set once at startup.

- Added `set_engine_stem_prefix(prefix)` (default: unset). The parent-process rule refuses a parent whose stem starts with this engine's own stem and, by default, every stem ending in `-engine`. With a prefix set, the `-engine` suffix refusal no longer applies and a parent is refused as an engine when its stem starts with the prefix (GenRight's rule: only stems starting `genright-engine`). Interpreters (`sh`, `python`, `node`, ...) stay refused either way. GenRight calls `rightkit_media_gen_approvals::set_engine_stem_prefix("genright-engine")`.
- Changed: the `approval_requires_app` message and the human-eyes delivery message name the app through `rightkit_media_gen::host::MediaHostConfig::app_name`: "in the app" by default, "in the GenRight app" with `app_name` `GenRight` (both messages byte for byte GenRight's; the second "in the app" of the first message is unchanged).
- Added `eyes::set_default_stage(stage)`, `eyes::default_stage()` and `eyes::DEFAULT_STAGE` (`app ui`): the `stage` recorded in the `.eyes.json` sidecar when the input names none. GenRight calls `rightkit_media_gen_approvals::eyes::set_default_stage("genright ui")` so decisions keep the value its sidecars already carry.
- Accepted, not read: a challenge GenRight wrote before it moved to this crate (`<data>/approvals/<kind>/<draft>.<nonce>.json` with its own fields and a hand-written `.used` marker) is not read by the core store, so a draft that was awaiting approval at the switch asks the user once more. Challenges live 30 minutes and the only effect is a fresh request, so no reader for the old format is added; the path and the `nonce` / `input_hash` keys are the same, and old `.used` markers beside them are inert.

## 0.1.0

- Requires `rightkit-media-gen` 0.2.0. The token domain is the host's (`rightkit_media_gen::host::approval::set_token_domain`; default `rightkit-approval-v1`): an engine whose agent plugin mints `genright-approval-v1` tokens sets that domain at start. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).
- `eyes::file_hash` is a re-export of `rightkit_media_gen::host::approval::eyes_file_hash`, and the approval-record check calls `eyes_record_bound` there (needs `rightkit-media-gen` 0.1.2): one copy, shared with the gallery index that moved from `rightkit-library` to `rightkit_media_gen::gallery`.
- New crate. GenRight's approvals (`rightkit-approvals` 0.1.0: the launch-secret token gate, `arm` and parent binding, `issue`, `consume`, `register_kind`, the `studio.approvals.challenge` and `studio.blender.spec.*` commands, and `eyes`, the human-eyes gate) moved here when `rightkit-approvals` 0.2.0 became the one approvals core. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; the review found the shared crate name held one app's draft kinds and depended on `rightkit-media-gen`, so no other app could use it (docs/audits/2026-11-11-shape-matrix/reports/family-agent-platform.md, rightkit-approvals section).
- Paths: `rightkit_approvals::X` is `rightkit_media_gen_approvals::X` with the same names and signatures.
- On the core: a challenge is a core `Request` in the core's `FileStore` (id = nonce; binding = input hash, draft id and expiry); the token is the core `Proof`; `consume` is the core's `rules::take`, so the single-use marker is the core store's `.used` file. Deleted as duplicates: the challenge path, safe-name check, `.used` marker creation and expiry check of 0.1.0.
- Challenge files keep their path and their `nonce` and `input_hash` keys, so `rightkit_media_gen::host::approval::receipt_valid` reads them unchanged. The rest of the file is the core record; a challenge written by 0.1.0 is not read (30-minute life: the app asks for a new one).
- Refusal codes unchanged (`approval_requires_app`, `approval_nonce_mismatch`, `approval_token_invalid`, `approval_input_changed`, `approval_expired`, `approval_token_used`).
