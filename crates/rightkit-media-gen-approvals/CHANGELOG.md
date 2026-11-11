# Changelog

## 0.1.0 (unpublished)

- New crate. GenRight's approvals (`rightkit-approvals` 0.1.0: the launch-secret token gate, `arm` and parent binding, `issue`, `consume`, `register_kind`, the `studio.approvals.challenge` and `studio.blender.spec.*` commands, and `eyes`, the human-eyes gate) moved here when `rightkit-approvals` 0.2.0 became the one approvals core. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; the review found the shared crate name held one app's draft kinds and depended on `rightkit-media-gen`, so no other app could use it (docs/audits/2026-11-11-shape-matrix/reports/family-agent-platform.md, rightkit-approvals section).
- Paths: `rightkit_approvals::X` is `rightkit_media_gen_approvals::X` with the same names and signatures.
- On the core: a challenge is a core `Request` in the core's `FileStore` (id = nonce; binding = input hash, draft id and expiry); the token is the core `Proof`; `consume` is the core's `rules::take`, so the single-use marker is the core store's `.used` file. Deleted as duplicates: the challenge path, safe-name check, `.used` marker creation and expiry check of 0.1.0.
- Challenge files keep their path and their `nonce` and `input_hash` keys, so `rightkit_media_gen::host::approval::receipt_valid` reads them unchanged. The rest of the file is the core record; a challenge written by 0.1.0 is not read (30-minute life: the app asks for a new one).
- Refusal codes unchanged (`approval_requires_app`, `approval_nonce_mismatch`, `approval_token_invalid`, `approval_input_changed`, `approval_expired`, `approval_token_used`).
