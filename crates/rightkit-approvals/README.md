# rightkit-approvals

In-app approval of paid drafts and deliveries. The engine issues a single-use challenge (OS-random
nonce, SHA-256 of the draft's canonical inputs, expiry); `approve` is refused unless it carries a
token only the holder of the per-launch secret can mint (`HMAC-SHA256(secret, kind | draft id |
input hash | nonce)`). The shell holds the secret and mints (`token::mint_token`); the engine is
armed once from the first stdio frame (`arm`, with parent binding in release builds) and verifies
(`gate`, `consume`). Hosts register their draft kinds with `register_kind`; the Blender product
spec finish confirmation (`blender_finish`) is built in.

Moved into RightKit from GenRight (`genright-studio/src/approvals.rs`; token primitives from
`genright-protocol/src/approval.rs`, now `rightkit_media_gen::host::approval`) on 2026-11-01. The
Tauri command that mints in the shell (`src-tauri/src/approval.rs`) stays in the app and calls
`token::mint_token`. Command names (`studio.approvals.challenge`, `studio.blender.spec.*`) and the
`genright-approval-v1` token domain are kept for wire compatibility.
