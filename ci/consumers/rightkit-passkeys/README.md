# rightkit-passkeys 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-passkeys` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `async-trait`.

Implements the crate's CredentialStore and UserVerifier traits over in-memory fakes, then exercises
relying-party id normalisation (accepted forms and nine refusals), the store adapter accessor, the
verifier adapter constructor, truthful presence and verification reporting and a declined prompt,
driven by a no-op-waker poll. No authenticator ceremony runs, no key is created and no credential
store is touched.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
