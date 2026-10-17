# Legal defaults adoption (#266)

`@rightkit/legal` 0.4.0 ships the suite company facts and default EULA, privacy, refund and terms texts. No app asks for or copies them.

## What is a suite default

Company facts: legal entity, address, legal contact, governing law, venue, minimum age 18. Privacy data statement: "data we receive: update checks only".

## What each app supplies (required, no default)

- `appName`
- `supportEmail` (products differ: hello@heardright.app, hello@orthiclabs.com)
- `refundPolicy`: `{ kind: "window", days: 14 }` (HeardRight) or `{ kind: "none", trialDays: 30 }` (GenRight, "no refunds except where the law requires")

Optional overrides: `facts`, `dataStatement`, `append`, `replace`.

## Steps

1. `renderDefaultPolicies(input)` for the texts; `defaultPolicyDescriptors(input, { version, publicUrlBase })` for the `LegalManifest` documents.
2. In the UI, `policyGateDocuments(renderDefaultPolicies(input))` from `@rightkit/legal-ui` feeds `LegalGate`.
3. Delete any local copy of the company facts or policy text.

A change to a fact or to the refund policy changes the rendered text and therefore the material digest (eula, license, product_schedule), which triggers re-acceptance. Mapping: terms -> license, refund -> product_schedule (the manifest has no terms or refund role).
