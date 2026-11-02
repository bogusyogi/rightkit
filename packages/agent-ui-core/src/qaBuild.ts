/**
 * Single compile-time QA switch. Never a runtime flag.
 *
 * `vite build` replaces `__RIGHTKIT_QA__` with the literal `true` (only for
 * `--mode qa`) or `false`, so in release bundles `QA_BUILD` is `false` and every
 * `if (QA_BUILD)` / `QA_BUILD && ...` branch is dead-code-eliminated, including the
 * env-var fallback below. The fallback only exists for dev server and vitest,
 * where the define is absent. `scripts/check-release-refuses-qa.mjs` verifies the release configs
 * enable no QA feature and that `right-release dist-scan` forbids the QA markers. Do not read `VITE_RIGHTKIT_QA` anywhere else.
 */
export const QA_BUILD: boolean =
  typeof __RIGHTKIT_QA__ === "boolean"
    ? __RIGHTKIT_QA__
    : import.meta.env?.MODE === "qa" || import.meta.env?.VITE_RIGHTKIT_QA === "1";
