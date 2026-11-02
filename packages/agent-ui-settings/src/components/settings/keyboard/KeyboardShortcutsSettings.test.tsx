// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import KeyboardShortcutsSettings, {
  SHORTCUT_OVERRIDES_STORAGE_KEY,
  readShortcutOverrides,
} from "./KeyboardShortcutsSettings.js";

beforeEach(() => localStorage.clear());

describe("KeyboardShortcutsSettings", () => {
  it("renders the grouped customizable shortcut inventory", () => {
    render(<KeyboardShortcutsSettings initialOverrides={{ new_chat: "alt+n" }} />);
    expect(screen.getByRole("heading", { name: "Keyboard shortcuts" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Session" })).toBeTruthy();
    expect(screen.getByText("New Chat")).toBeTruthy();
    expect(screen.getByText("Alt + N")).toBeTruthy();
  });

  it("persists reset-all and ignores malformed stored values", () => {
    render(<KeyboardShortcutsSettings initialOverrides={{ new_chat: "alt+n" }} />);
    fireEvent.click(screen.getByRole("button", { name: "Reset all" }));
    expect([null, "{}"]).toContain(localStorage.getItem(SHORTCUT_OVERRIDES_STORAGE_KEY));
    localStorage.setItem(SHORTCUT_OVERRIDES_STORAGE_KEY, "not json");
    expect(readShortcutOverrides({ open_settings: "mod+comma" })).toEqual({ open_settings: "mod+comma" });
  });
});
