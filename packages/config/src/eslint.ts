// Flat ESLint preset for React + TypeScript app webviews. The plugins are optional peer
// dependencies: the host installs them and passes them in, so this module imports none of them.

export interface RightkitEslintPlugins {
  /** `@eslint/js` */
  js: { configs: { recommended: unknown } };
  /** `globals` */
  globals: { browser: Record<string, unknown>; es2022: Record<string, unknown> };
  /** `typescript-eslint` */
  tseslint: { configs: { recommended: unknown[] }; config: (...configs: unknown[]) => unknown[] };
  /** `eslint-plugin-react-hooks` */
  reactHooks: { configs: { recommended: { rules: Record<string, unknown> } } };
  /** `eslint-plugin-react-refresh` */
  reactRefresh: unknown;
}

export interface RightkitEslintOptions {
  /** Extra ignore globs on top of dist, node_modules, src-tauri, .audit and .cache. */
  ignores?: string[];
  /** Extra config objects appended after the preset (per-file overrides). */
  overrides?: unknown[];
}

export const DEFAULT_ESLINT_IGNORES: readonly string[] = [".audit/**", ".cache/**", "dist", "node_modules", "src-tauri"];

/** Rules the apps share: hooks correctness as errors, react-compiler advisories as warnings. */
export const RIGHTKIT_ESLINT_RULES: Readonly<Record<string, unknown>> = {
  "react-hooks/rules-of-hooks": "error",
  "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
  "react-hooks/exhaustive-deps": "warn",
  "react-hooks/purity": "warn",
  "react-hooks/refs": "warn",
  "react-hooks/set-state-in-effect": "warn",
  "@typescript-eslint/no-unused-expressions": "warn",
  "@typescript-eslint/no-unused-vars": ["warn", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
  "no-empty": ["warn", { allowEmptyCatch: true }],
  "prefer-const": "warn",
};

/** Builds the flat config. `export default rightkitEslintConfig({ js, globals, tseslint, reactHooks, reactRefresh })`. */
export function rightkitEslintConfig(plugins: RightkitEslintPlugins, options: RightkitEslintOptions = {}): unknown[] {
  const { js, globals, tseslint, reactHooks, reactRefresh } = plugins;
  return tseslint.config(
    { ignores: [...DEFAULT_ESLINT_IGNORES, ...(options.ignores ?? [])] },
    {
      files: ["**/*.{ts,tsx}"],
      extends: [js.configs.recommended, ...tseslint.configs.recommended],
      languageOptions: { ecmaVersion: 2022, globals: { ...globals.browser, ...globals.es2022 } },
      plugins: { "react-hooks": reactHooks, "react-refresh": reactRefresh },
      rules: { ...reactHooks.configs.recommended.rules, ...RIGHTKIT_ESLINT_RULES },
    },
    ...(options.overrides ?? []),
  );
}
