import { invoke } from "@tauri-apps/api/core";

export interface MarkdownResourceInspectionRequest {
  readonly rawUrl: string;
  readonly needText: boolean;
}

export interface MarkdownResourceInspectionResult {
  readonly rawUrl: string;
  readonly pathPart: string;
  readonly exists: boolean;
  readonly isFile: boolean;
  readonly format: "md" | "markdown" | "mdx" | null;
  readonly grantedPath: string | null;
  readonly text: string | null;
  readonly error: string | null;
}

/** Calls native only for URLs already identified as local references. */
export async function inspectMarkdownResources(
  documentPath: string,
  requests: readonly MarkdownResourceInspectionRequest[],
): Promise<readonly MarkdownResourceInspectionResult[]> {
  if (!documentPath || requests.length === 0) return [];
  return invoke<MarkdownResourceInspectionResult[]>("inspect_markdown_resources", {
    documentPath,
    requests,
  });
}
