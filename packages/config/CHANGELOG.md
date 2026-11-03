# Changelog

## 0.1.1 (unpublished)

- **`rightkitQaDefine()` and the one web QA define (rightkit#1480).** `@rightkit/config/vite` exports `rightkitQaDefine(env?)`, returning `{ __RIGHTKIT_QA__: "true" | "false" }` from `RIGHTKIT_BUILD_MODE === "dev"` (the variable `right-release dev` sets for every command it runs), and `rightkitViteConfig` now includes it as `define`, so an app writes nothing. Written from code, not run.
- `rust/deny.toml` and `rust/audit.toml` document the optional per-app platform scope (`[graph] targets`, `[target] os`) for apps that ship only some targets (CutRight: macOS and Windows).

## 0.1.0

- First release: Vite base config, flat ESLint preset, tsconfig presets and Rust toolchain/deny/audit/rustfmt/about templates.
