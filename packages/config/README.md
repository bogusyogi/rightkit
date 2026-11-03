# @rightkit/config

Shared presets for desktop apps: TypeScript configs, a Vite base config, a flat ESLint preset and
Rust toolchain, cargo-deny, cargo-audit, rustfmt and cargo-about templates.

| Item | Value |
|---|---|
| Where it runs | Build tooling (Node) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/config/vite` | `rightkitViteConfig({ port, strictPort, host, plugins, watchIgnored, buildTarget, qa })` (also sets `define: rightkitQaDefine()`), `rightkitQaDefine()` (`{ __RIGHTKIT_QA__ }`, true only when `RIGHTKIT_BUILD_MODE=dev`), `DEFAULT_WATCH_IGNORED` |
| `@rightkit/config/eslint` | `rightkitEslintConfig({ js, globals, tseslint, reactHooks, reactRefresh }, { ignores, overrides })`, `RIGHTKIT_ESLINT_RULES` |
| `@rightkit/config/tsconfig/base.json` | Strict ES2022 + DOM, bundler resolution |
| `@rightkit/config/tsconfig/react-app.json` | App webview (`noEmit`, `react-jsx`, `vite/client` types) |
| `@rightkit/config/tsconfig/library.json` | Emitting library (`dist/` from `src/`, `.ts` imports rewritten) |
| `@rightkit/config/rust/*` | `deny.toml`, `audit.toml` (copy to `.cargo/audit.toml`), `rust-toolchain.toml`, `rustfmt.toml`, `about.toml`, `cargo-config.toml` (copy to `.cargo/config.toml`) |

```ts
// vite.config.ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { rightkitViteConfig } from "@rightkit/config/vite";

export default defineConfig(rightkitViteConfig({ port: 1421, plugins: [react()] }));
```

```js
// eslint.config.js
import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import { rightkitEslintConfig } from "@rightkit/config/eslint";

export default rightkitEslintConfig({ js, globals, tseslint, reactHooks, reactRefresh });
```

The one web QA switch is `__RIGHTKIT_QA__` (`declare const __RIGHTKIT_QA__: boolean;`): `rightkitViteConfig` defines it from `RIGHTKIT_BUILD_MODE` (`right-release dev` sets `dev` for every command it runs; a release build leaves it unset, so the define is `false` and QA branches fold away). An app with its own `define` writes `define: { ...rightkitQaDefine() }`. QA code inside `if (__RIGHTKIT_QA__)` carries the string `RIGHTKIT-QA-WEB-COMPILED`; a release bundle that contains the string, the define or a retired flag (`VITE_VIEWRIGHT_BROWSER_QA`, `VITE_RIGHTKIT_BROWSER_QA`, `VITE_RIGHTKIT_QA`, `__RIGHTKIT_MOCK__`) is refused by `@rightkit/release`.

The ESLint preset takes the plugins as arguments, so this package depends on none of them; `vite`
is an optional peer used only for types.

## Testing

```sh
pnpm --dir packages/config test
```
