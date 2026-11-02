import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";
import { VirtualSessionList } from "./VirtualSessionList.js";
import type { VirtualSessionListRow } from "./VirtualSessionList.js";

afterEach(cleanup);

const rows: VirtualSessionListRow[] = [
  { key: "today", kind: "divider", label: "Today" },
  { key: "older", kind: "divider", label: "Older" },
];

const noop = () => {};

function session(id: string, overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    title: `Session ${id}`,
    model: "openai/gpt-5",
    workspaceRoot: "D:/work/coderight",
    createdAt: "2026-06-29T09:00:00Z",
    updatedAt: "2026-06-29T09:20:00Z",
    ...overrides,
  };
}

describe("VirtualSessionList", () => {
  it("positions measured rows independently within a total-size spacer", () => {
    const { getByTestId } = render(
      <VirtualSessionList
        activeSessionId={null}
        onDeleteSession={noop}
        onSelectSession={noop}
        pinned={false}
        rows={rows}
        sortable={false}
      />,
    );

    const firstItem = getByTestId("divider-Today");
    const secondItem = getByTestId("divider-Older");
    const spacer = firstItem.parentElement;

    expect(firstItem.dataset.index).toBe("0");
    expect(firstItem.style.position).toBe("absolute");
    expect(firstItem.style.transform).toBe("translateY(0px)");
    expect(secondItem.dataset.index).toBe("1");
    expect(secondItem.style.transform).toBe("translateY(28px)");
    expect(spacer?.className).toBe("relative");
    expect(spacer?.style.height).toBe("56px");
    expect(spacer?.style.paddingTop).toBe("");
    expect(spacer?.style.paddingBottom).toBe("");
  });

  it("lets wheel overscroll chain to the outer sidebar scroller (#84964)", () => {
    const { getByTestId } = render(
      <VirtualSessionList
        activeSessionId={null}
        onDeleteSession={noop}
        onSelectSession={noop}
        pinned={false}
        rows={rows}
        sortable={false}
      />,
    );

    const scroller = getByTestId("divider-Today").parentElement?.parentElement;

    // This list is nested in the sidebar's own scroll container. Containing
    // overscroll here swallows wheel events at the inner scroll boundary.
    expect(scroller?.className).toContain("overflow-y-auto");
    expect(scroller?.className).not.toContain("overscroll-contain");
  });

  it("renders only the measured viewport plus an overscan window", () => {
    const manyRows: VirtualSessionListRow[] = Array.from({ length: 200 }, (_, index) => ({
      kind: "session" as const,
      session: session(`s-${index}`),
    }));
    const { getByLabelText, queryByLabelText } = render(
      <VirtualSessionList
        activeSessionId={null}
        onDeleteSession={noop}
        onSelectSession={noop}
        rows={manyRows}
      />,
    );

    expect(getByLabelText("Session s-0")).toBeTruthy();
    expect(queryByLabelText("Session s-199")).toBeNull();
  });

  it("keeps selection and action callbacks on the session row seam", () => {
    const onDeleteSession = vi.fn();
    const onSelectSession = vi.fn();
    const { getByRole, getByLabelText } = render(
      <VirtualSessionList
        activeSessionId="s-1"
        onDeleteSession={onDeleteSession}
        onSelectSession={onSelectSession}
        onTogglePin={noop}
        rows={[{ kind: "session", session: session("s-1", { tags: ["pinned"] }) }]}
      />,
    );

    fireEvent.click(getByRole("button", { name: "Session s-1" }));
    fireEvent.click(getByLabelText("Delete session Session s-1"));

    expect(onSelectSession).toHaveBeenCalledWith("s-1");
    expect(onDeleteSession).toHaveBeenCalledWith("s-1");
    expect(getByRole("button", { name: "Session s-1" })).toHaveAttribute("aria-current", "page");
  });
});
