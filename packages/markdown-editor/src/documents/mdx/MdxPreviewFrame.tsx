// The MDX preview pane. Mounts a sandboxed iframe (`sandbox="allow-scripts"`, never
// `allow-same-origin`) and ships the parent's compiled MDX into it. The parent compiles via
// mdxCompiler.ts; the iframe evaluates via the in-frame runtime in mdxBridgeScript.ts. Link
// clicks + find + height + runtime errors are brokered through the postMessage protocol in
// mdxProtocol.ts. The in-app error card renders compile-time errors in the parent's tree (not
// the iframe's) so the user can read + find + click into them.

import { useEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { compileMdx, type CompiledMdxModule, type MdxErrorKind } from "./mdxCompiler.js";
import { mdxBridgeScript } from "./mdxBridgeScript.js";
import { readMdxThemeVars } from "./mdxTheme.js";
import {
  isMdxCompileError,
  isMdxFindOpen,
  isMdxFindResult,
  isMdxHeightMessage,
  isMdxLinkMessage,
  isMdxPrintContent,
  isMdxRuntimeError,
  postMdxCompiled,
  postMdxPrint,
  postMdxTheme,
} from "./mdxProtocol.js";
import { useMdxPreviewFind } from "./useMdxPreviewFind.js";
import { openExternal, grantRelativeFile } from "@rightkit/document-core/state/tauriBridge";
import { normalizeLivePreviewUrl } from "@rightkit/document-core/documents/web/webUrl";
import { formatFromPath } from "@rightkit/document-core/documents/types";
import { logEvent, logError } from "@rightkit/document-core/state/diagnostics";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { toast } from "@rightkit/platform-ui/headless";
import { printHtmlMarkup } from "@rightkit/document-core/app/documentPrinting";
import { acquireResource } from "@rightkit/document-core/app/resourceCensus";
import { usePreviewFrameSrc } from "@rightkit/document-core/documents/preview/usePreviewFrameSrc";
import { documentEvent } from "@rightkit/document-core/host";

interface Props {
  source: string;
  /** Absolute path of the opened .mdx file — the entry root for contained imports and link grants. */
  documentPath: string;
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  registerSearchProvider: (provider: import("@rightkit/document-core/documents/types").SearchProvider | null) => void;
  onError?: (error: { kind: MdxErrorKind; message: string }) => void;
  /** Reports every path granted for this document (compile-time imports + in-preview link clicks) so
   *  the owning tab can revoke them on close (see useDocuments.ts's teardownDoc). Optional — a caller
   *  that doesn't track grants (e.g. a future standalone preview) simply skips revocation. */
  onImportGranted?: (entryPath: string, grantedPath: string) => void;
  active?: boolean;
  printPreview?: boolean;
}

function errorTitle(kind: MdxErrorKind): string {
  switch (kind) {
    case "bare-import":
      return "Bare import not allowed";
    case "remote-import":
      return "Remote import not allowed";
    case "absolute-path":
      return "Absolute path not allowed";
    case "bad-extension":
      return "Unsupported import extension";
    case "escapes-root":
      return "Import escapes the file's folder";
    case "unreadable":
      return "Could not read import";
    case "too-large":
      return "Import too large";
    case "syntax":
      return "MDX syntax error";
    case "internal":
      return "MDX runtime error";
    default:
      return "MDX preview error";
  }
}

function buildIframeSrcDoc(sessionId: string): string {
  // CSP: the AUTHORITATIVE policy is the response header served by src-tauri/src/preview_protocol.rs
  // (MDX_CSP); this meta copy must stay identical and only backs the plain-browser srcdoc fallback.
  // default-src 'none' so the iframe can ONLY run the inline bridge + the contained MDX module
  // graph. The runtime evaluates compiler-produced local code with new Function, so unsafe-eval is
  // required inside this opaque sandbox. No network, no fetch, no forms, no base URI override.
  // Image/font/style is data: only — MDX content shouldn't pull external assets. The MDX threat
  // model is "local code in an opaque sandbox" — different from the interactive HTML preview,
  // which allows http(s) for CDN libs.
  const csp =
    "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' blob:; style-src 'unsafe-inline'; img-src data:; font-src data:; base-uri 'none'; form-action 'none'";
  const script = mdxBridgeScript(sessionId);
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <title>MDX preview</title>
  <style>
    html,body{margin:0;padding:0;background:transparent;color:var(--vr-fg,#2d2923);font-family:Georgia,serif;font-size:16px;line-height:1.7;}
    #root{box-sizing:border-box;width:100%;max-width:96rem;margin:0 auto;padding:2.5rem 3rem;min-height:100vh;}
    h1,h2,h3,h4,h5,h6{margin:1.15em 0 .55em;color:var(--vr-heading,#231f1a);font-weight:760;line-height:1.16;}
    h1{margin-top:0;font-size:2.7rem;} h2{font-size:2rem;} h3{font-size:1.55rem;}
    p{margin:0 0 1em;} strong{font-weight:760;} em{font-style:italic;} a{color:var(--vr-link,#7a3f1f);text-decoration:underline;text-underline-offset:.18em;}
    blockquote{margin:1.1em 0;padding:.7em 1em;border:1px solid var(--vr-quote-line,#d7cfbd);border-radius:6px;color:var(--vr-muted,#5e5649);}
    ul,ol{margin:0 0 1em 1.4em;padding:0;} li{margin:.12em 0;} li>p{margin:.2em 0;}
    :not(pre)>code{border:1px solid var(--vr-line,#ddd4c1);border-radius:5px;background:var(--vr-code-bg,#eee8da);padding:.06em .28em;font-family:ui-monospace,monospace;font-size:.82em;color:var(--vr-code-fg,#6f3b20);}
    pre,.vr-mdx-code{margin:1em 0;overflow:auto;border:1px solid var(--vr-line,#ddd4c1);border-radius:8px;background:var(--vr-code-bg,#eee8da);padding:12px 14px;font-family:ui-monospace,monospace;font-size:.82em;line-height:1.55;white-space:pre-wrap;}
    .vr-mdx-code pre{margin:0;border:0;padding:0;background:transparent;}
    .vr-mdx-code-label{margin-bottom:.5em;font-size:.72em;letter-spacing:.14em;text-transform:uppercase;color:var(--vr-label,#7d7364);}
    table{border-collapse:collapse;margin:1em 0;width:100%;font-size:.95em;} th,td{border-bottom:1px solid var(--vr-line,#ddd4c1);padding:.35em .55em;text-align:left;vertical-align:top;} th{font-weight:760;color:var(--vr-heading,#231f1a);}
    .vr-mdx-table-wrap{overflow-x:auto;margin:1em 0;} .vr-mdx-frontmatter{display:grid;grid-template-columns:max-content 1fr;gap:.25em 1.4em;margin:1em 0;} .vr-mdx-frontmatter dt{font-weight:760;}
    .vr-mdx-callout{margin:1em 0;padding:.75em 1em;background:var(--vr-callout-bg,#f3eee2);border-radius:6px}.vr-mdx-label{margin-bottom:.35em;font-family:ui-monospace,monospace;font-size:.75em;text-transform:uppercase;letter-spacing:.12em}
    .vr-mdx-unknown,.vr-mdx-runtime-error{margin:.5rem 0;padding:.75rem;border:1px solid var(--vr-error,#c46a3d);border-radius:6px;background:rgba(196,106,61,.08);font-family:ui-monospace,monospace;font-size:.85em;white-space:pre-wrap;word-break:break-word;}
  </style>
</head>
<body>
  <div id="root"></div>
  <script data-vr-mdx-bridge>${script}</script>
</body>
</html>`;
}

export function MdxPreviewFrame({
  source,
  documentPath,
  onOpenPath,
  registerSearchProvider,
  onError,
  onImportGranted,
  active = true,
  printPreview = true,
}: Props) {
  const sessionId = useMemo(() => safeRandomUUID(), []);
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  useEffect(() => {
    const release = acquireResource({
      kind: "mdx-iframe",
      documentPath,
      format: "mdx",
    });
    return release;
  }, [documentPath]);
  const [compileState, setCompileState] = useState<
    | { phase: "idle" }
    | { phase: "compiling" }
    | { phase: "ok" }
    | {
        phase: "error";
        error: { kind: MdxErrorKind; message: string; line?: number; column?: number };
      }
  >({ phase: "idle" });
  const [runtimeError, setRuntimeError] = useState<{ name?: string; message: string } | null>(null);
  const [compiledBundle, setCompiledBundle] = useState<{
    entryId: string;
    modules: Record<string, CompiledMdxModule>;
  } | null>(null);
  const [frameLoaded, setFrameLoaded] = useState(false);

  const debouncedSource = useDebouncedValue(source, 150);
  const builtSrcDoc = useMemo(() => buildIframeSrcDoc(sessionId), [sessionId]);
  // Served from the viewright-preview protocol under its own CSP header, so the main window never
  // needs 'unsafe-eval'. Falls back to srcdoc only outside Tauri (browser QA).
  const frameSource = usePreviewFrameSrc("mdx", builtSrcDoc);

  // ── Find: register an `mdx-preview` provider that drives the in-frame find engine over postMessage.
  const { resolveFindResult } = useMdxPreviewFind({
    iframeRef,
    sessionId,
    ready: compileState.phase === "ok" && !runtimeError,
    registerSearchProvider,
  });

  useEffect(() => {
    const onPrint = (event: Event) => {
      if (!active || !printPreview || !iframeRef.current) return;
      event.preventDefault();
      // Ask the frame for its rendered markup rather than printing it directly: the iframe is pinned
      // to the on-screen content-pane box, so contentWindow.print() would paginate against that fixed
      // viewport, not the full document (the isMdxPrintContent branch above handles the reply).
      postMdxPrint(iframeRef.current, sessionId);
    };
    window.addEventListener(documentEvent("print"), onPrint);
    return () => window.removeEventListener(documentEvent("print"), onPrint);
  }, [active, printPreview, sessionId]);

  // ── Compile: debounce source → call mdxCompiler → post the function-body to the iframe.
  useEffect(() => {
    let cancelled = false;
    setCompileState({ phase: "compiling" });
    setRuntimeError(null);
    setCompiledBundle(null);
    compileMdx(debouncedSource, {
      documentPath,
      rootPath: documentPath,
      onGrant: onImportGranted
        ? (grantedPath) => onImportGranted(documentPath, grantedPath)
        : undefined,
    })
      .then((res) => {
        if (cancelled) return;
        if (res.ok) {
          setCompileState({ phase: "ok" });
          setCompiledBundle({ entryId: res.entryId, modules: res.modules });
        } else {
          setCompileState({ phase: "error", error: res.error });
          onError?.(res.error);
          logEvent("mdx:compile-error");
        }
      })
      .catch((e) => {
        if (cancelled) return;
        const message = errorMessage(e);
        setCompileState({ phase: "error", error: { kind: "internal", message } });
        onError?.({ kind: "internal", message });
        logEvent("mdx:compile-error");
      });
    return () => {
      cancelled = true;
    };
  }, [debouncedSource, documentPath, onError, onImportGranted]);

  // ── Theme: push the app's resolved colors into the frame on load and whenever the app theme
  // changes (<html data-theme> or the inline token styles applied by settings).
  useEffect(() => {
    if (!frameLoaded) return;
    const root = document.documentElement;
    const push = () => postMdxTheme(iframeRef.current, sessionId, readMdxThemeVars(root));
    push();
    const observer = new MutationObserver(push);
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme", "style"] });
    return () => observer.disconnect();
  }, [frameLoaded, sessionId]);

  useEffect(() => {
    if (!frameLoaded || compileState.phase !== "ok" || !compiledBundle) return;
    postMdxCompiled(iframeRef.current, sessionId, compiledBundle.entryId, compiledBundle.modules);
  }, [compiledBundle, compileState.phase, frameLoaded, sessionId]);

  // ── Inbound messages: links, find results, runtime errors, height, find-open. Guarded on
  // event.source === iframe.contentWindow + sessionId === sessionId.
  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const iframe = iframeRef.current;
      if (!iframe || event.source !== iframe.contentWindow) return;
      const data = event.data as unknown;
      if (isMdxLinkMessage(data, sessionId)) {
        void handleHref(data.href, !!data.newTab, documentPath, onOpenPath, onImportGranted);
        return;
      }
      if (isMdxFindResult(data, sessionId)) {
        resolveFindResult(data.requestId, { count: data.count, activeIndex: data.activeIndex });
        return;
      }
      if (isMdxFindOpen(data, sessionId)) {
        // Reuse the same App-level Ctrl+F handler; the FindBar opens against the active tab.
        window.dispatchEvent(new CustomEvent(documentEvent("open-find")));
        return;
      }
      if (isMdxRuntimeError(data, sessionId)) {
        setRuntimeError({ name: data.name, message: data.message });
        logEvent("mdx:runtime-error");
        return;
      }
      if (isMdxCompileError(data, sessionId)) {
        setCompileState({
          phase: "error",
          error: {
            kind: data.kind as MdxErrorKind,
            message: data.message,
            line: data.line,
            column: data.column,
          },
        });
        return;
      }
      if (isMdxHeightMessage(data, sessionId)) {
        // The frame reports its content height via ResizeObserver; the parent uses it to size
        // the iframe element. We don't currently surface this anywhere (the iframe is full-bleed
        // within the preview scroll container), so the message is consumed but not acted on.
        return;
      }
      // The frame replied to postMdxPrint with its own serialized document — print THAT in-flow
      // instead of the frame's viewport-sized iframe box (see mdxProtocol.ts for the rationale).
      if (isMdxPrintContent(data, sessionId)) {
        void printHtmlMarkup(data.html).then((printed) => {
          if (printed) return;
          logError("print:failed");
          toast("Couldn't open print dialog", "error");
        });
        return;
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [sessionId, documentPath, onOpenPath, resolveFindResult, onImportGranted]);

  // Reload handler — bumps nonce to remount the iframe (the bridge can't be hot-reloaded safely).
  // Not wired to a UI affordance in v1; the editor's "Reload" shortcut can be added later.

  const onContainerClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    // Catch clicks on the in-app error card so the user can read/copy it. No navigation.
    event.stopPropagation();
  };

  return (
    <div className="relative h-full min-h-0 min-w-0" onClick={onContainerClick}>
      {compileState.phase === "compiling" && (
        <div className="pointer-events-none absolute inset-0 flex items-start justify-center p-4 text-xs text-muted-2">
          Compiling…
        </div>
      )}
      {compileState.phase === "error" && (
        <MdxErrorCard error={compileState.error} documentPath={documentPath} />
      )}
      {runtimeError && compileState.phase !== "error" && (
        <MdxErrorCard
          error={{ kind: "internal", message: runtimeError.message }}
          documentPath={documentPath}
        />
      )}
      {frameSource.mode === "failed" && (
        <MdxErrorCard
          error={{ kind: "internal", message: `Preview unavailable: ${frameSource.message}` }}
          documentPath={documentPath}
        />
      )}
      {(frameSource.mode === "src" || frameSource.mode === "srcdoc") && (
        <iframe
          ref={iframeRef}
          title="MDX preview"
          sandbox="allow-scripts allow-modals"
          {...(frameSource.mode === "src" ? { src: frameSource.src } : { srcDoc: builtSrcDoc })}
          onLoad={() => setFrameLoaded(true)}
          className="h-full w-full border-0"
        />
      )}
    </div>
  );
}

function MdxErrorCard({
  error,
  documentPath,
}: {
  error: { kind: MdxErrorKind; message: string; line?: number; column?: number };
  documentPath: string;
}) {
  return (
    <div
      className="absolute inset-0 z-10 m-4 max-h-fit rounded-lg border border-copper/60 bg-panel/95 p-4 font-mono text-sm shadow-xl"
      data-vr-mdx-error-card
      data-testid="vr-mdx-error-card"
    >
      <div className="mb-2 flex items-center gap-2">
        <span className="rounded bg-copper/20 px-2 py-0.5 text-xs uppercase tracking-wide text-copper-hi">
          {errorTitle(error.kind)}
        </span>
        <span className="text-xs text-muted-2">{baseName(documentPath)}</span>
      </div>
      <pre className="whitespace-pre-wrap break-words text-paper/90">{error.message}</pre>
      {(error.line || error.column) && (
        <div className="mt-2 text-xs text-muted-2">
          {error.line ? `line ${error.line}` : ""}
          {error.line && error.column ? ":" : ""}
          {error.column ? `col ${error.column}` : ""}
        </div>
      )}
      <div className="mt-3 text-xs text-muted-2">
        {error.kind === "bare-import" &&
          "Hint: import relative files only — like ./Chart.jsx or ../shared/Footer.tsx."}
        {error.kind === "remote-import" &&
          "Hint: MDX cannot import remote URLs in the document viewer. Use HTML for that."}
        {error.kind === "absolute-path" && "Hint: use a relative path like ./component.jsx."}
        {error.kind === "escapes-root" && "Hint: the import must stay inside this file's folder."}
        {error.kind === "unreadable" && "Hint: check the file exists and is readable."}
        {error.kind === "too-large" && "Hint: split the import into smaller files."}
        {error.kind === "syntax" &&
          "Hint: check the MDX syntax — JSX expressions must be balanced."}
        {error.kind === "bad-extension" && "Hint: import only .mdx .md .js .jsx .ts .tsx files."}
        {error.kind === "internal" && "Hint: try reloading the document."}
      </div>
    </div>
  );
}

// ── Helpers ──────────────────────────────────────────────────────────────────────

function safeRandomUUID(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `mdx-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  }
}

function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const id = window.setTimeout(() => setDebounced(value), delayMs);
    return () => window.clearTimeout(id);
  }, [value, delayMs]);
  return debounced;
}

function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

async function handleHref(
  href: string,
  newTab: boolean,
  documentPath: string,
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void,
  onImportGranted?: (entryPath: string, grantedPath: string) => void,
): Promise<void> {
  if (!href || href.startsWith("#")) return;

  // External URL → VR web tab (foreground or background). Falls back to OS browser if the URL
  // isn't a "live preview" URL (matches useHtmlNavigation.ts:72-81).
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(href) || href.startsWith("//")) {
    const normalized = normalizeLivePreviewUrl(href);
    if (normalized) {
      window.dispatchEvent(
        new CustomEvent(documentEvent("open-url-tab"), {
          detail: newTab ? { url: normalized, background: true } : normalized,
        }),
      );
      return;
    }
    await openExternal(href);
    return;
  }

  // Local relative path → grant via the broker, then open as a tab. If formatFromPath returns
  // "mdx" we navigate-in-place; for everything else we open as a new tab.
  const granted = await grantRelativeFile(documentPath, href, documentPath);
  if (!granted) {
    logEvent("mdx:link-blocked");
    return;
  }
  onImportGranted?.(documentPath, granted);
  if (!newTab && formatFromPath(granted) === "mdx") {
    // The MDX preview doesn't have an in-tab navigation system (the iframe is per-document). Open
    // sibling .mdx files as new tabs so the user can read them side-by-side.
    onOpenPath?.(granted, { background: true });
    return;
  }
  onOpenPath?.(granted, newTab ? { background: true } : undefined);
}
