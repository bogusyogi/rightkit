# Right Suite legal source

This directory is the canonical drafting source for the proprietary Right Suite desktop contract. It is separate from `@rightkit/license`, which verifies paid entitlements, and from the open-source licenses of RightKit packages themselves.

## Customer contract

- `LICENSE.md` — ownership and source/binary restriction notice.
- `EULA.md` — contractual permission to install and use Right Suite desktop apps.
- `ACCEPTABLE_USE.md` — incorporated misuse restrictions.
- `products/<app>/PRODUCT_SCHEDULE.md` — app-specific devices, commercial scope, updates, AI/data, and support terms.
- Each app's generated `THIRD_PARTY_NOTICES.md` — exact-build third-party rights and attributions.
- `WEBSITE_TERMS.md` and `COOKIE_NOTICE.md` — shared website layer.
- `ENTERPRISE_ORDER_TERMS.md`, `ORDER_FORM_TEMPLATE.md`, and `ENTERPRISE_DPA_TEMPLATE.md` — order proof and the conditional enterprise processing layer.

The EULA and product schedule are accepted together. Privacy is presented as a notice, not bundled as consent. An enterprise order changes only the commercial fields it expressly identifies.

## Distribution rule

Apps do not reach into this workspace at build time. They carry an immutable snapshot plus `legal-manifest.json`. `@rightkit/release` verifies the snapshot's SHA-256 hashes and installer configuration before packaging. A document's `acceptanceVersion` must never be reused for different material bytes.

## Review status

Version 1.1 is a researched operational launch draft dated July 16, 2026. It has not been approved by a lawyer. It uses a two-basis model: individual (Free/Pro, follows one natural person and their own devices, including their own freelance work) and enterprise (any use by or for an Organization, on its devices, or that it pays for or directs, licensed per Seat under an Enterprise Order). Before suite-wide public adoption, obtain a focused review of the individual/enterprise-use boundary and Seat definition, grant and termination, consumer carve-outs, warranty/liability limits, governing law/venue, tax language, and enterprise proof terms. Record approved changes as a new material acceptance version.
