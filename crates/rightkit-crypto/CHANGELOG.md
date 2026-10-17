# Changelog

## 0.1.0 (2026-10-17)

- New crate 0.1.0 (Orthic-Labs/rightkit#370, #372, #417, #477). `sha256`, `hmac_sha256` (RFC 2104/4231), `ct_eq`, `base64` and `base64url` encode/decode (RFC 4648), `hex`, `blake3_text`, `canonical_json` (sorted keys, compact), and `approval` token `mint` / `verify` (HMAC-SHA256 over a length-prefixed encoding of purpose, subject, issued-at and expiry, constant-time verify).
- Cross-crate items from #417 not implemented here: LLM token counting (rightkit-llm) and the ignore-aware repo walk (rightkit-fs).
- Not compiled or run when written; verification is the CI run.
