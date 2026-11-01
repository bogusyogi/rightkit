import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import SessionMinimap from "./SessionMinimap";
import type { SessionMinimapMessage } from "./SessionMinimap";
import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";

describe("SessionMinimap", () => {
  it("renders one clickable, labelled cell per message in the 13-column grid", () => {
    const messages: SessionMinimapMessage[] = [
      { kind: "user", text: "inspect the stream" },
      { kind: "assistant", tokens: 100, time: "14:02:13" },
      { kind: "tool", tool: "search", title: "find references", status: "ok" },
      { kind: "assistant", tokens: 1_000, streaming: true },
    ];
    const onClick = vi.fn();
    const onHover = vi.fn();
    const { getByTestId, getByRole } = render(
      <SessionMinimap messages={messages} hoveredIdx={1} onClick={onClick} onHover={onHover} />,
    );

    expect(getByTestId("session-minimap-grid").getAttribute("style")).toContain(
      "grid-template-columns: repeat(13, minmax(0, 1fr))",
    );
    expect(getByRole("button", { name: /you · inspect the stream/ })).toBeTruthy();
    expect(getByRole("button", { name: /assistant · 100 tok/ })).toHaveAttribute(
      "aria-current",
      "true",
    );
    expect(getByRole("button", { name: /search find references/ })).toBeTruthy();

    fireEvent.mouseEnter(getByTestId("session-minimap-cell-0"));
    fireEvent.click(getByTestId("session-minimap-cell-2"));
    fireEvent.mouseLeave(getByTestId("session-minimap-cell-0"));
    expect(onHover).toHaveBeenNthCalledWith(1, 0);
    expect(onClick).toHaveBeenCalledWith(2);
    expect(onHover).toHaveBeenLastCalledWith(null);
  });

  it("preserves the OMP log-scaled assistant heatmap and streaming pulse", () => {
    const { getByTestId } = render(
      <SessionMinimap
        messages={[
          { kind: "assistant", tokens: 10 },
          { kind: "assistant", tokens: 1_000, streaming: true },
        ]}
        hoveredIdx={null}
      />,
    );

    const cheap = getByTestId("session-minimap-cell-0") as HTMLButtonElement;
    const expensive = getByTestId("session-minimap-cell-1") as HTMLButtonElement;
    expect(Number(cheap.style.opacity)).toBeLessThan(Number(expensive.style.opacity));
    expect(expensive.className).toContain("animate-pulse");
  });

  it("accepts the App transcript wire shape without changing the donor projection", () => {
    const messages: TranscriptMessage[] = [
      {
        $kind: "user",
        id: "u1",
        createdAt: "2026-08-29T00:00:00Z",
        content: "hello",
        pinned: false,
      },
      {
        $kind: "tool",
        id: "t1",
        createdAt: "2026-08-29T00:00:01Z",
        toolCallId: "call-1",
        summary: "12 matches",
        isError: true,
        pinned: false,
      },
    ];
    const { getByRole } = render(<SessionMinimap messages={messages} hoveredIdx={null} />);

    expect(getByRole("button", { name: /you · hello/ })).toBeTruthy();
    expect(getByRole("button", { name: /call-1.*error/ })).toBeTruthy();
  });
});
