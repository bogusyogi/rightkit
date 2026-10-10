# Changelog

## 0.4.1 — 2026-10-10

- Depend on the published `@rightkit/qa-chrome` `0.1.3` exactly instead of `workspace:*`, so the source manifest matches the tarball dependency. No runtime change.

## 0.4.0 — 2026-10-08

- Add `QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard` & typed `TimeoutError` using raw CDP over Node 26's native WebSocket.
- Support navigation, viewport/DPR, init scripts, JSON evaluation, selector/predicate waits, remote handles, real input & viewport PNG capture.
- Add mock transport regressions & real Chrome for Testing fixture coverage.

## 0.3.0 — 2026-10-08

- Breaking: WebdriverIO native lane & `right-qa` CLI are removed.
- Drive apps under test with Rust `rightkit-qa` + `rightkit-control` crates.
- Use `@rightkit/qa-chrome` for a test browser.
