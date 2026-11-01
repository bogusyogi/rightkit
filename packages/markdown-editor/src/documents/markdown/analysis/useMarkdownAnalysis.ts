/**
 * React hook that drives the analysis layer.
 *
 * Subscribes to a {@link import("../core/MarkdownDocumentCore").MarkdownDocumentCore}
 * and re-runs {@link analyzeMarkdown} on every commit. The hook is
 * the canonical way for React consumers to read the issue list.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { analyzeMarkdown } from "./analyzeMarkdown";
import { makeIssueId, type MarkdownAnalysis, type MarkdownIssue } from "./types";
import { isLocalMarkdownReference } from "./markdownUrls";
import { inspectMarkdownResources } from "./markdownResourceClient";
import { MarkdownValidationOrchestrator } from "./markdownAnalysisOrchestrator";
import { validateKatex, validateMermaid } from "./validateMarkdown";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes";

/**
 * Options for {@link useMarkdownAnalysis}.
 */
export interface UseMarkdownAnalysisOptions {
  readonly core: MarkdownDocumentCore | undefined;
  readonly safeHtml?: boolean;
  /** Open-document path; absent for untitled documents, which never inspect disk. */
  readonly documentPath?: string;
}

/**
 * Hook return value.
 */
export interface UseMarkdownAnalysisResult {
  readonly analysis: MarkdownAnalysis | null;
  readonly pending: boolean;
  readonly inspectionError: string | null;
}

/**
 * Subscribe to a core and re-derive the analysis on every commit.
 */
export function useMarkdownAnalysis(
  options: UseMarkdownAnalysisOptions,
): UseMarkdownAnalysisResult {
  const [snapshot, setSnapshot] = useState<MarkdownCoreSnapshot | null>(
    options.core ? options.core.snapshot() : null,
  );
  const [asyncIssues, setAsyncIssues] = useState<readonly MarkdownIssue[]>([]);
  const [resourceIssues, setResourceIssues] = useState<readonly MarkdownIssue[]>([]);
  const [pending, setPending] = useState(false);
  const [inspectionError, setInspectionError] = useState<string | null>(null);
  const validator = useRef<MarkdownValidationOrchestrator<
    MarkdownCoreSnapshot,
    readonly MarkdownIssue[]
  > | null>(null);
  if (!validator.current) validator.current = new MarkdownValidationOrchestrator(validateSnapshot);
  useEffect(() => {
    const core = options.core;
    if (!core) return;
    setSnapshot(core.snapshot());
    const unsubscribe = core.subscribe(0, () => {
      setSnapshot(core.snapshot());
    });
    return unsubscribe;
  }, [options.core]);
  const synchronousAnalysis = useMemo(() => {
    if (!snapshot) return null;
    return analyzeMarkdown(snapshot, { safeHtml: options.safeHtml });
  }, [snapshot, options.safeHtml]);
  useEffect(() => {
    if (!snapshot) return;
    let active = true;
    setAsyncIssues([]);
    setPending(true);
    const timer = window.setTimeout(() => {
      void validator.current?.validate(snapshot.revision, snapshot).then((result) => {
        if (!active || !result || result.revision !== snapshot.revision) return;
        setAsyncIssues(result.value);
        setPending(false);
      });
    }, 250);
    return () => {
      active = false;
      window.clearTimeout(timer);
      validator.current?.invalidate();
    };
  }, [snapshot]);
  useEffect(() => {
    if (!snapshot || !synchronousAnalysis || !options.documentPath) {
      setResourceIssues([]);
      return;
    }
    setResourceIssues([]);
    const requests = synchronousAnalysis.references
      .filter((reference) => isLocalMarkdownReference(reference.rawUrl))
      .map((reference) => ({ rawUrl: reference.rawUrl, needText: reference.fragment !== null }));
    if (requests.length === 0) {
      setResourceIssues([]);
      return;
    }
    let active = true;
    const timer = window.setTimeout(() => {
      void inspectMarkdownResources(options.documentPath as string, requests)
        .then((results) => {
          if (!active) return;
          const byUrl = new Map(
            synchronousAnalysis.references.map((reference) => [reference.rawUrl, reference]),
          );
          setResourceIssues(
            results.flatMap((result) => {
              const reference = byUrl.get(result.rawUrl);
              if (!reference || result.exists || result.error === "unsupported-format") return [];
              const code =
                reference.kind === "image" ? "md.image.missing-target" : "md.link.missing-target";
              return [
                {
                  id: makeIssueId(code, reference.nodeId, reference.range),
                  code,
                  severity: "warning",
                  message: "Local target is missing or unavailable.",
                  range: reference.range,
                },
              ];
            }),
          );
          setInspectionError(null);
        })
        .catch((error: unknown) => {
          if (active) {
            setResourceIssues([]);
            setInspectionError(error instanceof Error ? error.message : String(error));
          }
        });
    }, 350);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [snapshot, synchronousAnalysis, options.documentPath]);
  const analysis = useMemo(
    () =>
      !synchronousAnalysis
        ? null
        : {
            ...synchronousAnalysis,
            syntaxIssues: [...synchronousAnalysis.syntaxIssues, ...asyncIssues, ...resourceIssues],
          },
    [synchronousAnalysis, asyncIssues, resourceIssues],
  );
  return { analysis, pending, inspectionError };
}

async function validateSnapshot(snapshot: MarkdownCoreSnapshot): Promise<readonly MarkdownIssue[]> {
  const source = snapshot.logicalSource;
  type Finding = {
    node: MarkdownCoreSnapshot["sourceMap"]["nodes"][number];
    code: "md.math.invalid" | "md.mermaid.invalid";
    message: string | null;
  };
  const work: Array<Promise<Finding>> = [];
  for (const node of snapshot.sourceMap.nodes) {
    const raw = source.slice(node.range.from, node.range.to);
    if (node.kind === "math" || node.kind === "inline-math") {
      work.push(
        validateKatex(raw.replace(/^\${1,2}|\${1,2}$/g, ""), node.kind === "math").then(
          (message) => ({ node, code: "md.math.invalid", message }),
        ),
      );
    } else if (node.kind === "code" && /^```mermaid\b/i.test(raw)) {
      work.push(
        validateMermaid(raw.replace(/^```[^\n]*\n?|\n?```$/g, "")).then((message) => ({
          node,
          code: "md.mermaid.invalid",
          message,
        })),
      );
    }
  }
  const findings = await Promise.all(work);
  return findings.flatMap(({ node, code, message }) =>
    !message
      ? []
      : [
          {
            id: makeIssueId(code, node.id, node.range),
            code,
            severity: "warning" as const,
            message,
            range: node.range,
            nodeId: node.id,
          },
        ],
  );
}
