// Direct renderer port of Hermes IDE's terminal/ghostText.ts.
// The terminal runtime is daemon-owned in the host app, so the xterm-specific
// cursor adapter is an optional seam rather than a PTY dependency.

export interface GhostTextPosition {
  x: number;
  y: number;
}

export interface GhostTextEntry {
  container: HTMLElement;
  ghostText: string | null;
  ghostOverlay: HTMLElement | null;
  attached: boolean;
  opened: boolean;
  getCursorPosition?: () => GhostTextPosition | null;
}

export function renderGhostText(entry: GhostTextEntry, text: string): void {
  if (!entry.attached || !entry.opened || !text) return;

  clearGhostOverlay(entry);
  entry.ghostText = text;

  const overlay = document.createElement("span");
  overlay.className = "terminal-ghost-text";
  overlay.textContent = text;
  overlay.setAttribute("aria-hidden", "true");
  overlay.style.cssText = [
    "position: absolute",
    "color: var(--color-text-muted, var(--text-3))",
    "opacity: 0.55",
    "pointer-events: none",
    "white-space: pre",
    "z-index: 1",
  ].join(";");

  const position = entry.getCursorPosition?.() ?? null;
  if (position) {
    overlay.style.left = `${position.x}px`;
    overlay.style.top = `${position.y}px`;
  } else {
    overlay.style.left = "0";
    overlay.style.top = "0";
  }

  const host = entry.container.querySelector<HTMLElement>("[data-terminal-ghost-host]") ?? entry.container;
  if (getComputedStyle(host).position === "static") host.style.position = "relative";
  host.appendChild(overlay);
  entry.ghostOverlay = overlay;
}

export function clearGhostOverlay(entry: GhostTextEntry): void {
  entry.ghostText = null;
  entry.ghostOverlay?.remove();
  entry.ghostOverlay = null;
}
