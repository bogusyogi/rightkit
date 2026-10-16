import { describe, expect, it } from "vitest";
import { resolveShellKeybindings } from "./react/provider.js";

describe("resolveShellKeybindings", () => {
  it("defaults to the built-in chords", () => {
    expect(resolveShellKeybindings({})).toEqual({ palette: "mod+k", help: "?", sidebar: "mod+\\" });
  });

  it("lets keybindings override or disable each chord", () => {
    expect(resolveShellKeybindings({ keybindings: { palette: "mod+shift+k", help: false, sidebar: "mod+b" } })).toEqual({
      palette: "mod+shift+k",
      help: false,
      sidebar: "mod+b",
    });
  });

  it("keeps the legacy palette and help props when keybindings is absent", () => {
    expect(resolveShellKeybindings({ paletteChord: false, helpChord: "mod+/" })).toEqual({ palette: false, help: "mod+/", sidebar: "mod+\\" });
  });

  it("prefers keybindings over the legacy props", () => {
    expect(resolveShellKeybindings({ keybindings: { palette: "mod+p" }, paletteChord: "mod+k" }).palette).toBe("mod+p");
  });
});
