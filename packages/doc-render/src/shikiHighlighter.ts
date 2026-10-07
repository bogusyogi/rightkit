import type { HighlighterCore, LanguageRegistration } from "shiki/core";

/**
 * The one Shiki highlighter shared by the Markdown preview (`CodeBlock`), the hybrid editor's
 * code-fence decorations and the language validator.
 *
 * Built on `shiki/core` rather than `shiki/bundle/web` so the build ships only what is listed here:
 * two themes and a curated grammar set, each grammar its own lazy chunk. Oniguruma stays the regex
 * engine — the JavaScript engine tokenizes identically but transpiling a grammar's patterns made the
 * first highlight of a fence several times slower (≈1.6–2 s vs ≈0.3 s for a 40-line TS/C++ fence).
 * Its WASM ships as a binary asset fetched on first use, not as a base64 JS module that has to be
 * parsed. When WebAssembly is unavailable (macOS Lockdown Mode) the JavaScript engine takes over.
 */

export const SHIKI_LIGHT_THEME = "github-light";
export const SHIKI_DARK_THEME = "github-dark";

type LanguageModule = Promise<{ default: LanguageRegistration[] }>;

/** Curated grammars: mainstream Markdown fence languages. Anything else renders as plain text. */
const LANGUAGE_LOADERS: ReadonlyMap<string, () => LanguageModule> = new Map([
  ["astro", () => import("shiki/langs/astro.mjs")],
  ["bat", () => import("shiki/langs/bat.mjs")],
  ["c", () => import("shiki/langs/c.mjs")],
  ["cpp", () => import("shiki/langs/cpp.mjs")],
  ["csharp", () => import("shiki/langs/csharp.mjs")],
  ["css", () => import("shiki/langs/css.mjs")],
  ["csv", () => import("shiki/langs/csv.mjs")],
  ["diff", () => import("shiki/langs/diff.mjs")],
  ["docker", () => import("shiki/langs/docker.mjs")],
  ["dotenv", () => import("shiki/langs/dotenv.mjs")],
  ["glsl", () => import("shiki/langs/glsl.mjs")],
  ["go", () => import("shiki/langs/go.mjs")],
  ["graphql", () => import("shiki/langs/graphql.mjs")],
  ["handlebars", () => import("shiki/langs/handlebars.mjs")],
  ["html", () => import("shiki/langs/html.mjs")],
  ["http", () => import("shiki/langs/http.mjs")],
  ["ini", () => import("shiki/langs/ini.mjs")],
  ["java", () => import("shiki/langs/java.mjs")],
  ["javascript", () => import("shiki/langs/javascript.mjs")],
  ["jinja", () => import("shiki/langs/jinja.mjs")],
  ["json", () => import("shiki/langs/json.mjs")],
  ["json5", () => import("shiki/langs/json5.mjs")],
  ["jsonc", () => import("shiki/langs/jsonc.mjs")],
  ["jsonl", () => import("shiki/langs/jsonl.mjs")],
  ["jsx", () => import("shiki/langs/jsx.mjs")],
  ["julia", () => import("shiki/langs/julia.mjs")],
  ["kotlin", () => import("shiki/langs/kotlin.mjs")],
  ["less", () => import("shiki/langs/less.mjs")],
  ["lua", () => import("shiki/langs/lua.mjs")],
  ["make", () => import("shiki/langs/make.mjs")],
  ["markdown", () => import("shiki/langs/markdown.mjs")],
  ["mdx", () => import("shiki/langs/mdx.mjs")],
  ["php", () => import("shiki/langs/php.mjs")],
  ["postcss", () => import("shiki/langs/postcss.mjs")],
  ["powershell", () => import("shiki/langs/powershell.mjs")],
  ["python", () => import("shiki/langs/python.mjs")],
  ["r", () => import("shiki/langs/r.mjs")],
  ["regexp", () => import("shiki/langs/regexp.mjs")],
  ["ruby", () => import("shiki/langs/ruby.mjs")],
  ["rust", () => import("shiki/langs/rust.mjs")],
  ["sass", () => import("shiki/langs/sass.mjs")],
  ["scss", () => import("shiki/langs/scss.mjs")],
  ["shellscript", () => import("shiki/langs/shellscript.mjs")],
  ["shellsession", () => import("shiki/langs/shellsession.mjs")],
  ["sql", () => import("shiki/langs/sql.mjs")],
  ["svelte", () => import("shiki/langs/svelte.mjs")],
  ["swift", () => import("shiki/langs/swift.mjs")],
  ["toml", () => import("shiki/langs/toml.mjs")],
  ["tsx", () => import("shiki/langs/tsx.mjs")],
  ["typescript", () => import("shiki/langs/typescript.mjs")],
  ["vue", () => import("shiki/langs/vue.mjs")],
  ["xml", () => import("shiki/langs/xml.mjs")],
  ["yaml", () => import("shiki/langs/yaml.mjs")],
]);

/** Shiki's own aliases for the curated grammars (mirrors each grammar's `aliases`). */
const LANGUAGE_ALIASES: ReadonlyMap<string, string> = new Map([
  ["batch", "bat"],
  ["cmd", "bat"],
  ["c++", "cpp"],
  ["c#", "csharp"],
  ["cs", "csharp"],
  ["dockerfile", "docker"],
  ["hbs", "handlebars"],
  ["properties", "ini"],
  ["js", "javascript"],
  ["cjs", "javascript"],
  ["mjs", "javascript"],
  ["jl", "julia"],
  ["kt", "kotlin"],
  ["kts", "kotlin"],
  ["makefile", "make"],
  ["md", "markdown"],
  ["ps", "powershell"],
  ["ps1", "powershell"],
  ["pwsh", "powershell"],
  ["py", "python"],
  ["regex", "regexp"],
  ["rb", "ruby"],
  ["rs", "rust"],
  ["bash", "shellscript"],
  ["sh", "shellscript"],
  ["shell", "shellscript"],
  ["zsh", "shellscript"],
  ["console", "shellsession"],
  ["ts", "typescript"],
  ["cts", "typescript"],
  ["mts", "typescript"],
  ["yml", "yaml"],
]);

/** Names Shiki renders as plain text without a grammar. */
const PLAIN_TEXT_LANGUAGES = new Set(["text", "txt", "plain", "plaintext"]);

/**
 * Canonical Shiki language id for a fence info string, `"text"` for the plain-text names, or
 * `null` when the language is not in the curated set.
 */
export function resolveShikiLanguage(language: string | null | undefined): string | null {
  const normalized = (language ?? "").trim().toLowerCase();
  if (normalized.length === 0) return null;
  if (PLAIN_TEXT_LANGUAGES.has(normalized)) return "text";
  if (LANGUAGE_LOADERS.has(normalized)) return normalized;
  return LANGUAGE_ALIASES.get(normalized) ?? null;
}

let configuredWasmUrl: string | null = null;

/** Override where the Oniguruma WASM is fetched from (hosts that serve it themselves). */
export function configureShikiWasm(url: string): void {
  configuredWasmUrl = url;
}

// Imported only when a highlighter is first built: a top-level `?url` import is a bundler feature,
// and plain Node ESM hosts (e.g. Vitest without inlining) would try to load the .wasm as a module.
async function onigWasmUrl(): Promise<string> {
  if (configuredWasmUrl) return configuredWasmUrl;
  const mod = (await import("shiki/onig.wasm?url")) as { default: string };
  return mod.default;
}

async function createRegexEngine() {
  try {
    const { createOnigurumaEngine } = await import("shiki/engine/oniguruma");
    // Hand Shiki bytes, never the Response: for a Response it calls `instantiateStreaming`, which
    // rejects unless the asset protocol serves `application/wasm`. Any fetch or instantiate failure
    // lands in the catch below and falls back to the JavaScript regex engine.
    return await createOnigurumaEngine(() =>
      onigWasmUrl().then((url) => fetch(url)).then((response) => {
        if (!response.ok) throw new Error(`Shiki WASM failed to load (${response.status})`);
        return response.arrayBuffer();
      }),
    );
  } catch {
    const { createJavaScriptRegexEngine } = await import("shiki/engine/javascript");
    return createJavaScriptRegexEngine({ forgiving: true });
  }
}

let highlighterPromise: Promise<HighlighterCore> | null = null;

/** Lazy singleton — Shiki, its engine and the two themes load when the first code block needs them. */
export function getShikiHighlighter(): Promise<HighlighterCore> {
  if (!highlighterPromise) {
    highlighterPromise = (async () => {
      const [{ createHighlighterCore }, engine, light, dark] = await Promise.all([
        import("shiki/core"),
        createRegexEngine(),
        import("shiki/themes/github-light.mjs"),
        import("shiki/themes/github-dark.mjs"),
      ]);
      return createHighlighterCore({ themes: [light.default, dark.default], langs: [], engine });
    })();
    highlighterPromise.catch(() => {
      highlighterPromise = null;
    });
  }
  return highlighterPromise;
}

const languageLoads = new Map<string, Promise<boolean>>();

/**
 * Loads the grammar for `language` into `highlighter` and returns the id to pass to Shiki, or
 * `null` when the language is unknown or its grammar failed to load (callers render plain text).
 */
export async function ensureShikiLanguage(
  highlighter: HighlighterCore,
  language: string,
): Promise<string | null> {
  const resolved = resolveShikiLanguage(language);
  if (resolved === null || resolved === "text") return resolved;
  let load = languageLoads.get(resolved);
  if (!load) {
    const loader = LANGUAGE_LOADERS.get(resolved);
    if (!loader) return null;
    load = loader()
      .then((module) => highlighter.loadLanguage(module.default))
      .then(() => true);
    languageLoads.set(resolved, load);
    load.catch(() => languageLoads.delete(resolved));
  }
  try {
    return (await load) ? resolved : null;
  } catch {
    return null;
  }
}
