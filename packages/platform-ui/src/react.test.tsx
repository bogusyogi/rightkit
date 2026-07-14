import { expect, it } from "vitest";
import type { ReactElement } from "react";
import { Kbd } from "./react.js";

it("renders semantic kbd elements from a canonical chord", () => {
  const rendered = Kbd({ chord: "mod+space", platform: "macos", gap: 6 }) as ReactElement<{
    "aria-label": string;
    style: Record<string, string>;
    children: Array<{ type: unknown }>;
  }>;
  expect(rendered.type).toBe("span");
  expect(rendered.props["aria-label"]).toBe("⌘ + Space");
  expect(rendered.props.style).toEqual({ "--rightkit-kbd-gap": "6px" });
  expect(rendered.props.children.map((child: { type: unknown }) => child.type)).toEqual([
    "kbd",
    "kbd",
  ]);
});
