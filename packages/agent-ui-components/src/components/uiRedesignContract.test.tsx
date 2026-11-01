// @vitest-environment node
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import backgroundTasksPanelSource from "./BackgroundTasksPanel.tsx?raw";
import appBodySource from "./app/AppBody.tsx?raw";
import AppShellHeader from "@rightkit/agent-ui-core/components/app/AppShellHeader";
import appShellHeaderSource from "@rightkit/agent-ui-core/components/app/AppShellHeader?raw";
import commandPaletteSource from "@rightkit/agent-ui-core/components/CommandPalette?raw";
import composerBarSource from "./app/ComposerIsland.tsx?raw";
import composerChipsSource from "./app/composerIsland/Chips.tsx?raw";
import composerActivityStackSource from "./app/composerIsland/ComposerActivityStack.tsx?raw";
import composerMentionPopoverSource from "./app/composerIsland/MentionPopover.tsx?raw";
import composerContextUsageSource from "./app/composerIsland/ContextUsageMeterInline.tsx?raw";
import composerFormatSource from "./app/composerIsland/format.ts?raw";

// Concatenated source for parity tests — the spec moved some chip / popover
// / meter code into composerIsland/* helpers, so lookups span the cluster.
const composerIslandSource = [
  composerBarSource,
  composerChipsSource,
  composerActivityStackSource,
  composerMentionPopoverSource,
  composerContextUsageSource,
  composerFormatSource,
].join("\n");
import appSurfacesSource from "../hooks/app/useAppSurfaces.ts?raw";
import OnboardingPanel from "./app/OnboardingPanel";
import onboardingPanelSource from "./app/OnboardingPanel.tsx?raw";
import modelsTabSource from "./settings/tabs/ModelsTab.tsx?raw";
import settingsModalSource from "./settings/SettingsModal.tsx?raw";
import sidebarSource from "./Sidebar.tsx?raw";
import skillsTabSource from "./settings/tabs/SkillsTab.tsx?raw";
import skillListPanelSource from "./settings/tabs/skills/SkillListPanel.tsx?raw";
import titleBarSource from "./TitleBar.tsx?raw";
import { resolve } from "node:path";

// ?raw returns "" for CSS under vitest, so read the RightKit token map directly.
const rightKitTokensSource = readFileSync(
  resolve(__dirname, "@rightkit/agent-ui-core/styles/rightkit-shell-tokens.css"),
  "utf8",
);
import shortcutSchemaSource from "@rightkit/agent-ui-core/lib/shortcuts/config?raw";
import sessionEmptyStateSource from "@rightkit/agent-ui-workbench/components/workbench/SessionEmptyState?raw";
import workbenchContentSource from "./WorkbenchContent.tsx?raw";
// APP-CMP-001: the composer's textarea and its auto-size projection now live
// behind RichComposerEditor rather than inline in the ComposerIsland shell.
import richComposerEditorSource from "./composer/RichComposerEditor.tsx?raw";
import composerEditorProjectionSource from "@rightkit/agent-ui-core/lib/composer/editorProjection?raw";

const coreShellSources = {
  "components/TitleBar.tsx": titleBarSource,
  "components/Sidebar.tsx": sidebarSource,
  "components/BackgroundTasksPanel.tsx": backgroundTasksPanelSource,
  "components/app/AppShellHeader.tsx": appShellHeaderSource,
  "components/app/OnboardingPanel.tsx": onboardingPanelSource,
  "components/settings/SettingsModal.tsx": settingsModalSource,
};
const indexCss = [
  readFileSync(new URL("@rightkit/agent-ui-core/index.css", import.meta.url), "utf8"),
  readFileSync(new URL("@rightkit/agent-ui-core/styles/dark-theme.generated.css", import.meta.url), "utf8"),
].join("\n");

describe("UI redesign contract", () => {
  it("keeps the transcript scrollbar on the full-width reading surface", () => {
    expect(workbenchContentSource).toContain("cr-transcript-scroll");
    // The column follows the shared reading measure now. Pinning the old literal
    // 720px is what made four near-identical widths drift apart in the first
    // place — assert the token, not a number.
    expect(workbenchContentSource).toContain("var(--cr-measure)");
    expect(workbenchContentSource).not.toContain(
      "max-w-[760px] flex-1 flex-col gap-2 overflow-y-auto",
    );
  });

  it("uses a humanist sans body, display titles, and mono only for code surfaces", () => {
    expect(indexCss).toContain(
      '--font-body: "General Sans", ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;',
    );
    expect(indexCss).toContain(
      '--font-display: "Tanker", ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;',
    );
    expect(indexCss).toContain(
      '--font-mono: "JetBrains Mono", ui-monospace, "SFMono-Regular", "SF Mono", Menlo, Consolas, monospace;',
    );
    expect(indexCss).toContain("font-family: var(--font-body);");
    expect(indexCss).toMatch(
      /code,\s*pre,\s*kbd,\s*samp\s*\{[^}]*font-family:\s*var\(--font-mono\)/s,
    );
    expect(indexCss).not.toMatch(/--font-body:\s*"JetBrains Mono"/);
    expect(indexCss).not.toContain("font-family: theme(fontFamily.body)");
  });

  it("defines the spec type scale as reusable tokens", () => {
    for (const token of [
      "--text-display:",
      "--text-heading:",
      "--text-body:",
      "--text-secondary:",
      "--text-caption:",
      "--leading-body:",
    ]) {
      expect(indexCss).toContain(token);
    }
  });

  it("does not use mono for prose labels in core shell components", () => {
    for (const [relativePath, source] of Object.entries(coreShellSources)) {
      expect(source, relativePath).not.toContain('fontFamily: "var(--font-display)"');
      // BackgroundTasksPanel legitimately uses font-mono on tabular counters
      // (token counts, tool counts, elapsed seconds) for right-aligned tabular
      // numbers — that's not a prose label. The other shell components must
      // stay mono-free.
      if (relativePath === "components/BackgroundTasksPanel.tsx") continue;
      expect(source, relativePath).not.toContain("font-mono");
    }
  });

  it("keeps the black shell title chrome minimal and accessible", () => {
    expect(titleBarSource).toContain('aria-label="the host app window controls"');
    expect(titleBarSource).toContain("data-tauri-drag-region");
    // v5 chrome: 36 px high, traffic-light area on the left, compact controls.
    // Settings is intentionally labeled instead of being an unexplained gear.
    expect(titleBarSource).toContain("<Titlebar");
    expect(rightKitTokensSource).toContain("--rk-titlebar-height: 36px");
    expect(titleBarSource).toContain("inline-flex h-6");
    // Settings moved to the sidebar footer cluster; workspace utilities moved
    // to their floating rail, leaving only window controls on this edge.
    expect(titleBarSource).not.toContain('text="Settings"');
    expect(titleBarSource).not.toContain('text="Tools"');
    // Caption labels are passed to RightKit CaptionControls as captionLabels (rendered as aria-label).
    expect(titleBarSource).toContain('minimize: "Minimize window"');
    expect(titleBarSource).toContain('maximize: "Maximize window"');
    expect(titleBarSource).toContain('close: "Close window"');
    // CaptionControls derives the tooltip (title) from the same label as aria-label.
    expect(titleBarSource).not.toContain('title="Minimize"');
    expect(titleBarSource).not.toContain("bg-accent text-on-accent");
  });

  it("renders the sidebar toggle inside the titlebar (not as a floating button)", () => {
    // v5: IconButton takes `label=` (aria-label) and keeps the tooltip title.
    expect(titleBarSource).toContain('label="Toggle sessions sidebar"');
    expect(titleBarSource).toContain('title="Toggle sidebar"');
    expect(titleBarSource).not.toContain('label="Toggle workspace tools"');
    // AppBody does not duplicate a floating sidebar collapse button.
    expect(appBodySource).not.toContain('aria-label="Collapse sessions sidebar"');
  });

  it("renders the retry affordance only when the engine is disconnected", () => {
    const disconnectedHtml = renderToStaticMarkup(
      <AppShellHeader
        headerTitle="Demo"
        headerSubtitle="test"
        status="Disconnected"
        health={null}
        selected={null}
        messagesLength={0}
        isWorking={false}
        onCompact={() => {}}
        onRetryEngine={() => {}}
      />,
    );
    expect(disconnectedHtml).toContain("Retry engine");
    expect(disconnectedHtml).not.toContain("Reload preview");

    const connectedHtml = renderToStaticMarkup(
      <AppShellHeader
        headerTitle="Demo"
        headerSubtitle="test"
        status="Connected"
        health={{
          ok: true,
          service: "coderight",
          port: 9100,
          databasePath: "/tmp/db",
          utcNow: new Date().toISOString(),
        }}
        selected={null}
        messagesLength={0}
        isWorking={false}
        onCompact={() => {}}
        onRetryEngine={() => {}}
      />,
    );
    // v5: connected state shows an engine-status pill with the live label.
    expect(connectedHtml).toContain("Connected");
    expect(connectedHtml).not.toContain("Retry engine");
    expect(connectedHtml).not.toContain('aria-label="Retry engine"');
  });

  it("does not render a retry button in the sidebar footer", () => {
    expect(sidebarSource).not.toContain("Retry engine");
    expect(sidebarSource).not.toContain("Reload preview");
    expect(sidebarSource).not.toContain("onClick={onRefresh}");
  });

  it("does not duplicate the preview-mode status when it is already in the subtitle", () => {
    // v5: AppShellHeader no longer renders the subtitle text at all, so the
    // "Preview mode" string lives only in App.tsx's caller-supplied
    // subtitle (headerSubtitle). The header strip should not duplicate it.
    expect(appShellHeaderSource).not.toContain("Preview mode");
  });

  it("keeps deliberate accent touches on primary and active controls", () => {
    // Coding-agent shell: primary actions start immediately; no mode tabs.
    expect(sidebarSource).not.toContain('role="tab"');
    expect(sidebarSource).toContain("Recents");
    expect(sidebarSource).toContain("New session");
    expect(sidebarSource).not.toContain('label="Artifacts"');
    expect(sidebarSource).not.toContain("Customize");
    expect(indexCss).toContain("--color-accent: #e53855;");
    expect(indexCss).toContain("--color-on-accent: #0b0b0a;");
    // Running indicators are calm static green dots — brand bans pulsing
    // attention indicators (brand.md Motion).
    expect(appShellHeaderSource).toContain("bg-accent-2");
    expect(indexCss).not.toContain("cr-status-pulse");
  });

  it("keeps the sidebar compact, current-session aware, and keyboard inspectable", () => {
    // Compact primary nav, uppercase "Recents"
    // header, and a recent-sessions list where the active row uses
    // aria-current="page".
    expect(indexCss).toContain(".cr-sidebar-row");
    expect(sidebarSource).toContain("aria-current={isActive");
    expect(sidebarSource).not.toContain('role="tab"');
    expect(sidebarSource).toContain("Recents");
    expect(sidebarSource).not.toContain('username ?? "Adrian"');
    expect(sidebarSource).not.toContain(">Pro<");
    expect(sidebarSource).not.toContain("Session setup");
  });

  it("keeps chat utilities in the real header and the sidebar resize target invisible", () => {
    expect(appBodySource).toContain("headerActions=");
    expect(appBodySource).not.toContain('className="absolute right-3 top-2 z-30"');
    expect(appBodySource).toMatch(/aria-label="Resize sidebar"[\s\S]*?bg-transparent/);
    expect(appBodySource).not.toContain("bg-line/40");
  });

  it("keeps the command palette accessible and quick-switcher polished", () => {
    // The combobox / listbox / option roles, active-descendant, Home/End/Page keys and focus
    // trap come from RightKit's CommandPalette; the host app keeps its labels and chip types.
    expect(commandPaletteSource).toContain('CommandPalette as ShellCommandPalette');
    expect(commandPaletteSource).toContain('inputAriaLabel: "Search commands, sessions, and views"');
    expect(commandPaletteSource).toContain('title: "Command palette"');
    expect(commandPaletteSource).toContain("activeIndex={activeVisible}");
    expect(commandPaletteSource).toContain('type: "Session"');
    expect(commandPaletteSource).toContain('type: "View"');
  });

  it("keeps composer micro-interactions purposeful and reduced-motion safe", () => {
    // CSS-level contract: popovers, swaps, and reduced-motion handling
    // must remain in index.css (any widget can rely on these primitives).
    expect(indexCss).toContain(".cr-control-pill");
    expect(indexCss).toContain(".cr-popover-row");
    expect(indexCss).toContain(".cr-detail-swap");
    expect(indexCss).toContain("@keyframes cr-detail-swap");
    expect(indexCss).toContain("@media (hover: hover) and (pointer: fine)");
    expect(indexCss).toContain("@media (prefers-reduced-motion: reduce)");
    // Phase 4 composer rewrite: auto-size textarea + Read/Edit/Plan mode
    // pills. The detailed component contract lives in
    // ComposerIsland.test.tsx; this contract test only asserts the
    // shell-level markup that any popover/menu uses.
    //
    // APP-CMP-001 moved the raw <textarea> out of ComposerIsland and behind
    // RichComposerEditor, so the auto-size contract is asserted where it now
    // lives: the editor still renders a real textarea, and the projection
    // still starts every measurement from a single row before growing.
    expect(richComposerEditorSource).toMatch(/<textarea/);
    expect(richComposerEditorSource).toMatch(/rows=\{projection\.rows\}/);
    expect(composerEditorProjectionSource).toMatch(/input\.rows\s*=\s*1/);
    expect(composerIslandSource).toMatch(/event\.key\s*===\s*"Enter"/);
    // Enter sends, Shift+Enter inserts a newline. This used to assert the
    // literal `event.metaKey || event.ctrlKey`, which pinned the old
    // Cmd/Ctrl-only rule; mod+Enter still sends, it just shares the plain-Enter
    // branch now. Assert the guard that actually matters instead: an Enter
    // arriving mid-IME-composition must never submit, or CJK input is unusable.
    expect(composerIslandSource).toMatch(/!event\.shiftKey/);
    expect(composerIslandSource).toMatch(/isComposing|keyCode\s*===\s*229/);
  });

  it("gives workspace cards stronger titles and more generous copy leading", () => {
    const html = renderToStaticMarkup(
      <OnboardingPanel
        status="Preview mode"
        workspaceRoot=""
        runTarget="local"
        showAdvancedConnect={false}
        urlDraft=""
        tokenDraft=""
        onOpenWorkspace={() => {}}
        onRunTarget={() => {}}
        onRetryEngine={() => {}}
        onToggleAdvanced={() => {}}
        onUrlDraft={() => {}}
        onTokenDraft={() => {}}
        onConnect={() => {}}
      />,
    );

    expect(html).toContain("text-[15px] font-semibold");
    expect(html).toContain("leading-[1.55]");
    expect(html).toContain("Recommended");
    // The accent is a signature, not decoration: the ONLY accent on a card is
    // the selection marker. The static "Recommended" label and the icon chip
    // must stay neutral — otherwise the default-selected card carries three.
    expect(html).not.toContain("text-accent");
    expect(html).toContain("border-line-2 bg-panel-2 text-text");
    expect(html).toContain("[box-shadow:inset_3px_0_0_var(--color-accent),");
    expect(html).toContain("Reload preview");
    expect(html).not.toContain("border-accent/45");
    expect(html).not.toContain(">Connecting...<");
    expect(html).not.toContain("Retry local engine");
  });

  it("keeps agent work visible, stoppable, and transcript-openable", () => {
    const headerHtml = renderToStaticMarkup(
      <AppShellHeader
        headerTitle="Demo session"
        headerSubtitle="coderight"
        status="Preview mode"
        health={null}
        selected={{ id: "s1" }}
        messagesLength={1}
        isWorking={false}
        onCompact={() => {}}
        hasAgents
        agentCount={3}
        runningAgentCount={1}
        agentsOpen={false}
      />,
    );

    expect(headerHtml).toContain("Agents");
    // Agents live ONLY in the side panel (Adrian 2026-07-16 — no strip above
    // the composer); the header pill shows the ACTIVE count when agents run.
    expect(headerHtml).toContain(">1</span>");
    expect(appBodySource).toContain("runningAgentCount");
    expect(appBodySource).not.toContain("AgentProgress");
    // "Stop all" lives in the rendered agents panel (Adrian 2026-07-16);
    // the dead AgentRail + ConductorJobCard were deleted after the
    // affordance was moved here.
    expect(backgroundTasksPanelSource).toContain("Stop all");
    expect(backgroundTasksPanelSource).toContain("onCancelAll");
    expect(backgroundTasksPanelSource).toContain("running.length > 1");
    expect(backgroundTasksPanelSource).toContain("View transcript");
    expect(backgroundTasksPanelSource).toContain("fmtElapsed");
    expect(backgroundTasksPanelSource).toContain("No agents running.");
    expect(appSurfacesSource).toContain('qaScenario === "agents"');
    expect(appSurfacesSource).toContain('qaScenario === "new-chat"');
    expect(appSurfacesSource).toContain("withQaAgentScenario");
    expect(appSurfacesSource).toContain("Visual auditor");
    expect(appSurfacesSource).toContain("full transcript links are present");
  });

  it("allows the visible background-tasks close control to close while agents run", () => {
    expect(appBodySource).not.toContain('(hasAgentRail && rightPanelTab === "background-tasks")');
    expect(appBodySource).not.toContain(
      'hasAgentRail && runningAgentCount > 0 && rightPanelTab === "background-tasks"',
    );
  });

  it("keeps the QA agent fixture's selected model available in its model menu", () => {
    const agentScenario = appSurfacesSource.slice(
      appSurfacesSource.indexOf("function withQaAgentScenario"),
    );
    expect(agentScenario).toMatch(/primaryModelLabel:\s*"MiniMax M3"[\s\S]*modelOptions:/);
    expect(agentScenario).toContain('label: "MiniMax M3"');
    expect(agentScenario).toContain("supportsThinkingLevels: false");
    expect(agentScenario).toContain("activeGoal: qaGoal");
  });

  it("keeps goal state visible and status-aware", () => {
    expect(composerIslandSource).toContain("showGoalComposer");
    expect(composerIslandSource).toContain("ComposerActivityStack");
    expect(composerIslandSource).toContain("onCreateGoal(text)");
    expect(composerIslandSource).toContain("Goal mode");
    expect(composerActivityStackSource).toContain("Active goal");
    expect(composerActivityStackSource).toContain("Edit active goal");
    expect(composerActivityStackSource).toContain("completionPercent");
    expect(composerActivityStackSource).toContain("Verification:");
    expect(composerActivityStackSource).toContain("ProgressBar");
    expect(workbenchContentSource).not.toContain("GoalCard");
  });

  it("keeps skills selectable and inspectable from the composer", () => {
    // Phase 4 moved the composer-internal skill mention rendering out of
    // ComposerIsland; Phase 8 ships a dedicated SkillsChip component
    // (apps/coderight-tauri/src/components/SkillsChip.tsx) with its own
    // test scaffold. The composer's chip + popover contract is asserted
    // there. This test now only covers the Settings > Skills surfaces.
    expect(skillsTabSource).toContain("hasInstalledSkills");
    expect(skillsTabSource).toContain("Composer skill library");
    expect(skillsTabSource).toContain("No skills installed");
    expect(skillsTabSource).toContain("Installed skills");
    expect(skillsTabSource).toContain("Composer mention preview");
    expect(skillsTabSource).toContain("@security-check");
    expect(skillsTabSource).toContain("showInstallForm");
    expect(skillsTabSource).toContain("Skill creation");
    expect(skillListPanelSource).toContain("Search installed skills");
    expect(skillListPanelSource).toContain("Use New skill to add a local markdown skill.");
    expect(skillListPanelSource).toContain("[box-shadow:inset_2px_0_0_var(--color-accent)]");
  });

  it("teaches empty Settings states instead of showing generic missing-data panels", () => {
    expect(modelsTabSource).toContain("Model settings attach to a workspace");
    expect(modelsTabSource).toContain("Open local folder");
    expect(modelsTabSource).toContain("Workspace required");
    expect(modelsTabSource).toContain("Choose workspace");
    expect(modelsTabSource).toContain("Load roles");
    expect(modelsTabSource).toContain("current");
    expect(modelsTabSource).toContain("LocalModelCookbookEmpty");
    expect(modelsTabSource).toContain("[grid-template-columns:repeat(auto-fit,minmax(190px,1fr))]");
    expect(modelsTabSource).toContain("Inspect local model fit");
    expect(modelsTabSource).toContain("probe before save");
    expect(modelsTabSource).not.toContain("chat smoke before save");
    expect(modelsTabSource).not.toContain("Local model cookbook not loaded.");
    expect(skillsTabSource).toContain("SkillEmptyStep");
    expect(skillsTabSource).toContain("Use the New skill action above");
    expect(skillsTabSource).toContain("grid gap-2 sm:grid-cols-2");
    expect(skillsTabSource).toContain("rounded-lg border border-line bg-panel px-3 py-2");
    expect(skillsTabSource).toContain("Review before use");
    expect(skillsTabSource).toContain("Insert from composer");
    expect(skillsTabSource).toContain("Trigger and lifecycle stay visible.");
    expect(skillsTabSource).toContain("Source path is inspectable before use.");
  });

  it("opens settings as a modal dialog overlay, not a pane", () => {
    // Modal host is RightKit ModalShell (role=dialog, aria-modal, Escape, backdrop)
    // Backdrop closes, panel stops propagation (matches the CommandPalette pattern)
    // Esc keydown handler is registered while open
    expect(settingsModalSource).toContain("Discard unsaved settings changes?");
    // Footer carries Save/Discard/Reload
    expect(settingsModalSource).toContain("Save\n              </Button>");
    expect(settingsModalSource).toContain("Discard\n              </Button>");
    expect(settingsModalSource).toContain("Reload settings");
    // Compact sidebar + scrollable content, unclipped sticky header and footer.
    expect(settingsModalSource).toContain("h-[min(680px,88vh)]");
    expect(settingsModalSource).toContain("min-h-14");
    expect(settingsModalSource).toContain("h-12");
    // All 19 tabs reachable via 6 grouped nav sections
    expect(settingsModalSource).toContain("ModalShell");
    expect(settingsModalSource).toContain("SETTINGS_NAV_GROUPS");
    expect(settingsModalSource).toContain("SETTINGS_NAV_ITEMS");
    // AppFrame wires the modal with open/onClose; TitleBar exposes the open-settings hook
    expect(appBodySource).not.toContain("<SettingsPane");
    expect(appBodySource).not.toContain('pane === "settings"');
    expect(titleBarSource).toContain("onOpenSettings");
    expect(titleBarSource).toContain("settingsOpen");
  });

  it("command palette exposes an Open Settings action, not a Go to Settings pane", () => {
    expect(commandPaletteSource).toContain("onOpenSettings");
    expect(commandPaletteSource).toContain("Open Settings");
    expect(commandPaletteSource).not.toContain('onPane("settings")');
  });

  it("keeps runtime and terminal reachable while tasks stay in the titlebar", () => {
    expect(commandPaletteSource).toContain("Go to Runtime");
    expect(commandPaletteSource).toContain("Open Terminal Dock");
    expect(commandPaletteSource).not.toContain("Go to Tasks");
    expect(appShellHeaderSource).toContain("TaskIndicator");
  });

  it("renders a clickable status dot in the header with expand-to-details", () => {
    const html = renderToStaticMarkup(
      <AppShellHeader
        headerTitle="Demo"
        headerSubtitle="test"
        status="Connected"
        health={{
          ok: true,
          service: "coderight",
          port: 9100,
          databasePath: "/tmp/db",
          utcNow: new Date().toISOString(),
        }}
        selected={{ id: "s1" }}
        messagesLength={1}
        isWorking={false}
        onCompact={() => {}}
      />,
    );
    // v5: status pill carries the live label, dot color, and aria-expanded.
    expect(html).toContain("bg-accent-2");
    expect(html).toContain("Connected");
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('aria-label="Engine status: Connected"');
  });

  it("renders status dot with danger color when disconnected", () => {
    const html = renderToStaticMarkup(
      <AppShellHeader
        headerTitle="Demo"
        headerSubtitle="test"
        status="Disconnected"
        health={null}
        selected={null}
        messagesLength={0}
        isWorking={false}
        onCompact={() => {}}
        onRetryEngine={() => {}}
      />,
    );
    // v5: disconnected state shows the danger-tinted Retry pill.
    expect(html).toContain("bg-danger");
    expect(html).toContain("Retry engine");
  });

  it("provides keyboard shortcuts with a ? overlay on the RightKit shortcut registry", () => {
    // Configurable bindings live in the shared schema.
    expect(shortcutSchemaSource).toContain('id: "toggle_sidebar"');
    expect(shortcutSchemaSource).toContain('defaultBinding: "mod+b"');
    expect(shortcutSchemaSource).toContain('defaultBinding: "mod+l"');
    // Utility-panel toggling remains a target-only shell action.
    expect(appBodySource).toContain('chord: "mod+."');
    // The "?" binding is declared by AppBody and listed in the overlay.
    expect(appBodySource).toContain('chord: "?"');
    // The overlay is RightKit's ShortcutHelp rendered from the registry, mounted by AppBody.
    expect(appBodySource).toContain("ShortcutHelp");
    expect(appBodySource).toContain("@rightkit/react-shell/react");
    expect(appBodySource).toContain('title="Keyboard shortcuts"');
    // AppBody attaches the RightKit-backed registry to the window and renders the overlay.
    expect(appBodySource).toContain("rightKitShortcuts.attach()");
    expect(appBodySource).toContain("<ShortcutHelp");
    expect(appBodySource).toContain("shortcutsOpen");
  });

  it("wires ⌘K palette toggle and shows hint in title bar", () => {
    // AppFrame handles ⌘K globally
    expect(appBodySource).not.toContain('key: "mod+k"');
    // v5: TitleBar hands palette trigger to onOpenPalette; the search icon
    // replaces the inline ⌘K chip so the chrome stays inspector-tight.
    expect(titleBarSource).toContain("onOpenPalette");
    expect(titleBarSource).toContain('label="Search sessions"');
  });

  it("provides ⌘B sidebar toggle and ⌘. agents toggle shortcuts", () => {
    // Sidebar collapse state was lifted to AppFrame (see hooks/useSidebarCollapsed);
    // the shortcut still routes through AppBody which now binds onToggleSidebar.
    expect(appBodySource).toContain("onToggleSidebar");
    expect(appBodySource).toContain("toggleAgents");
    expect(appBodySource).toContain("focusComposer");
    expect(appBodySource).toContain("composerRef");
  });

  it("empty state shows 2-3 example prompts with clean copy", () => {
    expect(sessionEmptyStateSource).toContain("What should the host app do?");
    expect(sessionEmptyStateSource).toContain("Write tests for the current file");
    expect(sessionEmptyStateSource).toContain("Fix the TypeScript errors in this project");
    expect(sessionEmptyStateSource).toContain("Refactor this module to reduce complexity");
    expect(sessionEmptyStateSource).toContain("Open a project to begin");
    expect(sessionEmptyStateSource).toContain("Choose project");
  });

  it("motion restraint — no slide-in on transcript items, 120-180ms ease-out", () => {
    // The msg-in animation is opacity-only (no translateY)
    expect(indexCss).toMatch(/@keyframes cr-msg-in\s*\{[^}]*opacity/s);
    expect(indexCss).not.toMatch(/@keyframes cr-msg-in\s*\{[^}]*translateY/s);
    expect(indexCss).toContain("cr-msg-in");
    // Ease-out curves are defined
    expect(indexCss).toContain("--ease-out:");
    // Button transitions use the brand easing
    expect(indexCss).toMatch(/button\s*\{[^}]*var\(--ease-out\)/s);
  });
});
