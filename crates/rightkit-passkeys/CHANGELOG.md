# Changelog

## 0.1.0 (unpublished)

- New crate (#968): `CredentialStore` and `UserVerifier` traits for the consumer, `StoreAdapter` and `VerifierAdapter` onto passkey-authenticator 0.6.0 (`CredentialStore`, `UserValidationMethod`), `authenticator(...)`, `normalize_rp_id`. Built from the published docs.rs signatures of passkey-authenticator 0.6.0 and passkey-types 0.6.0 (not from the source: passkey-rs is not in the local registry cache).
- Unverified until the gate compiles it: the `passkey_types::ctap2` paths for `Aaguid`, `Ctap2Error` and `StatusCode`, the `Ctap2Error` variants `Other`, `UnsupportedOption` and `OperationDenied`, `From<Ctap2Error> for StatusCode`, and the `passkey-crypto` 0.1 `CryptoBackend` path. A compile failure there is a path or variant name, not a design change.
- No unit tests and no synthetic store (Adrian: end-to-end only, no dummy data). RightPass exercises it against its real vault.
