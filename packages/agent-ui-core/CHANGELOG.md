# Changelog

## 0.1.1 (unpublished)

- The legal store reads the receipt through `@rightkit/onboarding`'s `LegalReceiptPort`, whose `read()` may return a historical receipt (`{ record, current: false }`); that value is superseded, so the store keeps `null` and the gate asks again. Fixes the TS2322 type error that stopped the package from building.

## 0.1.0

- First release. Coding-agent UI core: daemon client, session and transcript types, stores and helpers.
