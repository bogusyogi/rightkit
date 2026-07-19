# @rightkit/legal

Pure, serializable legal-contract primitives for proprietary Right Suite desktop apps.

This package validates legal manifests, creates versioned acceptance receipts, decides when a material document change needs reacceptance, and encodes the suite's 0–9 free / 10+ commercial Worker threshold. It contains no entitlement, payment, analytics, or customer-identification code and no customer-facing legal prose.

Canonical master documents live in `tools/rightkit/legal/`. Each app ships a hash-bound snapshot plus its own product schedule and exact-build third-party notices. `@rightkit/release` verifies those snapshots before packaging.

The package is infrastructure, not legal advice. Customer wording remains subject to independent legal review.
