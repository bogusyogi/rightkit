// Contained relative-import resolver for MDX. The iframe sandbox is the FIRST wall (the script
// can't reach the app even if the import succeeds). This resolver is the SECOND wall: it fails
// closed on any spec that doesn't satisfy the contained + allowlisted + readable contract. Each
// successful read is brokered by Rust (read_relative_script + grant_relative_file), so the OS-
// level canonicalization + containment happens server-side at src-tauri/src/commands/html_assets.rs.

import { grantRelativeFile, readRelativeScript } from "@rightkit/document-core/state/tauriBridge";

const ALLOWED_EXT = /\.(mdx|md|js|jsx|ts|tsx)$/i;
const MAX_FILE_BYTES = 2_000_000; // 2 MB — same ceiling as read_relative_script (html_assets.rs)
const MAX_SPEC_LEN = 1024;

export type ResolveResult =
  | { kind: "source"; content: string; path: string }
  | { kind: "error"; code: ResolveErrorCode; message: string };

export type ResolveErrorCode =
  | "bare-import"
  | "remote-import"
  | "absolute-path"
  | "bad-spec"
  | "bad-extension"
  | "escapes-root"
  | "unreadable"
  | "too-large"
  | "internal";

interface ResolverContext {
  /** The opened .mdx file (or the file that issued the import) — the parent path for the Rust broker. */
  documentPath: string;
  /** The opened root .mdx file; Rust derives its parent folder as the contained import root. */
  rootPath: string;
  /** Reports a successful grant so the owning tab can revoke it on close (see useDocuments.ts's
   *  teardownDoc — mirrors the HTML sub-page grant-revocation pattern). Optional so existing callers
   *  and tests that don't care about revocation keep working unchanged. */
  onGrant?: (grantedPath: string) => void;
}

function joinRel(fromDir: string, spec: string): string {
  const parts = (fromDir ? `${fromDir}/` : "") + spec;
  const segments: string[] = [];
  for (const part of parts.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      // Do not silently strip; surface as an escape attempt so the parent gets the
      // bounded "escapes-root" error rather than the resolver rewriting paths. The Rust broker
      // is the source of truth on whether the path stays inside the contained root.
      segments.push("..");
      continue;
    }
    segments.push(part);
  }
  return segments.join("/");
}

/** Resolve an MDX `import` spec against the contained root. */
export async function resolveImport(spec: string, ctx: ResolverContext): Promise<ResolveResult> {
  if (typeof spec !== "string" || spec.length === 0) {
    return { kind: "error", code: "bad-spec", message: "Empty import specifier." };
  }
  if (spec.length > MAX_SPEC_LEN || spec.includes("\0")) {
    return { kind: "error", code: "bad-spec", message: "Invalid import specifier." };
  }

  // Remote URL specifier (http:, https:, data:, file:, etc.). The MDX iframe's CSP blocks fetch
  // anyway, but reject it at the resolver too so the user sees the reason.
  if (/^[a-z][a-z0-9+.-]*:/i.test(spec)) {
    return {
      kind: "error",
      code: "remote-import",
      message: `MDX cannot import "${spec}" — remote imports are not allowed.`,
    };
  }
  // Bare / npm package specifiers — no leading "./" or "/" or "..". MDX's intended `import "react"`
  // is rejected here; if a user wants that they need a real bundler, not the document viewer.
  if (!spec.startsWith(".") && !spec.startsWith("/")) {
    return {
      kind: "error",
      code: "bare-import",
      message: `MDX cannot import "${spec}" — bare package imports are not allowed. Use a relative path inside the opened file's folder.`,
    };
  }
  // Absolute filesystem paths. The user can use relative paths only.
  if (spec.startsWith("/")) {
    return {
      kind: "error",
      code: "absolute-path",
      message: `MDX cannot import "${spec}" — absolute paths are not allowed.`,
    };
  }

  const candidate = joinRel("", spec);
  if (candidate.split("/").includes("..")) {
    return {
      kind: "error",
      code: "escapes-root",
      message: `MDX import "${spec}" is outside the opened file's folder.`,
    };
  }
  if (!ALLOWED_EXT.test(candidate)) {
    return {
      kind: "error",
      code: "bad-extension",
      message: `MDX import "${spec}" must end in one of .mdx .md .js .jsx .ts .tsx`,
    };
  }

  // Containment + read are brokered in Rust. The frontend can't canonicalize paths in the browser,
  // and it MUSTN'T — canonicalization here would let a ".." escape slip through into the read.
  const granted = await grantRelativeFile(ctx.documentPath, spec, ctx.rootPath);
  if (!granted) {
    return {
      kind: "error",
      code: "escapes-root",
      message: `MDX import "${spec}" is outside the opened file's folder.`,
    };
  }
  ctx.onGrant?.(granted);
  const text = await readRelativeScript(ctx.documentPath, spec, ctx.rootPath);
  if (text == null) {
    return { kind: "error", code: "unreadable", message: `MDX could not read "${spec}".` };
  }
  if (text.length > MAX_FILE_BYTES) {
    return {
      kind: "error",
      code: "too-large",
      message: `MDX import "${spec}" exceeds the 2 MB cap.`,
    };
  }
  return { kind: "source", content: text, path: granted };
}

/** Build a wire-friendly VFile-compatible "source" payload the `@mdx-js/mdx` resolver expects. */
export function resolveImportToVFileSource(
  spec: string,
  ctx: ResolverContext,
): Promise<string | { errors: Array<{ message: string; code?: string }> }> {
  return resolveImport(spec, ctx).then((res) => {
    if (res.kind === "source") return res.content;
    const err = new Error(res.message) as Error & { code?: string; line?: number; column?: number };
    err.code = res.code;
    return { errors: [{ message: res.message, code: res.code }] };
  });
}
