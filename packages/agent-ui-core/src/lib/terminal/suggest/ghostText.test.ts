// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import { clearGhostOverlay, renderGhostText, type GhostTextEntry } from "./ghostText.js";

const entries: GhostTextEntry[] = [];
afterEach(() => {
  for (const entry of entries.splice(0)) clearGhostOverlay(entry);
  document.body.replaceChildren();
});

describe("terminal ghost text", () => {
  it("renders at the cursor seam and keeps the text inert", () => {
    const host = document.createElement("div");
    const ghostHost = document.createElement("div");
    ghostHost.dataset.terminalGhostHost = "";
    host.append(ghostHost);
    document.body.append(host);
    const entry: GhostTextEntry = {
      container: host,
      ghostText: null,
      ghostOverlay: null,
      attached: true,
      opened: true,
      getCursorPosition: () => ({ x: 24, y: 12 }),
    };
    entries.push(entry);

    renderGhostText(entry, " status");

    expect(entry.ghostText).toBe(" status");
    expect(entry.ghostOverlay?.textContent).toBe(" status");
    expect(entry.ghostOverlay?.getAttribute("aria-hidden")).toBe("true");
    expect(entry.ghostOverlay?.style.left).toBe("24px");
    expect(entry.ghostOverlay?.style.top).toBe("12px");
  });

  it("replaces and clears stale overlays", () => {
    const host = document.createElement("div");
    document.body.append(host);
    const entry: GhostTextEntry = { container: host, ghostText: null, ghostOverlay: null, attached: true, opened: true };
    entries.push(entry);

    renderGhostText(entry, " first");
    const first = entry.ghostOverlay;
    renderGhostText(entry, " second");
    expect(first?.isConnected).toBe(false);
    expect(entry.ghostOverlay?.textContent).toBe(" second");

    clearGhostOverlay(entry);
    expect(entry.ghostText).toBeNull();
    expect(entry.ghostOverlay).toBeNull();
    expect(host.children).toHaveLength(0);
  });

  it("does not render for a detached or empty terminal", () => {
    const host = document.createElement("div");
    const entry: GhostTextEntry = { container: host, ghostText: null, ghostOverlay: null, attached: false, opened: true };
    renderGhostText(entry, " ignored");
    expect(host.children).toHaveLength(0);
  });
});
