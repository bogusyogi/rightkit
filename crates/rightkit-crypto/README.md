# rightkit-crypto

Neutral hashing, MAC, signature, AEAD, key-derivation and encoding primitives so apps stop hand-rolling them. Not yet published (0.1.0). Catalog capability: `crypto-primitives` (to be added by the lead).

- `sha256(data) -> [u8; 32]`, `hmac_sha256(key, msg) -> [u8; 32]` (in-crate, RFC 4231 vectors in tests).
- `ct_eq(a, b) -> bool`: constant time over equal lengths; unequal lengths return false.
- `base64_encode` / `base64_decode` (standard, padded) and `base64url_encode` / `base64url_decode` (no padding), RFC 4648. `hex_encode`.
- `blake3_text(text) -> String`: lowercase hex BLAKE3 of the UTF-8 bytes.
- `canonical_json(&Value) -> String`: compact, object keys sorted by byte order, serde_json number and string forms.
- `approval::mint(key, purpose, subject, now, ttl_secs) -> String` and `approval::verify(key, purpose, subject, token, now) -> Result<(), ApprovalError>`. The MAC covers a length-prefixed encoding of the version, purpose, subject, issued-at and expiry; the compare is constant time. Token form: `v1.<issued>.<expires>.<base64url mac>`. The key is never formatted or logged, and errors carry no key or MAC material.

## Not here

LLM token counting belongs to `rightkit-llm` and the ignore-aware repo walk to `rightkit-fs` (#417). Not a TLS, certificate or X.509 library.

## Dependencies

`blake3`, `serde_json` and `ring` 0.17.14 (the version rightkit-mesh, rightkit-daemon and rightkit-context-runtime already use). SHA-256, HMAC-SHA256, base64 and compare are implemented in-crate.

## 0.1.2 additions (#845, #1172)

- Signatures: `Ed25519Signer` (`generate`, `from_pkcs8`, `from_seed`, `public_key`, `sign`), `ed25519_verify`, `ecdsa_p256_verify(pub, msg, sig, EcdsaEncoding::{Asn1, Fixed})`, `rsa_pkcs1_sha256_verify` (2048 to 8192 bit PKCS#1 `RSAPublicKey`). Verify returns `bool`.
- `aead::{seal, open}` with `Cipher::{Aes256Gcm, ChaCha20Poly1305}`: `nonce || ciphertext || tag`, fresh random nonce per call; `seal_with_nonce` / `open_with_nonce` for protocols that fix the nonce.
- `hkdf_sha256`, `pbkdf2_derive` / `pbkdf2_verify` (`Pbkdf2Hash::{Sha256, Sha512}`), `hmac(HmacAlgorithm::{Sha1, Sha256, Sha512}, key, msg)`, `totp(secret, algorithm, digits, period, unix) -> (code, seconds_left)` (RFC 4226/6238), `random_fill` / `random_bytes`.
- `Secret`: wiped on drop, `Debug` prints the length only, no `Clone`. `CryptoError` is opaque.
