/** Live navigation callbacks the link service delegates to (held behind a ref by the viewer so
 *  the service object stays stable across renders). */
export interface PdfLinkActions {
  goToDest: (dest: string | unknown[] | null) => void;
  goToPage: (n: number) => void;
  currentPage: () => number;
  pagesCount: () => number;
  openExternal?: (url: string) => void | Promise<void>;
}

// PDF link URLs are untrusted document content: only http(s)/mailto ever reach the host.
const SAFE_EXTERNAL_URL = /^(?:https?:|mailto:)/i;

export function isSafeExternalUrl(url: string): boolean {
  return SAFE_EXTERNAL_URL.test(url);
}

/** Minimal IPDFLinkService for pdf.js's AnnotationLayer: external URLs, internal destinations, and
 *  page-navigation named actions. Attachments, OCG state and scripting are deliberate no-ops. */
export function createPdfLinkService(actions: PdfLinkActions) {
  return {
    eventBus: null,
    externalLinkEnabled: true,
    isInPresentationMode: false,
    getDestinationHash: (_dest: unknown): string => "#",
    getAnchorUrl: (_hash: string): string => "#",

    addLinkAttributes(link: HTMLAnchorElement, url: string, _newWindow?: boolean): void {
      // Never put the document-supplied URL in href: ctrl/middle-click would navigate the host
      // page itself. The URL lives only in the tooltip and the click handler.
      link.href = "#";
      link.title = url;
      link.rel = "noopener noreferrer";
      const open = (e: Event) => {
        e.preventDefault();
        if (isSafeExternalUrl(url)) void actions.openExternal?.(url);
      };
      link.addEventListener("click", open);
      link.addEventListener("auxclick", open);
    },

    goToDestination(dest: string | unknown[] | null): void {
      actions.goToDest(dest);
    },
    goToPage(n: number): void {
      actions.goToPage(n);
    },
    executeNamedAction(action: string): void {
      switch (action) {
        case "NextPage":
          actions.goToPage(actions.currentPage() + 1);
          break;
        case "PrevPage":
          actions.goToPage(actions.currentPage() - 1);
          break;
        case "FirstPage":
          actions.goToPage(1);
          break;
        case "LastPage":
          actions.goToPage(actions.pagesCount());
          break;
        default:
          break;
      }
    },
    executeSetOCGState(_action: unknown): void {},
    getAttachmentContent: async (_id: string): Promise<null> => null,
  };
}

export type PdfLinkService = ReturnType<typeof createPdfLinkService>;
