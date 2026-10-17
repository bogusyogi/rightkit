# Changelog

## 0.4.1 — 2026-10-17

- `supportEmail` now has a suite default, `hello@orthiclabs.com`, and is an optional per-app override (#266, decision D1).
- The default privacy text states the product is not directed to anyone under the minimum age (18) (#266, decision D2). Rendered privacy text changes, which does not move the material digest.

## 0.4.0 — 2026-10-17

Additive (Orthic-Labs/rightkit#266). Manifest, receipt and digest behaviour is unchanged.

- `RIGHT_SUITE_COMPANY_FACTS` (frozen) and `CompanyFacts`: legal entity, address, contact email, governing law, venue and minimum age 18, each cited to its source file in code comments. `supportEmail` was required in 0.4.0 (see 0.4.1).
- `RefundPolicy`: required per-app field, either `{ kind: "window", days }` or `{ kind: "none", trialDays? }` ("no refunds except where the law requires"). Refunds are not a suite default.
- `renderDefaultPolicy` / `renderDefaultPolicies`: default EULA, privacy (data statement "data we receive: update checks only"), refund and terms texts rendered from the facts, with per-app `facts`, `dataStatement`, `append` and `replace` overrides.
- `defaultPolicyDescriptors`: `LegalDocumentDescriptor`s hash-bound to the rendered text (eula -> eula, terms -> license, refund -> product_schedule, privacy -> privacy_notice), so a facts change moves `materialAcceptanceDigest`.
