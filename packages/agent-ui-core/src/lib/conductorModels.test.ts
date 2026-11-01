import { describe, expect, it } from "vitest";
import {
  CONDUCTOR_RUNTIME_MODELS,
  enabledConductorRuntimeModelLabel,
  conductorRuntimeModelLabel,
  conductorRuntimeModelsFromCatalog,
  conductorSupportsThinkingLevels,
  resolveConductorRuntimeModelId,
} from "./conductorModels";

describe("conductor runtime models", () => {
  it("offers MiniMax M3 as a first-class conductor", () => {
    expect(CONDUCTOR_RUNTIME_MODELS.map((model) => model.id)).toContain("minimax/minimax-m3");
    expect(resolveConductorRuntimeModelId("minimax-m3")).toBe("minimax/minimax-m3");
    expect(conductorRuntimeModelLabel("minimax/minimax-m3")).toBe("MiniMax M3");
    expect(conductorSupportsThinkingLevels("minimax/minimax-m3")).toBe(false);
    expect(
      CONDUCTOR_RUNTIME_MODELS.find((model) => model.id === "minimax/minimax-m3")
        ?.supportsThinkingBudgetTokens,
    ).toBe(false);
  });

  it("derives the conductor chooser from enabled roster entries only", () => {
    const common = {
      provider: "openrouter",
      contextWindow: 128_000,
      supportsVision: false,
      supportsTools: true,
    };
    expect(
      conductorRuntimeModelsFromCatalog([
        {
          ...common,
          id: "planner",
          displayName: "GLM 5.2",
          modelSlug: "z-ai/glm-5.2",
          enabled: true,
        },
        {
          ...common,
          id: "fake-marketing-model",
          displayName: "GPT 5.1",
          modelSlug: "openai/gpt-5.1",
          enabled: true,
        },
        {
          ...common,
          id: "worker",
          displayName: "MiniMax M3",
          modelSlug: "minimax/minimax-m3",
          enabled: false,
        },
      ]),
    ).toEqual([expect.objectContaining({ id: "z-ai/glm-5.2", label: "GLM 5.2" })]);
  });

  it("does not present a removed session model as available", () => {
    expect(
      enabledConductorRuntimeModelLabel("mm-sonnet", [CONDUCTOR_RUNTIME_MODELS[2]]),
    ).toBeNull();
    expect(enabledConductorRuntimeModelLabel("minimax-m3", [CONDUCTOR_RUNTIME_MODELS[2]])).toBe(
      "MiniMax M3",
    );
  });
});
