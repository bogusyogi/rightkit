# rightkit-crypto

Neutral hashing, MAC and encoding primitives so apps stop hand-rolling them. Not yet published (0.1.0). Catalog capability: `crypto-primitives` (to be added by the lead).

- `sha256(data) -> [u8; 32]`, `hmac_sha256(key, msg) -> [u8; 32]` (in-crate, RFC 4231 vectors in tests).
- `ct_eq(a, b) -> bool`: constant time over equal lengths; unequal lengths return false.
- `base64_encode` / `base64_decode` (standard, padded) and `base64url_encode` / `base64url_decode` (no padding), RFC 4648. `hex_encode`.
- `blake3_text(text) -> String`: lowercase hex BLAKE3 of the UTF-8 bytes.
- `canonical_json(&Value) -> String`: compact, object keys sorted by byte order, serde_json number and string forms.
- `approval::mint(key, purpose, subject, now, ttl_secs) -> String` and `approval::verify(key, purpose, subject, token, now) -> Result<(), ApprovalError>`. The MAC covers a length-prefixed encoding of the version, purpose, subject, issued-at and expiry; the compare is constant time. Token form: `v1.<issued>.<expires>.<base64url mac>`. The key is never formatted or logged, and errors carry no key or MAC material.

## Not here

LLM token counting belongs to `rightkit-llm` and the ignore-aware repo walk to `rightkit-fs` (#417). No encryption, signatures or key derivation.

## Dependencies

`blake3` and `serde_json`, both at versions already in the workspace lockfile. SHA-256, HMAC, base64 and compare are implemented in-crate.
