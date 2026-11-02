import { createRoot } from "react-dom/client";
import { MarkdownExportDocument } from "./MarkdownExportDocument.js";
import { MarkdownRenderTracker, waitForStableMarkdownRender } from "./MarkdownRenderTracker.js";
import { snapshotMarkdownDom } from "./exportSnapshotDom.js";
import { buildMarkdownStandaloneHtml } from "./markdownStandaloneHtml.js";
import type { MarkdownExportRequest, MarkdownExportSnapshot } from "./markdownExportTypes.js";
import { markdownToPlainText } from "../exportMarkdown.js";
import { readFrontmatterDocument } from "../properties/frontmatterDocument.js";

let exportQueue: Promise<void> = Promise.resolve();

const bounded = async <T,>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> => {
  let timer = 0;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(() => reject(new Error(`${label} timed out`)), timeoutMs);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    window.clearTimeout(timer);
  }
};

const enqueueExport = <T,>(run: () => Promise<T>): Promise<T> => {
  const result = exportQueue.then(run, run);
  exportQueue = result.then(
    () => undefined,
    () => undefined,
  );
  return result;
};

const titleFor = (request: MarkdownExportRequest) => {
  const frontmatter = readFrontmatterDocument(request.snapshot.logicalSource);
  const value = frontmatter.parsed?.get("title");
  return typeof value === "string" && value.trim() ? value.trim() : request.fallbackTitle;
};

async function decodeImages(article: HTMLElement): Promise<void> {
  for (const image of article.querySelectorAll("img")) {
    if (typeof image.decode !== "function") continue;
    await bounded(image.decode(), 5_000, `Image ${image.alt || image.src}`);
  }
}

export function renderMarkdownExport(
  request: MarkdownExportRequest,
): Promise<MarkdownExportSnapshot> {
  return enqueueExport(async () => {
    const host = document.createElement("div");
    host.dataset.vrMarkdownExportHost = "";
    Object.assign(host.style, {
      position: "fixed",
      left: "-100000px",
      top: "0",
      width: "960px",
      minHeight: "1px",
      opacity: "0",
      pointerEvents: "none",
      zIndex: "-1",
    });
    if (request.theme === "light") host.dataset.theme = "light";
    document.body.append(host);
    const root = createRoot(host);
    const tracker = new MarkdownRenderTracker();
    let mountedResolve: (() => void) | null = null;
    const mounted = new Promise<void>((resolve) => {
      mountedResolve = resolve;
    });
    try {
      root.render(
        <MarkdownExportDocument
          snapshot={request.snapshot}
          documentPath={request.documentPath}
          tracker={tracker}
          safeHtmlEnabled={request.safeHtmlEnabled ?? false}
          onMounted={() => mountedResolve?.()}
        />,
      );
      await bounded(mounted, 2_000, "Markdown export mount");
      if (document.fonts?.ready) await bounded(document.fonts.ready, 2_000, "Font loading");
      const trackerWarnings = await waitForStableMarkdownRender(
        tracker,
        request.timeoutMs ?? 10_000,
      );
      const article = host.querySelector<HTMLElement>("[data-vr-markdown-document]");
      if (!article) throw new Error("Markdown export document did not mount");
      await decodeImages(article);
      const dom = snapshotMarkdownDom(article, { remoteImages: request.remoteImages });
      const title = titleFor(request);
      return {
        revision: request.snapshot.revision,
        title,
        htmlFragment: dom.fragment,
        htmlDocument: buildMarkdownStandaloneHtml({ fragment: dom.fragment, title }),
        plainText: markdownToPlainText(request.snapshot.logicalSource),
        warnings: [...trackerWarnings, ...dom.warnings],
      };
    } finally {
      root.unmount();
      host.remove();
    }
  });
}
