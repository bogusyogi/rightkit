import { expect, it, vi } from "vitest";
import * as React from "react";
import type { ReactElement } from "react";
import { Kbd, usePlatform } from "./react.js";

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

it("lets apps override the keyboard chord accessibility label", () => {
  const rendered = Kbd({
    chord: "mod+space",
    platform: "windows",
    ariaLabel: "Start or stop recording",
  }) as ReactElement<{ "aria-label": string }>;

  expect(rendered.props["aria-label"]).toBe("Start or stop recording");
});

it("exports usePlatform as a real stable React hook", () => {
  const dispatcher = {
    useState: vi.fn((initializer: () => string) => [initializer(), vi.fn()]),
  };
  const internals = (
    React as unknown as {
      __CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE: { H: unknown };
    }
  ).__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE;
  const previous = internals.H;
  internals.H = dispatcher;
  globalThis.__RIGHTKIT_QA_PLATFORM__ = "windows";

  try {
    expect(usePlatform()).toBe("windows");
    expect(dispatcher.useState).toHaveBeenCalledOnce();
  } finally {
    internals.H = previous;
    delete globalThis.__RIGHTKIT_QA_PLATFORM__;
  }
});
