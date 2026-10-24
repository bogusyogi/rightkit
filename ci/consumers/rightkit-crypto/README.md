# rightkit-crypto 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-crypto =0.1.0` exactly with its default features (the crate has none; every app pins it this way). It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion. `serde_json` is the only other dependency, because `canonical_json` takes a `serde_json::Value`.

The expected values do not come from the crate. They are the FIPS 180-4 and RFC 4231 vectors, the RFC 4648 base64 vectors, the BLAKE3 reference vectors, and digests, MACs and approval tokens computed once with a second implementation (a Python standard-library hash and HMAC) and pasted in as constants.

## What it proves

- `sha256`: the FIPS 180-4 examples (empty, `abc`, the 448-bit and 896-bit messages), one million `a`, and the padding boundaries at 55, 56, 63, 64, 65, 119, 120, 127 and 128 bytes, where the length field moves into another block.
- `hmac_sha256`: RFC 4231 cases 1, 2, 3, 4, 6 and 7, an empty key with an empty message, a key exactly one block long, and a key one byte over the block (hashed first); the key and the message each change the MAC.
- `ct_eq`: equal, first-byte, last-byte and length differences in both directions, empty slices, and SHA-256 digests. The consumer cannot show that the comparison is constant time; that is a timing property, not an output.
- `hex_encode`: lowercase, zero padded, empty input, and every byte value.
- `base64_encode`, `base64_decode`, `base64url_encode`, `base64url_decode`: the RFC 4648 vectors in both alphabets (padded and unpadded), the `+/` against `-_` difference, a round trip for every length from 0 to 96 and for all 256 byte values, the padded output length, and refusal of a bad length, bad padding, non-canonical trailing bits, characters outside the alphabet, and the other alphabet. `DecodeError` is a payload-free value with the message `invalid base64` and implements `std::error::Error`.
- `blake3_text`: the BLAKE3 reference vectors for the inputs of 0, 1, 2 and 3 bytes, a 64-character lowercase hex shape over multi-chunk non-ASCII text, determinism, and sensitivity to one more character.
- `canonical_json`: keys sorted by byte order at every depth (including a non-ASCII key), compact output, array order kept, scalar forms, serde_json string escapes, independence from insertion order, and an idempotent parse and re-serialise round trip.
- `approval::mint` and `approval::verify`: the exact token `v1.<issued>.<expires>.<base64url MAC>` for four independently computed tokens (ASCII, non-ASCII purpose and subject, empty key and fields, a key longer than the HMAC block), the 43-character url-safe MAC, the validity window `[issued, expires)` including its edges, a zero lifetime that never validates, an expiry that saturates at `u64::MAX`, refusal of a changed purpose, subject or key (`BadSignature`), unambiguous field boundaries, tampered times and a flipped MAC, and every malformed form (wrong segment count or version, non-numeric, signed, zero-padded, space-padded and overflowing numbers, an undecodable MAC). `ApprovalError` is a copyable value with four stable messages, implements `std::error::Error`, and never renders the key or the MAC.

## What it deliberately does not prove

- Constant-time behaviour of `ct_eq` and the absence of timing leaks in `verify`.
- Behaviour on a host with a different pointer width or byte order than the hosted runners.
- Any use of the crate inside an app; the apps pin `=0.1.0` and call the same public functions.

State: in-memory values only. No network, no files, no process spawn, no clock, no credentials, no real user data. The longest step hashes one million bytes.
