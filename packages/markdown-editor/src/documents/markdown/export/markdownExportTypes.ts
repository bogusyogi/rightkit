import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes.js";

export type MarkdownExportTheme = "current" | "light";

export interface MarkdownExportRequest {
  snapshot: MarkdownCoreSnapshot;
  fallbackTitle: string;
  documentPath?: string;
  safeHtmlEnabled?: boolean;
  theme: MarkdownExportTheme;
  remoteImages: "preserve" | "omit";
  timeoutMs?: number;
}

export interface MarkdownExportSnapshot {
  revision: number;
  title: string;
  htmlDocument: string;
  htmlFragment: string;
  plainText: string;
  warnings: readonly string[];
}

export interface MarkdownExportApi {
  readonly documentPath?: string;
  getRevision(): number;
  createSnapshot(
    options?: Partial<Pick<MarkdownExportRequest, "theme" | "remoteImages">>,
  ): Promise<MarkdownExportSnapshot>;
}
