# Changelog

## 0.1.0

- Initial source: host-configured provider slots, validation, live checks & engine read-state classification.
- Validate HTTPS endpoints before attaching credentials; restrict redirects to configured origin & classify only 2xx as valid.
- Preserve per-slot read failures & attempt every requested change.
