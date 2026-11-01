/**
 * Async validators for Markdown content.
 *
 * Math (KaTeX), Mermaid, and code-block language validators are
 * loaded lazily so the Markdown editor never blocks on a slow
 * validator. Validators return `null` for "no error" or an error
 * message string.
 */

import { resolveShikiLanguage } from "@rightkit/doc-render";

/**
 * KaTeX validator. Returns `null` when the value parses; otherwise
 * returns the parse error message.
 *
 * The implementation lazy-loads `katex` so the Markdown editor's
 * initial bundle stays small. Validators run on the main thread; if
 * KaTeX is not available, the validator returns a generic error
 * telling the caller to enable KaTeX.
 */
export async function validateKatex(value: string, displayMode: boolean): Promise<string | null> {
  try {
    const katex = await import("katex");
    katex.default.renderToString(value, { displayMode, throwOnError: true });
    return null;
  } catch (err) {
    return err instanceof Error ? err.message : String(err);
  }
}

/**
 * Mermaid validator. The implementation lazy-loads `mermaid`; if
 * the package is unavailable, the validator returns a generic
 * error. Otherwise it parses the diagram and returns `null`.
 */
export async function validateMermaid(code: string): Promise<string | null> {
  try {
    const mermaid = await import("mermaid");
    await mermaid.default.parse(code);
    return null;
  } catch (err) {
    return err instanceof Error ? err.message : String(err);
  }
}

/**
 * Shiki language availability check. Returns `null` when the
 * language or one of its aliases is in the shared highlighter's
 * curated grammar set; otherwise returns a short error.
 */
export async function validateCodeLanguage(language: string): Promise<string | null> {
  if (language.trim().length === 0) return null;
  if (resolveShikiLanguage(language) !== null) return null;
  return `Unknown language: ${language}.`;
}
