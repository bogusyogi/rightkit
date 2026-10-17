# Changelog

## 0.1.0 (2026-10-17)

## 0.1.0

- Added (#163): split-key secret vault. Payload sealed under a random data key; the key wrapped under `HKDF-SHA256(server_key || client_secret)` with ChaCha20-Poly1305 (`ring`, same construction as `rightkit-sync`'s cipher). Server half from the entitlement service (`HttpServerKeySource`), client half in the OS credential store (`CredentialsClientSecret`), offline server-key cache in the OS credential store (`CredentialsKeyCache`, 7-day default grace). `load`, `save`, `rotate` (re-wrap), `reset` (after a 410 erase), `refresh`, `sign_out`. Storage is the `BlobStore` trait.
- Unverified at release: written without a build or test run (standing order); first CI run is the first compile. The service response shape is assumed pending rightsites#81. No rollback detection; concurrent saves are last-writer-wins.
