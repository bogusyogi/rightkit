# ADR-001 — strict default deny

Status: frozen for Phase 0

All execution outside an authenticated, request-scoped broker grant is denied in `ENFORCE`. A signature, cdhash or hash proves identity only; copied output never inherits authorization. Audit or policy drift admits no broker work. This preserves V5 §20 default-deny gate without publisher-wide trust.
