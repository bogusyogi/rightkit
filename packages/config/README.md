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
| `@rightkit/config/vite` | `rightkitViteConfig({ port, strictPort, host, plugins, watchIgnored, buildTarget, qa })`, `DEFAULT_WATCH_IGNORED` |
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

The ESLint preset takes the plugins as arguments, so this package depends on none of them; `vite`
is an optional peer used only for types.

## Testing

```sh
pnpm --dir packages/config test
```
