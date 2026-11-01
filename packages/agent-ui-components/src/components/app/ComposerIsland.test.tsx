// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import ComposerIsland, { type ConductorOption } from "./ComposerIsland";
import type { PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import { CLAUDE_MODELS } from "@rightkit/agent-ui-core/lib/claudeModels";
import type { CatalogModel } from "@rightkit/agent-ui-core/lib/types/inference";
import { useReferenceStore } from "@rightkit/agent-ui-core/state/referenceStore";

// Only the file-mention popover test needs a live workspace lookup — it
// force-enables the Tauri-host path and stubs the daemon call so the
// popover renders with a real, highlighted item to assert against.
vi.mock("@rightkit/agent-ui-core/lib/daemon", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@rightkit/agent-ui-core/lib/daemon")>();
  return {
    ...actual,
    isTauriHost: () => true,
    searchWorkspaceMentions: vi.fn(async () => [
      {
        kind: "file" as const,
        label: "App.tsx",
        detail: "src/App.tsx",
        insertText: "@src/App.tsx",
        path: "src/App.tsx",
        line: null,
      },
    ]),
  };
});

const ALL_CONDUCTORS_AVAILABLE: ConductorOption[] = [
  { kind: "codex", label: "Codex", detail: "Installed · 12 governed tools", available: true },
  { kind: "claude", label: "Claude", detail: "Installed · 9 governed tools", available: true },
];

vi.mock("../../../state/composerStore", () => {
  const setMode = vi.fn((mode: "read" | "edit" | "plan") => {
    const ls = globalThis.localStorage;
    if (ls && typeof mode === "string") {
      try {
        ls.setItem("cr.composerMode.v1", mode);
      } catch {
        // ignore
      }
    }
    (store as unknown as { mode: string }).mode = mode;
  });
  const setAgentMode = vi.fn((mode: string) => {
    (store as unknown as { agentMode: string }).agentMode = mode;
  });
  const setModel = vi.fn((model: string) => {
    (store as unknown as { model: string }).model = model;
  });
  const setFastMode = vi.fn((value: boolean) => {
    (store as unknown as { fastMode: boolean }).fastMode = value;
  });
  const setScope = vi.fn((scope: "local" | "cloud") => {
    (store as unknown as { scope: string }).scope = scope;
  });
  const setWorktree = vi.fn((worktree: string) => {
    (store as unknown as { worktree: string }).worktree = worktree;
  });
  const setEffort = vi.fn((effort: string) => {
    (store as unknown as { effort: string }).effort = effort;
  });
  const store = {
    composer: "",
    promptHistory: [],
    promptHistoryIndex: null,
    promptHistoryDraft: "",
    attachments: [],
    mode: "edit" as "read" | "edit" | "plan",
    agentMode: "accept-edits" as string,
    model: "opus-4.8" as string,
    fastMode: false,
    scope: "local" as "local" | "cloud",
    worktree: "" as string,
    effort: "deeper" as string,
    setComposer: vi.fn(),
    setAttachments: vi.fn(),
    addAttachments: vi.fn(),
    removeAttachment: vi.fn(),
    clear: vi.fn(),
    rememberPrompt: vi.fn(),
    changeComposer: vi.fn((value: string) => {
      store.composer = value;
    }),
    navigateHistory: vi.fn(),
    setMode,
    setAgentMode,
    setModel,
    setFastMode,
    setScope,
    setWorktree,
    setEffort,
  };
  return { useComposerStore: () => store };
});

const baseProps = {
  composerRef: undefined as never,
  commands: [],
  skills: [],
  disabled: false,
  isWorking: false,
  queueLength: 0,
  activeGoal: null,
  showGoalComposer: false,
  workspaceRoot: "",
  workspaces: [],
  primaryModelLabel: null,
  conductorOptions: ALL_CONDUCTORS_AVAILABLE,
  activeConductor: "native" as const,
  onConductorChange: () => {},
  modelOptions: [],
  supportsThinkingLevels: false,
  supportsThinkingBudgetTokens: false,
  gitBranch: null,
  gitBranches: [],
  branchSwitchPending: null,
  costUsd: 0,
  contextPercent: 0,
  usagePromptTokens: 0,
  usageCompletionTokens: 0,
  usageContextTokens: 0,
  cacheReadTokens: 0,
  usageCacheCreationTokens: 0,
  permissionMode: "ask" as const,
  historyActive: false,
  showWorkspaceMenu: false,
  showBranchMenu: false,
  onSubmit: () => {},
  onInterrupt: () => {},
  permissionModes: ["ask", "accept-edits", "auto-safe", "plan", "bypass"] as PermissionMode[],
  onSelectPermissionMode: () => {},
  onPasteImages: () => {},
  onSetWorkspace: () => {},
  onToggleWorkspaceMenu: () => {},
  onToggleBranchMenu: () => {},
  onDismissMenus: () => {},
  onSwitchBranch: () => {},
  onConductorModelChange: () => {},
  onConductorModelSelect: () => {},
  onToggleGoalComposer: () => {},
  onCreateGoal: (_text: string) => {},
  onPauseGoal: () => {},
  onResumeGoal: () => {},
  onEditGoal: (_text: string) => {},
  onClearGoal: () => {},
  onOpenModelSettings: () => {},
  onOpenProviderSettings: () => {},
  onOpenWorkspace: () => {},
};

function catalogModel(
  id: string,
  ownedBy: string | null = null,
  capabilities: string[] = [],
): CatalogModel {
  return {
    id,
    ownedBy,
    capabilities,
    object: null,
    created: null,
    root: null,
    parent: null,
    permission: null,
    contextLength: null,
    maxInputTokens: null,
    maxOutputTokens: null,
    selectable: true,
    unavailableReason: null,
  };
}

function clearLs() {
  const ls = globalThis.localStorage as Storage | undefined;
  if (ls) ls.clear();
}

describe("ComposerIsland", () => {
  beforeEach(() => {
    clearLs();
    useReferenceStore.setState({ references: [], restoreError: null });
  });
  afterEach(() => {
    cleanup();
    useReferenceStore.setState({ references: [], restoreError: null });
    clearLs();
  });

  it("renders the textarea with aria-label='Message composer'", () => {
    render(<ComposerIsland {...baseProps} />);
    const textarea = screen.getByLabelText("Message composer");
    expect(textarea).toBeTruthy();
    expect(textarea.tagName).toBe("TEXTAREA");
  });

  it("disables model switching while inference or child work is active", () => {
    render(
      <ComposerIsland
        {...baseProps}
        selectionSwitchDisabled
        selectionSwitchUnavailableReason="Stop active inference and child agents before changing model"
      />,
    );

    const picker = screen.getByTestId("composer-model-picker") as HTMLButtonElement;
    expect(picker.disabled).toBe(true);
    expect(picker.title).toBe("Stop active inference and child agents before changing model");
  });

  it("renders a single mode chip showing Accept edits by default and toggles its expanded state", () => {
    render(<ComposerIsland {...baseProps} />);
    const chip = screen.getByTestId("composer-mode-chip");
    expect(chip).toBeTruthy();
    expect(chip.textContent ?? "").toContain("Accept edits");
    expect(chip.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(chip);
    expect(chip.getAttribute("aria-expanded")).toBe("true");
  });

  it("plain Enter submits, and mod+Enter still submits too", () => {
    const onSubmit = vi.fn();
    render(<ComposerIsland {...baseProps} onSubmit={onSubmit} />);
    const textarea = screen.getByLabelText("Message composer") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "hello" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(textarea, { key: "Enter", ctrlKey: true });
    expect(onSubmit).toHaveBeenCalledTimes(2);
  });

  it("Shift+Enter does not submit and leaves the newline to the textarea's default behavior", () => {
    const onSubmit = vi.fn();
    render(<ComposerIsland {...baseProps} onSubmit={onSubmit} />);
    const textarea = screen.getByLabelText("Message composer") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "hello" } });
    // fireEvent.* returns false when a handler called preventDefault() — true
    // here proves the newline was left to the browser's own Enter behavior.
    const notPrevented = fireEvent.keyDown(textarea, { key: "Enter", shiftKey: true });
    expect(notPrevented).toBe(true);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("does not submit on Enter while an IME composition is active", () => {
    const onSubmit = vi.fn();
    render(<ComposerIsland {...baseProps} onSubmit={onSubmit} />);
    const textarea = screen.getByLabelText("Message composer") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "こんにちは" } });
    const notPrevented = fireEvent.keyDown(textarea, { key: "Enter", isComposing: true });
    expect(notPrevented).toBe(true);
    expect(onSubmit).not.toHaveBeenCalled();
    // The legacy keyCode 229 signal is honored the same way.
    const notPreventedLegacy = fireEvent.keyDown(textarea, { key: "Enter", keyCode: 229 });
    expect(notPreventedLegacy).toBe(true);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("renders an Attach button and a Send button in the footer", () => {
    render(<ComposerIsland {...baseProps} />);
    expect(screen.getByRole("button", { name: "Attach files" })).toBeTruthy();
    expect(screen.getByLabelText("Send")).toBeTruthy();
  });

  it("enables submit when the composer contains only a typed reference", () => {
    const onSubmit = vi.fn();
    useReferenceStore.getState().addReferences([
      {
        id: "repository:stablyai/orca",
        kind: "repository",
        destination: "https://github.com/stablyai/orca",
        filename: "stablyai/orca",
        provenance: {
          source: "composer.test",
          createdAt: "2026-07-21T00:00:00Z",
          trust: "user",
        },
        actions: ["remove"],
      },
    ]);
    render(<ComposerIsland {...baseProps} onSubmit={onSubmit} />);

    const send = screen.getByLabelText("Send") as HTMLButtonElement;
    expect(send.disabled).toBe(false);
    fireEvent.click(send);
    expect(onSubmit).toHaveBeenCalledOnce();
  });

  it("keeps Goal out of the footer until /goal selects Goal mode", () => {
    const onToggleGoalComposer = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        commands={[{ name: "goal", usage: "/goal", description: "Start a goal" }]}
        onToggleGoalComposer={onToggleGoalComposer}
      />,
    );
    expect(screen.queryByTestId("composer-goal-chip")).toBeNull();
    fireEvent.change(screen.getByLabelText("Message composer"), { target: { value: "/goal" } });
    fireEvent.click(screen.getByTestId("composer-completion-command-0"));
    expect(onToggleGoalComposer).toHaveBeenCalledTimes(1);
  });

  it("shows a dismissible Goal mode chip only while Goal mode is active", () => {
    render(<ComposerIsland {...baseProps} showGoalComposer />);
    const chip = screen.getByTestId("composer-goal-chip");
    expect(chip.getAttribute("aria-pressed")).toBe("true");
    expect(chip.textContent).toContain("Goal mode");
  });

  it("submits the main composer as a goal with plain Enter in Goal mode", () => {
    const onCreateGoal = vi.fn();
    render(<ComposerIsland {...baseProps} showGoalComposer onCreateGoal={onCreateGoal} />);
    const textarea = screen.getByLabelText("Message composer");
    fireEvent.change(textarea, { target: { value: "Finish the composer UX" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onCreateGoal).toHaveBeenCalledWith("Finish the composer UX");
  });

  it("does not submit on Enter when the mention popover has a highlighted item, and accepts it instead", async () => {
    const onSubmit = vi.fn();
    render(<ComposerIsland {...baseProps} workspaceRoot="/repo/coderight" onSubmit={onSubmit} />);
    const textarea = screen.getByLabelText("Message composer") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "@file:App" } });
    const item = await screen.findByRole("option", { name: /App\.tsx/ });
    expect(item.getAttribute("aria-selected")).toBe("true");

    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSubmit).not.toHaveBeenCalled();
    expect(textarea.value).toContain("@src/App.tsx");
  });

  it("anchors the slash command list above the entire composer shell", () => {
    render(
      <ComposerIsland
        {...baseProps}
        commands={[{ name: "goal", usage: "/goal", description: "Start a goal" }]}
      />,
    );
    fireEvent.change(screen.getByLabelText("Message composer"), { target: { value: "/" } });
    const popover = screen.getByTestId("composer-completion-menu");
    expect(popover.className).toContain("bottom-full");
    expect(popover.parentElement?.tagName).toBe("FOOTER");
  });

  it("summarizes usage with tokens instead of implying the estimated cost was billed", () => {
    render(
      <ComposerIsland
        {...baseProps}
        costUsd={0.0147}
        usagePromptTokens={1200}
        usageCompletionTokens={300}
      />,
    );
    const summary = screen.getByText("Usage").closest("summary");
    expect(summary?.textContent).toContain("1.5k tokens");
    expect(summary?.textContent).not.toContain("$");
  });

  it("hides thinking levels when the selected conductor does not support them", () => {
    render(<ComposerIsland {...baseProps} />);
    expect(screen.getByTestId("composer-model-chip")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Choose thinking level" })).toBeNull();
    expect(screen.getByTestId("composer-context-chip")).toBeTruthy();
  });

  it("shows thinking levels only for a conductor with explicit level support", () => {
    render(<ComposerIsland {...baseProps} supportsThinkingLevels />);
    expect(screen.getByRole("button", { name: "Choose thinking level" })).toBeTruthy();
  });

  it("renders only the configured conductor models", () => {
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel="MiniMax M3"
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
          {
            id: "moonshotai/kimi-k2.6",
            label: "Kimi K2.6",
            aliases: ["kimi-k2.6"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Choose conductor model" }));
    expect(screen.getByRole("menuitem", { name: "MiniMax M3" })).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: "Kimi K2.6" })).toBeTruthy();
    expect(screen.queryByText("More models…")).toBeNull();
  });

  it("never substitutes a configured option for a missing current model", () => {
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel="mm-sonnet"
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    expect(screen.getByRole("button", { name: "Select model" })).toBeTruthy();
    expect(screen.queryByText("mm-sonnet")).toBeNull();
    // "Select model" is the neutral no-selection-yet prompt, not a problem
    // state — it must not carry the amber unavailable tone.
    expect(screen.getByRole("button", { name: "Select model" }).className).not.toMatch(
      /text-warning/,
    );
  });

  it("names a stale session model as unavailable", () => {
    render(
      <ComposerIsland
        {...baseProps}
        modelUnavailable
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    // Must render as an explicit amber state, not the same passive/muted
    // treatment as "Select model" — otherwise it reads as agreeing with any
    // nearby healthy/connected indicator instead of flagging a real problem.
    expect(screen.getByRole("button", { name: "Model unavailable" }).className).toMatch(
      /text-warning/,
    );
  });

  it("offers a Providers recovery action next to an unavailable model, not a dead end", () => {
    const onOpenProviderSettings = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        modelUnavailable
        onOpenProviderSettings={onOpenProviderSettings}
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open Providers settings" }));
    expect(onOpenProviderSettings).toHaveBeenCalledOnce();
  });

  it("does not show the Providers recovery action once a model resolves", () => {
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel="mm-sonnet"
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3", "mm-sonnet"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    expect(screen.queryByRole("button", { name: "Open Providers settings" })).toBeNull();
  });

  it("keeps the picker enabled and shows driverModelLabel when a conductor is active", () => {
    render(
      <ComposerIsland
        {...baseProps}
        activeConductor="codex"
        driverModelLabel="Codex via App"
      />,
    );
    const picker = screen.getByTestId("composer-model-picker");
    expect(picker).not.toBeDisabled();
    expect(picker.textContent).toContain("Codex via App");
  });

  it("lists the external conductors in the picker menu and marks the active one", () => {
    render(
      <ComposerIsland
        {...baseProps}
        activeConductor="codex"
        driverModelLabel="Codex via App"
      />,
    );
    fireEvent.click(screen.getByTestId("composer-model-picker"));
    const group = within(screen.getByRole("group", { name: "Conductor" }));
    // Native belongs to the model group, never the conductor group.
    expect(group.queryByText("Native App")).toBeNull();
    expect(group.getByText("Codex")).toBeTruthy();
    expect(group.getByText("Claude")).toBeTruthy();
    const activeItem = group.getByRole("menuitem", { name: /^Codex/ });
    expect(activeItem.textContent).toContain("✓");
  });

  it("selecting a conductor calls onConductorChange with that kind and closes the menu", () => {
    const onConductorChange = vi.fn();
    render(<ComposerIsland {...baseProps} onConductorChange={onConductorChange} />);
    const picker = screen.getByTestId("composer-model-picker");
    fireEvent.click(picker);
    fireEvent.click(screen.getByRole("menuitem", { name: /^Codex/ }));
    expect(onConductorChange).toHaveBeenCalledWith("codex");
    expect(picker.getAttribute("aria-expanded")).toBe("false");
  });

  it("moves focus into the model menu, roves with arrows, and restores its trigger on Escape", () => {
    render(<ComposerIsland {...baseProps} />);
    const picker = screen.getByTestId("composer-model-picker");
    picker.focus();

    fireEvent.keyDown(picker, { key: "ArrowDown" });
    const codex = screen.getByRole("menuitem", { name: /^Codex/ });
    expect(document.activeElement).toBe(codex);
    expect(codex.tabIndex).toBe(-1);

    fireEvent.keyDown(document.activeElement ?? document.body, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: /^Claude/ }));

    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Home" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: /^Codex/ }));

    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "Conductor models" })).toBeNull();
    expect(document.activeElement).toBe(picker);
  });

  it("presents concrete Direct models without router aliases", () => {
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel="GLM 5.2"
        catalogGroups={[
          {
            authority: "direct",
            connectionId: "local",
            name: "Local LLM",
            protocol: "openai_compatible",
            state: "ready",
            models: [catalogModel("local-coder")],
          },
          {
            authority: "direct",
            connectionId: "default",
            name: "Direct",
            protocol: "openai_compatible",
            state: "ready",
            models: [
              catalogModel("auto/best-vision", "combo", ["vision"]),
              catalogModel("glm-5.2"),
            ],
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByTestId("composer-model-picker"));
    expect(screen.getByRole("group", { name: "Direct · Local LLM" })).toBeTruthy();
    expect(screen.getByRole("group", { name: "Direct · Direct" })).toBeTruthy();
    expect(screen.queryByText("Best for vision")).toBeNull();
    expect(screen.queryByText("auto/best-vision")).toBeNull();
    expect(screen.getByText("glm-5.2")).toBeTruthy();
    expect(within(screen.getByRole("menuitemradio", { name: /glm-5\.2/ })).getByText("Direct · Direct")).toBeTruthy();
    expect(screen.queryByRole("group", { name: "Conductor" })).toBeNull();
    expect(screen.queryByRole("group", { name: "Native App" })).toBeNull();
    expect(screen.queryByText("Inference authorities")).toBeNull();
  });

  it("does not let an unavailable conductor be selected and shows why in the DOM", () => {
    const onConductorChange = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        onConductorChange={onConductorChange}
        conductorOptions={[
          {
            kind: "codex",
            label: "Codex",
            detail: "CLI not installed",
            available: false,
            unavailableReason: "Codex CLI not installed",
          },
          {
            kind: "claude",
            label: "Claude",
            detail: "Installed · 9 governed tools",
            available: true,
          },
        ]}
      />,
    );
    fireEvent.click(screen.getByTestId("composer-model-picker"));
    expect(screen.getByText("Codex CLI not installed")).toBeTruthy();
    const codexItem = screen.getByRole("menuitem", { name: /^Codex/ });
    expect(codexItem).toBeDisabled();
    fireEvent.click(codexItem);
    expect(onConductorChange).not.toHaveBeenCalled();
  });

  // Native is not a menu row: the model list IS Native the host app. Listing a
  // separate "Native the host app" entry above the same models made picking a
  // model a two-step choice, so the models stay visible under every conductor
  // and selecting one hands the session back to native in a single click.
  it("always lists the native models and switches back to native when one is picked", () => {
    const onConductorChange = vi.fn();
    const onConductorModelChange = vi.fn();
    const modelOptions = [
      {
        id: "minimax/minimax-m3",
        label: "MiniMax M3",
        aliases: ["minimax-m3"],
        supportsThinkingLevels: false,
        supportsThinkingBudgetTokens: false,
      },
    ];
    render(
      <ComposerIsland
        {...baseProps}
        activeConductor="codex"
        driverModelLabel="Codex via App"
        modelOptions={modelOptions}
        onConductorChange={onConductorChange}
        onConductorModelChange={onConductorModelChange}
      />,
    );
    fireEvent.click(screen.getByTestId("composer-model-picker"));

    // No "Native the host app" conductor row — the group heading carries the name.
    expect(screen.getByRole("group", { name: "Native App" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: /^Native App/ })).toBeNull();
    expect(screen.getByText(/brings its own model/i)).toBeTruthy();

    fireEvent.click(screen.getByRole("menuitem", { name: /MiniMax M3/ }));
    expect(onConductorChange).toHaveBeenCalledWith("native");
    expect(onConductorModelChange).toHaveBeenCalledWith("minimax/minimax-m3");
  });

  it("shows a recoverable unavailable state when no models are enabled", () => {
    const onOpenModelSettings = vi.fn();
    render(<ComposerIsland {...baseProps} onOpenModelSettings={onOpenModelSettings} />);
    const picker = screen.getByRole("button", { name: "Models unavailable" });
    // Same "nothing works right now" problem as "Model unavailable" — must
    // carry the same amber tone, not the neutral picker treatment.
    expect(picker.className).toMatch(/text-warning/);
    fireEvent.click(picker);
    fireEvent.click(screen.getByRole("menuitem", { name: "Configure models" }));
    expect(onOpenModelSettings).toHaveBeenCalledOnce();
  });

  it("closes open composer menus when clicking outside", () => {
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel="MiniMax M3"
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    const picker = screen.getByRole("button", { name: "Choose conductor model" });
    fireEvent.click(picker);
    expect(picker.getAttribute("aria-expanded")).toBe("true");
    fireEvent.mouseDown(document.body);
    expect(picker.getAttribute("aria-expanded")).toBe("false");
  });

  it("dismisses a repository menu when clicking elsewhere inside the composer", () => {
    const onDismissMenus = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        workspaceRoot="/repo/coderight"
        showWorkspaceMenu
        onDismissMenus={onDismissMenus}
      />,
    );
    fireEvent.mouseDown(screen.getByLabelText("Message composer"));
    expect(onDismissMenus).toHaveBeenCalled();
  });

  it("shows truthful repository controls without fake worktree or PR affordances", () => {
    render(<ComposerIsland {...baseProps} workspaceRoot="/repo/coderight" />);
    expect(screen.getByText("Local")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Choose repository" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Choose Git branch" })).toBeTruthy();
    expect(screen.queryByText("Worktree")).toBeNull();
    expect(screen.queryByRole("button", { name: "Create pull request" })).toBeNull();
  });

  it("selects a real daemon permission mode from the permission menu", () => {
    const onSelectPermissionMode = vi.fn();
    render(<ComposerIsland {...baseProps} onSelectPermissionMode={onSelectPermissionMode} />);
    fireEvent.click(screen.getByRole("button", { name: "Permission mode: Ask first" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Plan only/ }));
    expect(onSelectPermissionMode).toHaveBeenCalledWith("plan");
  });

  it("does not render a permanent Skills chip", () => {
    render(
      <ComposerIsland
        {...baseProps}
        skills={[
          {
            slug: "audit",
            name: "Audit",
            source: "local",
            description: "Audit code",
            trigger: "@audit",
            enabled: true,
            active: true,
            lifecycle: "active",
            category: "tool",
            priority: 0,
            tags: [],
            pathAffinity: [],
            path: "/skills/audit.md",
            directory: "/skills",
            kind: "tool",
            valid: true,
          },
        ]}
      />,
    );
    expect(screen.queryByRole("button", { name: "Open skills" })).toBeNull();
  });

  it("shows skills contextually from an @ mention", () => {
    render(
      <ComposerIsland
        {...baseProps}
        skills={[
          {
            slug: "audit",
            name: "Audit",
            source: "local",
            description: "Audit code",
            trigger: "@audit",
            enabled: true,
            active: true,
            lifecycle: "active",
            category: "tool",
            priority: 0,
            tags: [],
            pathAffinity: [],
            path: "/skills/audit.md",
            directory: "/skills",
            kind: "tool",
            valid: true,
          },
        ]}
      />,
    );
    fireEvent.change(screen.getByLabelText("Message composer"), {
      target: { value: "@skill:aud" },
    });
    expect(screen.getByRole("listbox", { name: "Skill mentions" })).toBeTruthy();
    fireEvent.click(screen.getByRole("option", { name: /Audit/ }));
    expect(screen.queryByRole("listbox", { name: "Skill mentions" })).toBeNull();
    const reference = useReferenceStore.getState().references[0];
    expect(reference).toMatchObject({
      id: "skill:audit",
      kind: "skill",
      destination: "@audit",
      filename: "Audit",
    });
    expect(screen.getByLabelText("Reference Audit")).toBeTruthy();
  });

  it("keeps send and the message input present, enabled, and full-size in a narrow container", () => {
    const { container } = render(
      <div style={{ width: 320 }}>
        <ComposerIsland {...baseProps} workspaceRoot="/repo/coderight" queueLength={3} />
      </div>,
    );
    const textarea = screen.getByLabelText("Message composer") as HTMLTextAreaElement;
    expect(textarea).toBeTruthy();
    expect(textarea.disabled).toBe(false);

    const send = screen.getByLabelText("Send") as HTMLButtonElement;
    expect(send).toBeTruthy();
    expect(send.className).toContain("h-8");
    expect(send.className).toContain("w-8");

    const footer = container.querySelector('[data-testid="composer-footer"]');
    expect(footer?.className).toContain("flex-wrap");
  });

  it("keeps every footer control mounted with a 60-character model name", () => {
    const longLabel = "A".repeat(60);
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel={longLabel}
        modelOptions={[
          {
            id: "long/model-id",
            label: longLabel,
            aliases: [],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
        queueLength={2}
      />,
    );
    expect(screen.getByLabelText("Message composer")).toBeTruthy();
    expect(screen.getByLabelText("Send")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Attach files" })).toBeTruthy();
    expect(screen.getByTestId("composer-model-picker")).toBeTruthy();
    expect(screen.getByText("2 queued")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Permission mode: Ask first" })).toBeTruthy();
  });

  it("exposes the full model name via title when the label is truncated", () => {
    const longLabel = "B".repeat(60);
    render(
      <ComposerIsland
        {...baseProps}
        primaryModelLabel={longLabel}
        modelOptions={[
          {
            id: "long/model-id",
            label: longLabel,
            aliases: [],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    const picker = screen.getByTestId("composer-model-picker");
    expect(picker.getAttribute("title")).toBe(longLabel);
  });

  it("renders every footer control alongside attachments and a queued prompt", () => {
    useReferenceStore.getState().addReferences([
      {
        id: "attachment-1",
        kind: "image",
        destination: "screenshot.png",
        filename: "screenshot.png",
        mime: "image/png",
        provenance: {
          source: "composer.attachment",
          createdAt: "2026-08-02T00:00:00Z",
          trust: "user",
        },
        actions: ["remove"],
      },
    ]);
    render(
      <ComposerIsland
        {...baseProps}
        workspaceRoot="/repo/coderight"
        queueLength={1}
        primaryModelLabel="MiniMax M3"
        modelOptions={[
          {
            id: "minimax/minimax-m3",
            label: "MiniMax M3",
            aliases: ["minimax-m3"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
          },
        ]}
      />,
    );
    expect(screen.getByLabelText("Send")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Attach files" })).toBeTruthy();
    expect(screen.getByTestId("composer-model-picker")).toBeTruthy();
    expect(screen.getByText("1 queued")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Choose repository" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Choose Git branch" })).toBeTruthy();
  });

  // GLM and Kimi were listed as if connected while their provider had no saved
  // key at all, so picking one failed at send time rather than at the click.
  it("does not let a model whose provider has no key be picked, and says why", () => {
    const onConductorModelChange = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        activeConductor="native"
        onConductorModelChange={onConductorModelChange}
        modelOptions={[
          {
            id: "z-ai/glm-5.2",
            label: "GLM 5.2",
            aliases: ["glm-5.2"],
            supportsThinkingLevels: false,
            supportsThinkingBudgetTokens: false,
            provider: "OpenRouter",
            unavailableReason: "OpenRouter has no saved key",
          },
        ]}
      />,
    );
    fireEvent.click(screen.getByTestId("composer-model-picker"));
    const item = screen.getByRole("menuitem", { name: /GLM 5.2/ });
    expect(item).toBeDisabled();
    expect(screen.getByText("OpenRouter has no saved key")).toBeTruthy();
    fireEvent.click(item);
    expect(onConductorModelChange).not.toHaveBeenCalled();
  });

  it("a conductor with models expands into a nested submenu and selecting one picks both the conductor and the model", () => {
    const onConductorChange = vi.fn();
    const onConductorModelSelect = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        activeConductor="claude"
        onConductorChange={onConductorChange}
        onConductorModelSelect={onConductorModelSelect}
        conductorOptions={[
          {
            kind: "codex",
            label: "Codex",
            detail: "Installed · 12 governed tools",
            available: true,
          },
          {
            kind: "claude",
            label: "Claude",
            detail: "Installed · 9 governed tools",
            available: true,
            models: [
              { id: "claude-sonnet-5", label: "Claude Sonnet 5" },
              { id: "claude-opus-5", label: "Claude Opus 5", isDefault: true },
            ],
            selectedModelId: "claude-opus-5",
          },
        ]}
      />,
    );
    fireEvent.click(screen.getByTestId("composer-model-picker"));
    const claudeTrigger = screen.getByRole("menuitem", { name: /^Claude/ });
    expect(claudeTrigger.getAttribute("aria-haspopup")).toBe("true");
    expect(claudeTrigger.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("menuitem", { name: /Claude Sonnet 5/ })).toBeNull();

    fireEvent.click(claudeTrigger);
    expect(claudeTrigger.getAttribute("aria-expanded")).toBe("true");
    expect(onConductorChange).not.toHaveBeenCalled();

    // The currently bound model carries the ✓ treatment.
    const opusItem = screen.getByRole("menuitem", { name: /Claude Opus 5/ });
    expect(opusItem.textContent).toContain("✓");

    fireEvent.click(screen.getByRole("menuitem", { name: "Claude Sonnet 5" }));
    expect(onConductorChange).toHaveBeenCalledWith("claude");
    expect(onConductorModelSelect).toHaveBeenCalledWith("claude", "claude-sonnet-5");
    expect(screen.queryByTestId("composer-model-picker")?.getAttribute("aria-expanded")).toBe(
      "false",
    );
  });

  it("a conductor with no models does not expand and stays a single selectable row", () => {
    const onConductorChange = vi.fn();
    render(<ComposerIsland {...baseProps} onConductorChange={onConductorChange} />);
    fireEvent.click(screen.getByTestId("composer-model-picker"));
    const codexTrigger = screen.getByRole("menuitem", { name: /^Codex/ });
    expect(codexTrigger.getAttribute("aria-haspopup")).toBeNull();
    fireEvent.click(codexTrigger);
    expect(onConductorChange).toHaveBeenCalledWith("codex");
  });

  it("an unavailable conductor does not expand even when it carries a model list", () => {
    const onConductorChange = vi.fn();
    render(
      <ComposerIsland
        {...baseProps}
        onConductorChange={onConductorChange}
        conductorOptions={[
          {
            kind: "codex",
            label: "Codex",
            detail: "Not installed",
            available: false,
            unavailableReason: "Codex CLI not installed",
            models: [{ id: "codex-mini", label: "Codex Mini" }],
          },
          {
            kind: "claude",
            label: "Claude",
            detail: "Installed · 9 governed tools",
            available: true,
          },
        ]}
      />,
    );
    fireEvent.click(screen.getByTestId("composer-model-picker"));
    const codexTrigger = screen.getByRole("menuitem", { name: /^Codex/ });
    expect(codexTrigger.getAttribute("aria-haspopup")).toBeNull();
    fireEvent.click(codexTrigger);
    expect(codexTrigger.getAttribute("aria-expanded")).toBeNull();
    expect(screen.queryByRole("menuitem", { name: /Codex Mini/ })).toBeNull();
    expect(onConductorChange).not.toHaveBeenCalled();
  });

  // Verified 2026-08-03 against
  // https://platform.claude.com/docs/en/about-claude/models/overview — the
  // fetched-live source, not training memory.
  it("Claude's static model list has the current default and excludes the deprecated/invitation-only models", () => {
    expect(CLAUDE_MODELS.some((m) => m.id === "claude-opus-5")).toBe(true);
    expect(CLAUDE_MODELS.some((m) => m.id === "claude-opus-4-1-20250805")).toBe(false);
  });
});
