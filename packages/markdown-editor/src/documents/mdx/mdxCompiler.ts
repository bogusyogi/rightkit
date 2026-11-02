// @mdx-js/mdx compile path. Lazy — only loaded when an .mdx file is opened. The parent compiles
// the root document plus its contained local import graph, then ships that graph into the sandboxed
// iframe. The iframe never reads the filesystem directly; it can execute only the precompiled graph.

import remarkFrontmatter from "remark-frontmatter";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { transform as sucraseTransform } from "sucrase";
import { resolveImport } from "./mdxResolver.js";

type Compile = (source: string, options?: Record<string, unknown>) => Promise<unknown>;

let compilePromise: Promise<Compile> | null = null;

async function loadCompile(): Promise<Compile> {
  if (!compilePromise) {
    compilePromise = import("@mdx-js/mdx").then((m) => (m as { compile: Compile }).compile);
  }
  return compilePromise;
}

export type MdxErrorKind =
  | "syntax"
  | "bare-import"
  | "remote-import"
  | "absolute-path"
  | "bad-spec"
  | "bad-extension"
  | "escapes-root"
  | "unreadable"
  | "too-large"
  | "internal";

export interface CompiledMdxModule {
  kind: "mdx" | "cjs";
  code: string;
}

export type CompileResult =
  | {
      ok: true;
      entryId: string;
      modules: Record<string, CompiledMdxModule>;
    }
  | { ok: false; error: { kind: MdxErrorKind; message: string; line?: number; column?: number } };

interface CompileContext {
  documentPath: string;
  rootPath: string;
  /** Forwarded to the resolver so every grant minted while compiling the import graph is reported
   *  to the owning tab for revocation on close. */
  onGrant?: (grantedPath: string) => void;
}

interface ResolvedModule {
  path: string;
  source: string;
}

const IMPORT_LIKE =
  /(?:^|\n)\s*(?:import\s+[^"'`\n]+\s+from\s*|import\s*\(\s*|import\s*|export\s+[^"'`\n]+\s+from\s*)(["'])([^"'`\n]+)\1/g;

/** Compile MDX plus its contained import graph into a sandbox-ready module map. */
export async function compileMdx(source: string, ctx: CompileContext): Promise<CompileResult> {
  const modules: Record<string, CompiledMdxModule> = {};
  const seen = new Set<string>();
  try {
    await compileContainedModule(
      { path: ctx.documentPath, source },
      ctx.rootPath,
      modules,
      seen,
      ctx.onGrant,
    );
    return {
      ok: true,
      entryId: syntheticModuleId(ctx.documentPath),
      modules,
    };
  } catch (e) {
    return mapCompileError(e);
  }
}

async function compileContainedModule(
  module: ResolvedModule,
  rootPath: string,
  modules: Record<string, CompiledMdxModule>,
  seen: Set<string>,
  onGrant?: (grantedPath: string) => void,
): Promise<void> {
  const moduleId = syntheticModuleId(module.path);
  if (seen.has(moduleId)) return;
  seen.add(moduleId);

  const imports = await collectResolvedImports(module.source, {
    documentPath: module.path,
    rootPath,
    onGrant,
  });
  for (const child of imports) {
    await compileContainedModule(child, rootPath, modules, seen, onGrant);
  }

  if (isMdxLike(module.path)) {
    const compile = await loadCompile();
    const code = await compile(module.source, {
      outputFormat: "function-body",
      development: false,
      baseUrl: moduleId,
      remarkPlugins: [remarkFrontmatter, remarkGfm, remarkMath],
    });
    modules[moduleId] = {
      kind: "mdx",
      code: rewriteCompiledMdx(String(code)),
    };
    return;
  }

  modules[moduleId] = {
    kind: "cjs",
    code: await transpileScriptModule(module.source, module.path),
  };
}

async function collectResolvedImports(
  source: string,
  ctx: CompileContext,
): Promise<ResolvedModule[]> {
  const specs: string[] = [];
  for (const match of source.matchAll(IMPORT_LIKE)) {
    const spec = match[2];
    if (spec) specs.push(spec);
  }
  // Each spec's resolution is an independent broker round-trip (grant_relative_file /
  // read_relative_script) — resolve them concurrently instead of one at a time. Promise.all
  // rejects with the first spec whose resolution errors, preserving the original throw-on-error
  // semantics (under concurrency "first" means first to actually reject, not necessarily first
  // in source order — acceptable, since any error still aborts the whole compile).
  const resolved = await Promise.all(
    specs.map(async (spec) => {
      const res = await resolveImport(spec, ctx);
      if (res.kind === "error") {
        throw typedError(res.code, res.message);
      }
      return res;
    }),
  );
  const out: ResolvedModule[] = [];
  const seen = new Set<string>();
  for (const res of resolved) {
    if (seen.has(res.path)) continue;
    seen.add(res.path);
    out.push({ path: res.path, source: res.content });
  }
  return out;
}

async function transpileScriptModule(source: string, fileName: string): Promise<string> {
  const ext = fileName.split(".").pop()?.toLowerCase() ?? "";
  const transforms = ["imports"] as Array<"imports" | "jsx" | "typescript">;
  if (ext === "jsx" || ext === "tsx") transforms.push("jsx");
  if (ext === "ts" || ext === "tsx") transforms.push("typescript");
  try {
    return sucraseTransform(source, {
      filePath: fileName,
      transforms,
      jsxRuntime: "classic",
      jsxPragma: "createElement",
      jsxFragmentPragma: "Fragment",
      production: true,
      disableESTransforms: true,
    }).code;
  } catch (error) {
    throw typedError("syntax", (error as Error).message ?? "MDX script compile failed.");
  }
}

function rewriteCompiledMdx(code: string): string {
  return `const __vrRequire = arguments[1];\n${code.replace(/await import\(/g, "__vrRequire(")}`;
}

function typedError(
  code: MdxErrorKind,
  message: string,
  line?: number,
  column?: number,
): Error & { code: MdxErrorKind; line?: number; column?: number } {
  const err = new Error(message) as Error & {
    code: MdxErrorKind;
    line?: number;
    column?: number;
  };
  err.code = code;
  err.line = line;
  err.column = column;
  return err;
}

function mapCompileError(e: unknown): CompileResult {
  const err = e as {
    code?: string;
    message?: string;
    line?: number;
    column?: number;
  };
  const code = (err.code ?? "") as MdxErrorKind;
  const knownKinds: MdxErrorKind[] = [
    "bare-import",
    "remote-import",
    "absolute-path",
    "bad-spec",
    "bad-extension",
    "escapes-root",
    "unreadable",
    "too-large",
    "syntax",
  ];
  return {
    ok: false,
    error: {
      kind: (knownKinds as string[]).includes(code) ? code : "internal",
      message: err.message ?? "MDX compile failed.",
      line: typeof err.line === "number" ? err.line : undefined,
      column: typeof err.column === "number" ? err.column : undefined,
    },
  };
}

function isMdxLike(path: string): boolean {
  return /\.(mdx|md)$/i.test(path);
}

export function syntheticModuleId(documentPath: string): string {
  const path = documentPath.replace(/\\/g, "/").replace(/^\/*/, "");
  return `https://viewright.invalid/${encodeURI(path || "document.mdx")}`;
}

export function resolveSyntheticModuleId(spec: string, parentId: string): string {
  if (/^https:\/\/viewright\.invalid\//i.test(spec)) return spec;
  return new URL(spec, parentId).href;
}
