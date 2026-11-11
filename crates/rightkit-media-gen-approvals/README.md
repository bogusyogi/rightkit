# rightkit-media-gen-approvals

Media-generation approvals on the `rightkit-approvals` core: in-app approval of paid drafts and the
human-eyes release of assets for delivery. The engine issues a single-use challenge (OS-random nonce,
SHA-256 of the draft's canonical inputs, expiry) as a request in the core's `FileStore`; `approve` is
refused unless it carries a token only the holder of the per-launch secret can mint (`HMAC-SHA256(secret,
kind | draft id | input hash | nonce)`), checked as the core's `Proof` and spent by the core's single-use
rule (`rules::take`). The shell holds the secret and mints (`token::mint_token`); the engine is armed once
from the first stdio frame (`arm`, with parent binding) and verifies (`gate`, `consume`). Hosts register
their draft kinds with `register_kind`; the Blender product spec finish confirmation (`blender_finish`)
and the human-eyes release (`eyes`) are built in.

Moved into RightKit from GenRight (`genright-studio/src/approvals.rs` and `eyes.rs`; token primitives from
`genright-protocol/src/approval.rs`, now `rightkit_media_gen::host::approval`) on 2026-11-01, as
`rightkit-approvals` 0.1.0; moved to this crate on 2026-11-11 when `rightkit-approvals` became the one
approvals core. The Tauri command that mints in the shell (`src-tauri/src/approval.rs`) stays in the app
and calls `token::mint_token`. Command names (`studio.approvals.challenge`, `studio.blender.spec.*`,
`studio.eyes.*`), the `genright-approval-v1` token domain and the challenge file layout
(`<data>/approvals/<kind>/<draft id>.<nonce>.json` plus `.used`, read by
`rightkit_media_gen::host::approval::receipt_valid`) are kept for wire compatibility.
