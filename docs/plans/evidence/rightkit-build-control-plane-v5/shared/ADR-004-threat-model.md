# ADR-004 — threat model

Status: frozen for Phase 0

Threats include copied or renamed executables, hostile build scripts/proc macros/native children, user-writable interpreter modules, stale storage mounts, policy drift, receipt theft, process/UID reuse & concurrent capacity exhaustion. Controls are request-bound authorization, immutable tool roots, read-only inputs, capability-mediated shared writes, ACL/service isolation, content-addressed provenance, non-exportable rotating receipt keys, quota reservations & poison-on-disconnect. Private/deprecated macOS sandbox APIs are forbidden.
