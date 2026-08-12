# ADR-005 — migration

Status: frozen for Phase 0

Migrate one classified legacy namespace at a time through inventory, external registration, shadow comparison, checksum manifest, rollback simulation & quarantine after rollback window. Legacy namespaces never gain implicit `ENFORCE` access. SwiftPM parity completes before any Swift producer or consumer cutover; unclassified frontends remain denied.
