// @vitest-environment jsdom
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { ImageViewer } from "./ImageViewer.js";

beforeAll(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    },
  );
});
afterEach(cleanup);

// jsdom never decodes images, so a changed displaySrc stays mid-swap.
function mount(extra: Record<string, unknown> = {}) {
  const registerSave = vi.fn();
  const adapters = { printImage: vi.fn(async () => true) };
  const props = { path: "/a.png", displayName: "a.png", src: "/orig.png", registerSave, adapters, ...extra };
  const view = render(<ImageViewer {...props} displaySrc="/a.png" />);
  registerSave.mockClear();
  view.rerender(<ImageViewer {...props} displaySrc="/b.png" />);
  return { registerSave, adapters };
}

describe("save during a sibling decode swap", () => {
  it("stays registered against the export source by default", () => {
    const { registerSave } = mount();
    const last = registerSave.mock.calls.at(-1)?.[0];
    expect(typeof last).toBe("function");
  });

  it("blockExportDuringSwap unregisters save mid-swap", () => {
    const { registerSave } = mount({ blockExportDuringSwap: true });
    expect(registerSave.mock.calls.at(-1)?.[0]).toBeNull();
  });
});
