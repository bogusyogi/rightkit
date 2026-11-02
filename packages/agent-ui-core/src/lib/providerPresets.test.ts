import { describe, expect, it } from "vitest";

import {
  checkKeyFormat,
  HOSTED_PROVIDER_PRESETS,
  SETTINGS_PROVIDER_PRESETS,
} from "./providerPresets.js";

const openrouter = HOSTED_PROVIDER_PRESETS.find((preset) => preset.id === "openrouter");
if (!openrouter) throw new Error("openrouter preset missing from HOSTED_PROVIDER_PRESETS");

it("offers MiniMax as a persisted hosted provider", () => {
  const minimax = HOSTED_PROVIDER_PRESETS.find((preset) => preset.id === "minimax");

  expect(minimax).toMatchObject({
    label: "MiniMax",
    env: "MINIMAX_API_KEY",
    requiresApiKey: true,
  });
  expect(minimax?.models?.map((model) => model.id)).toContain("minimax/minimax-m3");
});

it("offers TinyFish as a Settings-only search provider", () => {
  const tinyfish = SETTINGS_PROVIDER_PRESETS.find((preset) => preset.id === "tinyfish");
  expect(tinyfish).toMatchObject({
    label: "TinyFish Search",
    env: "TINYFISH_API_KEY",
    requiresApiKey: true,
  });
  expect(HOSTED_PROVIDER_PRESETS.some((preset) => preset.id === "tinyfish")).toBe(false);
});

describe("checkKeyFormat", () => {
  it("treats an empty draft as empty, not invalid", () => {
    expect(checkKeyFormat(openrouter, "")).toEqual({ status: "empty", message: "" });
    expect(checkKeyFormat(openrouter, "   ")).toEqual({ status: "empty", message: "" });
  });

  it("flags a value missing the provider key prefix", () => {
    const result = checkKeyFormat(openrouter, "abcdefghijklmnopqrstuvwxyz");
    expect(result.status).toBe("invalid");
    expect(result.message).toContain("sk-or-");
  });

  it("flags a too-short value even with the right prefix", () => {
    const result = checkKeyFormat(openrouter, "sk-or-123");
    expect(result.status).toBe("invalid");
  });

  it("flags a value containing whitespace", () => {
    const result = checkKeyFormat(openrouter, "sk-or-v1-abc defghijklmnopqrstuvwxyz");
    expect(result.status).toBe("invalid");
    expect(result.message).toContain("spaces");
  });

  it("accepts a well-formed key", () => {
    const result = checkKeyFormat(openrouter, "sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789");
    expect(result).toEqual({ status: "valid", message: "Looks valid" });
  });
});
