// Focus after a page switch: land keyboard users on the destination's title-bar heading once the page has
// mounted, or restore focus to a launcher that is still usable. Imports nothing.

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

export interface FocusPageOptions {
  /** Attribute on the page root that names the page. Default `data-page`. */
  pageAttribute?: string;
  /** Selector of the title-bar heading to focus. Default `.rk-top h1` (the shell's title bar). */
  headingSelector?: string;
  /** Poll window while the destination mounts (it may sit in a lazy boundary). Default 2500. */
  timeoutMs?: number;
}

/**
 * Focuses the destination heading once the page (`[data-page="<expectedTab>"]`, or any `[data-page]`) is
 * mounted; polls each frame until `timeoutMs`. Without `expectedTab` it falls back to the page's first control.
 */
export function focusPageDestination(expectedTab?: string, options: FocusPageOptions = {}): void {
  const { pageAttribute = "data-page", headingSelector = ".rk-top h1", timeoutMs = 2500 } = options;
  const deadline = performance.now() + timeoutMs;
  const attempt = () => {
    const page = expectedTab
      ? document.querySelector<HTMLElement>(`[${pageAttribute}="${CSS.escape(expectedTab)}"]`)
      : document.querySelector<HTMLElement>(`[${pageAttribute}]`);
    // The page title lives in the title bar, so wait for the page body and focus that heading.
    const heading = page ? document.querySelector<HTMLElement>(headingSelector) : null;
    const control = expectedTab ? null : page?.querySelector<HTMLElement>(FOCUSABLE) ?? null;
    const target = heading ?? control;
    if (target) {
      if (target === heading) target.tabIndex = -1;
      target.focus({ preventScroll: true });
      return;
    }
    if (performance.now() < deadline) window.requestAnimationFrame(attempt);
  };
  window.requestAnimationFrame(attempt);
}

/**
 * Native dialogs restore focus only while their opener remains usable. A removed launcher falls back to the
 * current page's destination.
 */
export function focusConnectedOrPageDestination(opener: HTMLElement | null, options: FocusPageOptions = {}): void {
  window.requestAnimationFrame(() => {
    if (opener?.isConnected && opener.matches(FOCUSABLE)) {
      opener.focus({ preventScroll: true });
      return;
    }
    focusPageDestination(undefined, options);
  });
}
