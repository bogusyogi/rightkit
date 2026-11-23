# Changelog

## 0.1.1

- Derive renderer key status, updates & readiness from the host's provider table. Gate writes & refresh credentials after mutations, including partial updates.

## 0.1.0

- Initial source: host-configured provider slots, validation, live checks & engine read-state classification.
- Validate HTTPS endpoints before attaching credentials; restrict redirects to configured origin & classify only 2xx as valid.
- Preserve per-slot read failures & attempt every requested change.
