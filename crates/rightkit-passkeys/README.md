# rightkit-passkeys

A passkey authenticator for Right Suite apps, as an adapter over `passkey-rs` 0.6. The consumer
(RightPass) implements `CredentialStore` (find, save, update counter over its vault) and
`UserVerifier` (the user interaction); `authenticator(aaguid, store, verifier, crypto)` returns a
`passkey_authenticator::Authenticator`.

- Request normalisation: `normalize_rp_id` (lowercase ASCII, one trailing dot removed, no scheme,
  port, path or non-ASCII); the store only ever sees the normalised id.
- Truthful flags: the user-presence and user-verification flags are exactly what the `UserVerifier`
  reports it did. A request that requires verification is refused when the verifier cannot verify,
  and a verifier that falls short of the requirement is refused, never rounded up.
- No key material of its own: private keys live inside the consumer's store.

Not here: the Windows plugin-authenticator activation (packaged MSIX app, COM
`IPluginAuthenticator`, `WebAuthNPluginAddAuthenticator`), which is app-side, and the Apple
credential-provider extension, which is `RightKitCredentialProvider` in RightKitSwift.

**Do not use it** to store credentials: it has no store of its own and ships no test double.
