import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ModelsTab from "./ModelsTab";
import type { OrchestrationSettings } from "@rightkit/agent-ui-core/lib/types";
import type { SettingsDraft } from "../types";

function baseSettings(): OrchestrationSettings {
  return {
    workspaceRoot: "/workspace",
    defaultModel: "coder",
    primaryOrchestratorModelId: "coder",
    roleModelMode: "single",
    roleModelSplitAvailable: false,
    models: [],
    agents: [],
    budget: {
      perTaskMaxUsd: 1,
      perGoalMaxUsd: 2,
      perSessionMaxUsd: 3,
      perAgentCallMaxUsd: 0.5,
      globalMonthlyMaxUsd: 100,
      warningAt: 0.8,
      maxJuryCallsPerRun: 3,
      maxEscalationDepth: 2,
      maxRetriesPerRole: 2,
      maxWallClockMinutes: 60,
      maxIterations: 50,
      maxWorkers: 4,
    },
    benchmarks: { dailyHealthChecksEnabled: false, promotionMinQualityDelta: 0.05 },
    remote: { telegramTokenConfigured: false, telegramAllowedUsers: [], webEnabled: false },
    routing: { capabilityRouter: false, aggressiveness: "conservative" },
    keepDaemonResident: false,
  };
}

function baseDraft(routing: SettingsDraft["routing"]): SettingsDraft {
  return {
    primaryOrchestratorModelId: "coder",
    modelOverrides: {},
    roleModelMode: "single",
    roleModelSplitAvailable: false,
    budget: baseSettings().budget,
    benchmarks: baseSettings().benchmarks,
    remote: {
      telegramToken: "",
      clearTelegramToken: false,
      telegramAllowedUsersText: "",
      webEnabled: false,
      telegramTokenConfigured: false,
    },
    routing,
    keepDaemonResident: false,
  };
}

const noop = () => {};

describe("ModelsTab capability router toggle (R1a-L4)", () => {
  it("groups Direct catalog routes without inferring a provider", () => {
    const html = renderToStaticMarkup(
      <ModelsTab
        settings={baseSettings()}
        settingsDraft={baseDraft({ capabilityRouter: false, aggressiveness: "conservative" })}
        workspaceRoot="/workspace"
        localModelCatalog={null}
        localModelProbeBaseUrl=""
        localModelProbeResult={null}
        localModelChatSmokeResult={null}
        localProviderSuggestion={null}
        localModelActionPending={null}
        localModelError={null}
        catalogGroups={[
          {
            authority: "direct",
            connectionId: "direct-1",
            name: "http://127.0.0.1:11434",
            protocol: "openai_compatible",
            state: "ready",
            models: [
              {
                id: "local-model",
                object: null,
                created: null,
                ownedBy: null,
                root: null,
                parent: null,
                permission: null,
                capabilities: [],
                contextLength: null,
                maxInputTokens: null,
                maxOutputTokens: null,
                selectable: true,
                unavailableReason: null,
              },
            ],
          },
          {
            authority: "direct",
            connectionId: "default",
            name: "default",
            protocol: "openai_compatible",
            state: "ready",
            models: [
              {
                id: "auto/balanced",
                object: null,
                created: null,
                ownedBy: null,
                root: null,
                parent: null,
                permission: null,
                capabilities: [],
                contextLength: 128000,
                maxInputTokens: null,
                maxOutputTokens: null,
                selectable: true,
                unavailableReason: null,
              },
              {
                id: "cp/cline-pass/glm-5.2",
                object: null,
                created: null,
                ownedBy: "cline-pass",
                root: null,
                parent: null,
                permission: null,
                capabilities: [],
                contextLength: 128000,
                maxInputTokens: null,
                maxOutputTokens: null,
                selectable: true,
                unavailableReason: null,
              },
            ],
          },
        ]}
        onSettingsDraft={noop}
        onRefreshLocalModels={noop}
        onLocalModelProbeBaseUrl={noop}
        onProbeLocalModelEndpoint={noop}
        onSmokeLocalModelEndpoint={noop}
        onSuggestLocalProvider={noop}
        onOpenWorkspace={noop}
      />,
    );
    expect(html).toContain("Direct Connections");
    expect(html).not.toContain("OmniRoute");
    expect(html).not.toContain("auto/balanced");
    expect(html).toContain("cp/cline-pass/glm-5.2");
    expect(html).not.toContain("provider/pool");
  });
  it("renders the toggle unchecked and aggressiveness select disabled when routing is off", () => {
    const html = renderToStaticMarkup(
      <ModelsTab
        settings={baseSettings()}
        settingsDraft={baseDraft({ capabilityRouter: false, aggressiveness: "conservative" })}
        workspaceRoot="/workspace"
        localModelCatalog={null}
        localModelProbeBaseUrl=""
        localModelProbeResult={null}
        localModelChatSmokeResult={null}
        localProviderSuggestion={null}
        localModelActionPending={null}
        localModelError={null}
        onSettingsDraft={noop}
        onRefreshLocalModels={noop}
        onLocalModelProbeBaseUrl={noop}
        onProbeLocalModelEndpoint={noop}
        onSmokeLocalModelEndpoint={noop}
        onSuggestLocalProvider={noop}
        onOpenWorkspace={noop}
      />,
    );

    expect(html).toContain("Capability router");
    expect(html).toContain("Routes tasks by capability inside the selected role model policy");
    expect(html).not.toContain('checked=""');
    // The authority picker replaces the legacy primary-model <select>; the
    // aggressiveness select remains disabled while routing is
    // off. Match the `disabled=""` boolean-attribute form specifically —
    // the element also carries a `disabled:opacity-40` Tailwind class,
    // which would false-positive a plain substring check for "disabled".
    const selectTags = html.match(/<select[^>]*>/g) ?? [];
    expect(selectTags.length).toBeGreaterThanOrEqual(1);
    expect(selectTags.at(-1)).toMatch(/\sdisabled=""/);
  });

  it("renders the toggle checked and the selected aggressiveness preset when routing is on", () => {
    const html = renderToStaticMarkup(
      <ModelsTab
        settings={baseSettings()}
        settingsDraft={baseDraft({ capabilityRouter: true, aggressiveness: "aggressive" })}
        workspaceRoot="/workspace"
        localModelCatalog={null}
        localModelProbeBaseUrl=""
        localModelProbeResult={null}
        localModelChatSmokeResult={null}
        localProviderSuggestion={null}
        localModelActionPending={null}
        localModelError={null}
        onSettingsDraft={noop}
        onRefreshLocalModels={noop}
        onLocalModelProbeBaseUrl={noop}
        onProbeLocalModelEndpoint={noop}
        onSmokeLocalModelEndpoint={noop}
        onSuggestLocalProvider={noop}
        onOpenWorkspace={noop}
      />,
    );

    expect(html).toContain('checked=""');
    expect(html).toContain("Aggressive");
    // The aggressiveness <select> (last one rendered) must be enabled once
    // routing is on. Controlled <select value> itself isn't reflected as
    // an HTML "selected" attribute in static SSR output (React applies it
    // client-side via the DOM property), so this test checks enablement
    // (absence of the `disabled=""` boolean attribute), not the selected
    // option's markup. The element still carries the `disabled:opacity-40`
    // Tailwind class regardless of state, so match the attribute form.
    const selectTags = html.match(/<select[^>]*>/g) ?? [];
    expect(selectTags.length).toBeGreaterThanOrEqual(1);
    expect(selectTags.at(-1)).not.toMatch(/\sdisabled=""/);
  });
});

describe("ModelsTab role model entitlement", () => {
  it("explains that every role inherits the conductor and hides editable role selectors on Free", () => {
    const settings = baseSettings();
    settings.models = [
      {
        id: "conductor",
        displayName: "MiniMax M3",
        provider: "minimax",
        modelSlug: "minimax/minimax-m3",
        contextWindow: 204800,
        supportsVision: false,
        supportsTools: true,
        enabled: true,
      },
    ];
    settings.agents = [
      { id: "coder", name: "Coder", role: "implementation", modelId: "conductor", enabled: true },
    ];

    const html = renderToStaticMarkup(
      <ModelsTab
        settings={settings}
        settingsDraft={baseDraft({ capabilityRouter: false, aggressiveness: "conservative" })}
        workspaceRoot="/workspace"
        localModelCatalog={null}
        localModelProbeBaseUrl=""
        localModelProbeResult={null}
        localModelChatSmokeResult={null}
        localProviderSuggestion={null}
        localModelActionPending={null}
        localModelError={null}
        onSettingsDraft={noop}
        onRefreshLocalModels={noop}
        onLocalModelProbeBaseUrl={noop}
        onProbeLocalModelEndpoint={noop}
        onSmokeLocalModelEndpoint={noop}
        onSuggestLocalProvider={noop}
        onOpenWorkspace={noop}
      />,
    );

    expect(html).toContain("Every role uses the conductor model");
    expect(html).toContain("Pro");
    expect(html).not.toContain("Override the model any specialist role uses");
    const selectTags = html.match(/<select[^>]*>/g) ?? [];
    // Authority picker + capability-router aggressiveness. No per-role selector.
    expect(selectTags).toHaveLength(1);
  });

  it("shows per-role selectors only when Pro role splitting is available and selected", () => {
    const settings = baseSettings();
    settings.roleModelMode = "split";
    settings.roleModelSplitAvailable = true;
    settings.models = [
      {
        id: "conductor",
        displayName: "MiniMax M3",
        provider: "minimax",
        modelSlug: "minimax/minimax-m3",
        contextWindow: 204800,
        supportsVision: false,
        supportsTools: true,
        enabled: true,
      },
    ];
    settings.agents = [
      { id: "coder", name: "Coder", role: "implementation", modelId: "conductor", enabled: true },
    ];
    const draft = baseDraft({ capabilityRouter: false, aggressiveness: "conservative" });
    draft.roleModelMode = "split";
    draft.roleModelSplitAvailable = true;

    const html = renderToStaticMarkup(
      <ModelsTab
        settings={settings}
        settingsDraft={draft}
        workspaceRoot="/workspace"
        localModelCatalog={null}
        localModelProbeBaseUrl=""
        localModelProbeResult={null}
        localModelChatSmokeResult={null}
        localProviderSuggestion={null}
        localModelActionPending={null}
        localModelError={null}
        onSettingsDraft={noop}
        onRefreshLocalModels={noop}
        onLocalModelProbeBaseUrl={noop}
        onProbeLocalModelEndpoint={noop}
        onSmokeLocalModelEndpoint={noop}
        onSuggestLocalProvider={noop}
        onOpenWorkspace={noop}
      />,
    );

    expect(html).toContain("Per-role models");
    expect(html).toContain("Coder");
    const selectTags = html.match(/<select[^>]*>/g) ?? [];
    // Role-policy + router aggressiveness; authority pickers use buttons.
    expect(selectTags.length).toBeGreaterThanOrEqual(1);
  });
});

describe("ModelsTab capability badge tone (PA-06b)", () => {
  it("renders the 'tools' capability tag in a neutral tone, not success green", () => {
    const settings = baseSettings();
    settings.primaryOrchestratorModelId = "primary";
    settings.models = [
      {
        id: "primary",
        displayName: "Primary Model",
        provider: "minimax",
        modelSlug: "minimax/minimax-m3",
        contextWindow: 204800,
        supportsVision: false,
        supportsTools: false,
        enabled: true,
      },
      {
        id: "other",
        displayName: "Other Model",
        provider: "moonshotai",
        modelSlug: "moonshotai/kimi-k2.6",
        contextWindow: 204800,
        supportsVision: false,
        supportsTools: true,
        enabled: true,
      },
    ];
    const draft = baseDraft({ capabilityRouter: false, aggressiveness: "conservative" });
    draft.primaryOrchestratorModelId = "primary";

    const html = renderToStaticMarkup(
      <ModelsTab
        settings={settings}
        settingsDraft={draft}
        workspaceRoot="/workspace"
        localModelCatalog={null}
        localModelProbeBaseUrl=""
        localModelProbeResult={null}
        localModelChatSmokeResult={null}
        localProviderSuggestion={null}
        localModelActionPending={null}
        localModelError={null}
        onSettingsDraft={noop}
        onRefreshLocalModels={noop}
        onLocalModelProbeBaseUrl={noop}
        onProbeLocalModelEndpoint={noop}
        onSmokeLocalModelEndpoint={noop}
        onSuggestLocalProvider={noop}
        onOpenWorkspace={noop}
      />,
    );

    expect(html).toContain(">tools<");
    expect(html).toContain("current");
    // The capability tag ("tools") is decorative, not a success signal — it
    // must not share the green token with "current" (the true active-model
    // state).
    expect(html).toMatch(/text-text-3">tools</);
    expect(html).toMatch(/text-accent-2">current</);
  });
});
