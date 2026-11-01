// D14 — MarkdownArtifactView.
//
// This is the persistent document surface for a workspace Markdown artifact.
// It follows the Hermes Agent preview contract: a rendered/source switch,
// refresh, and explicit reveal action. the host app existing MarkdownText
// renderer supplies the richer safe React Markdown grammar used by the
// transcript; raw HTML is never injected into the document.

import { useCallback, useEffect, useState } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import { useWorkspaceFile } from "@rightkit/agent-ui-hooks/hooks/useWorkspaceFile";
import { registerShortcut } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import MarkdownText from "../workbench/MarkdownText.js";

export interface MarkdownArtifactViewProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
  /** Any artifact/document descriptor with a workspace-relative path. */
  artifact: {
    path: string;
    kind?: string;
    title?: string | null;
  };
  /** Default mode on first render. Defaults to "rendered". */
  defaultMode?: "rendered" | "source";
  onReveal?: (path: string) => void;
}

interface Block {
  kind: "heading" | "paragraph" | "list" | "code" | "quote";
  text: string;
  level?: number;
  items?: string[];
  lang?: string;
}

export default function MarkdownArtifactView({
  config,
  workspaceRoot,
  artifact,
  defaultMode = "rendered",
  onReveal,
}: MarkdownArtifactViewProps) {
  const [mode, setMode] = useState<"rendered" | "source">(defaultMode);
  const path = artifact.path;
  const { status, response, reason, refresh } = useWorkspaceFile({
    config,
    workspaceRoot,
    path,
  });

  useEffect(
    () =>
      registerShortcut({
        id: "markdown.toggle_view",
        // Old matching: Ctrl or Meta (either, or both) with Shift; other modifiers ignored; no composing or
        // defaultPrevented guard. A held key does not repeat the toggle (a binding runs on repeats only with `repeat: true`).
        chord: "ctrlormeta+shift+m",
        label: "Toggle Markdown source",
        hidden: true,
        ignoreExtraModifiers: true,
        allowComposing: true,
        allowDefaultPrevented: true,
        run: () => {
          setMode((current) => (current === "rendered" ? "source" : "rendered"));
          return true;
        },
      }),
    [],
  );

  const handleReveal = useCallback(() => onReveal?.(path), [onReveal, path]);
  const hasFile = response?.kind === "file" && response.content !== null;
  const source = hasFile ? response.content ?? "" : "";

  const statusLabel =
    status === "loading"
      ? "Loading…"
      : status === "error"
        ? (reason ?? "Failed to read")
        : status === "unsupported"
          ? response?.kind === "binary"
            ? "Binary"
            : response?.kind === "tooLarge"
              ? "Too large"
              : "Unsupported"
          : hasFile
            ? "Ready"
            : "Not loaded";

  return (
    <section
      aria-label="Markdown artifact"
      data-testid="markdown-artifact"
      className="flex h-full min-h-0 flex-col"
    >
      <header className="flex flex-none items-center gap-2 border-b border-line px-3 py-2 text-[12px] text-text-2">
        <span className="truncate font-mono" data-testid="markdown-artifact-title">
          {artifact.title ?? path}
        </span>
        <span className="ml-auto text-[10px] text-text-3" role="status" aria-live="polite">
          {statusLabel}
        </span>
        <button
          type="button"
          onClick={() => setMode((current) => (current === "rendered" ? "source" : "rendered"))}
          data-testid="markdown-artifact-toggle"
          aria-label={`Show ${mode === "rendered" ? "source" : "rendered"} Markdown`}
          aria-pressed={mode === "source"}
          className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          {mode === "rendered" ? "Source" : "Rendered"}
        </button>
        <button
          type="button"
          onClick={handleReveal}
          disabled={!path.trim()}
          data-testid="markdown-artifact-reveal"
          className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:cursor-not-allowed disabled:opacity-50"
        >
          Reveal
        </button>
        <button
          type="button"
          onClick={() => void refresh()}
          data-testid="markdown-artifact-refresh"
          className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          Refresh
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto" data-testid="markdown-artifact-body">
        {!hasFile ? (
          <div className="px-3 py-4 text-[12px] text-text-2">
            {status === "error" ? (reason ?? "Failed to read") : "Loading artifact…"}
          </div>
        ) : mode === "rendered" ? (
          <article
            className="mx-auto max-w-3xl px-6 py-6 text-[13px] leading-relaxed text-text"
            data-testid="markdown-artifact-rendered"
          >
            {source.trim() ? <MarkdownText text={source} /> : <p className="text-text-3">(empty document)</p>}
          </article>
        ) : (
          <pre
            className="block whitespace-pre-wrap break-words px-4 py-4 font-mono text-[12px] leading-relaxed text-text"
            data-testid="markdown-artifact-source"
          >
            {source}
          </pre>
        )}
      </div>
    </section>
  );
}

// Kept as a small pure projection for callers/tests that need block identity;
// the visible renderer uses MarkdownText so tables, task lists, tilde fences,
// and inline reference handling stay consistent with the transcript.
export function parseMarkdown(input: string): Block[] {
  const lines = input.split(/\r?\n/);
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const headingMatch = /^(#{1,6})\s+(.+)$/.exec(line);
    if (headingMatch) {
      blocks.push({ kind: "heading", level: headingMatch[1].length, text: headingMatch[2].trim() });
      i += 1;
      continue;
    }
    if (line.startsWith("```")) {
      const lang = line.slice(3).trim();
      const start = i + 1;
      let end = start;
      while (end < lines.length && !lines[end].startsWith("```")) end += 1;
      blocks.push({ kind: "code", text: lines.slice(start, end).join("\n"), lang });
      i = end < lines.length ? end + 1 : end;
      continue;
    }
    if (/^[-*+]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^[-*+]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^[-*+]\s+/, "").trim());
        i += 1;
      }
      blocks.push({ kind: "list", text: "", items });
      continue;
    }
    if (line.startsWith(">")) {
      blocks.push({ kind: "quote", text: line.replace(/^>\s*/, "").trim() });
      i += 1;
      continue;
    }
    const paragraph: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() &&
      !/^(#{1,6})\s+/.test(lines[i]) &&
      !lines[i].startsWith("```") &&
      !/^[-*+]\s+/.test(lines[i]) &&
      !lines[i].startsWith(">")
    ) {
      paragraph.push(lines[i]);
      i += 1;
    }
    blocks.push({ kind: "paragraph", text: paragraph.join(" ") });
  }
  return blocks;
}

// Safe-href helper retained for typed-document consumers. The visible
// MarkdownText link renderer only creates external anchors for http(s)/mailto;
// this allowlist additionally supports in-workspace relative references.
const SAFE_HREF_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);

export function sanitizeHref(href: string): string | null {
  const trimmed = href.trim();
  if (!trimmed || [...trimmed].some((char) => char.charCodeAt(0) <= 31 || char.charCodeAt(0) === 127 || /\s/.test(char))) {
    return null;
  }
  const lower = trimmed.toLowerCase();
  if (["javascript:", "data:", "vbscript:", "file:", "blob:", "about:", "chrome:", "ms-appx:", "ms-appx-web:"]
    .some((blocked) => lower.startsWith(blocked))) {
    return null;
  }
  if (trimmed.startsWith("/") || trimmed.startsWith("#")) return trimmed;
  const colon = trimmed.indexOf(":");
  if (colon < 0) {
    return /^[A-Za-z0-9._~%-]+(?:\/[A-Za-z0-9._~%-]*)*$/.test(trimmed) ? trimmed : null;
  }
  return SAFE_HREF_PROTOCOLS.has(lower.slice(0, colon + 1)) ? trimmed : null;
}
