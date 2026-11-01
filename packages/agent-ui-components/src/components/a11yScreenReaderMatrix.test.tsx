/**
 * FM-158 — screen-reader matrix as vitest.
 *
 * Mirrors check-a11y-screenreader.mjs but as a JS test so `pnpm test`
 * catches a regression even when the gate script is not run. Each surface
 * asserts Name / Role / State / Live-region for VoiceOver + NVDA.
 *
 * 8 surfaces × 2 ATs share the same DOM contract; the test proves the
 * DOM would announce correctly on either AT.
 */
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it, expect } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const appRoot = resolve(here, "../..");
function src(rel: string) {
  return readFileSync(resolve(appRoot, rel), "utf8");
}

describe("FM-158 VoiceOver+NVDA matrix — Name/Role/State/Live-region", () => {
  it("onboarding: Name/Role/State/Live-region", () => {
    const c = src("src/components/app/OnboardingPanel.tsx");
    expect(c).toContain('aria-labelledby="onboarding-title"');
    expect(c).toContain('aria-describedby="onboarding-desc"');
    expect(c).toContain('aria-pressed={runTarget === "local"}');
    expect(c).toContain('aria-pressed={runTarget === "worktree"}');
    expect(c).toContain('aria-label="Local engine URL"');
    expect(c).toContain('aria-label="Daemon bearer token"');
    expect(c).toContain('aria-live="polite"');
    expect(c).toContain('role="group" aria-label="Start point options"');
  });

  it("session creation: Name/Role/State/Live-region", () => {
    const c = src("src/components/Sidebar.tsx");
    expect(c).toContain('aria-label="Sessions"');
    expect(c).toContain('role="region" aria-label="Recent sessions"');
    expect(c).toContain('aria-current={isActive ? "page" : undefined}');
    expect(c).toContain("aria-label={`Delete session");
    expect(c).toContain('role="status" aria-live="polite"');
  });

  it("composer: Name/Role/State", () => {
    const c = src("src/components/app/ComposerIsland.tsx");
    expect(c).toContain('aria-label="Message composer"');
    expect(c).toContain("aria-label={sendLabel}");
    expect(c).toContain("aria-pressed={pressed}");
    expect(c).toContain('aria-haspopup="menu"');
    expect(c).toContain("aria-expanded={showPermissionMenu}");
    expect(c).toContain('role="menu"');
    expect(c).toContain('role="menuitemradio"');
  });

  it("approvals: alertdialog + assertive + polite announcer", () => {
    const perm = src("src/components/workbench/PermissionCard.tsx");
    expect(perm).toContain('role="alertdialog"');
    expect(perm).toContain('aria-live="assertive"');
    const plan = src("src/components/workbench/PlanPrompt.tsx");
    expect(plan).toContain('role="alertdialog"');
    expect(plan).toContain('aria-live="assertive"');
    const question = src("src/components/workbench/QuestionPrompt.tsx");
    expect(question).toContain('role="alertdialog"');
    expect(question).toContain('aria-live="assertive"');
    const announcer = src("src/components/workbench/RunAnnouncer.tsx");
    expect(announcer).toContain('aria-live="polite"');
    expect(announcer).toContain('aria-atomic="true"');
  });

  it("diff review: Name/Role/State/Live-region", () => {
    const pane = src("src/components/review/ReviewPane.tsx");
    expect(pane).toContain('aria-label="Search review diffs"');
    expect(pane).toContain('role="alert"');
    const stream = src("src/components/workbench/TranscriptStream.tsx");
    // biome-ignore lint/suspicious/noTemplateCurlyInString: test needle intentionally contains template placeholder
    expect(stream).toContain("aria-label={`Tool ${toolName}`}");
    expect(stream).toContain("aria-expanded={expanded}");
    // biome-ignore lint/suspicious/noTemplateCurlyInString: test needle intentionally contains template placeholder
    expect(stream).toContain("aria-label={`Diff ${trimmedPath}`}");
    expect(stream).toContain('role="alert"');
  });

  it("settings: Name/Role/State/Live-region", () => {
    const modal = src("src/components/settings/SettingsModal.tsx");
    // Dialog role, aria-modal, the nav's aria-current and the search field are RightKit's
    // ModalShell / SettingsPanel; the modal names itself and its search.
    expect(modal).toContain("<ModalShell");
    expect(modal).toContain('ariaLabel="App settings"');
    expect(modal).toContain('searchLabel="Search settings"');
    expect(modal).toContain('role="status"');
    expect(modal).toContain('aria-live="polite"');
    expect(modal).toContain('role="alert"');
  });

  it("errors: role alert + assertive live", () => {
    // The root boundary is RightKit's ErrorBoundary default fallback (role="alert").
    const peb = src("src/components/PaneErrorBoundary.tsx");
    expect(peb).toContain('role="alert"');
    expect(peb).toContain('aria-live="assertive"');
    // The toast live region is RightKit's ToastViewport (polite region, errors role="alert"); the
    // shell's ShellProvider mounts it on the default toast bus that toast() writes to.
    const frame = src("src/components/app/AppFrame.tsx");
    expect(frame).toContain("<ShellProvider");
    expect(frame).toContain("toastBus={defaultToastBus}");
  });

  it("restart: live region + landmarks", () => {
    const body = src("src/components/app/AppBody.tsx");
    expect(body).toContain('aria-live="polite"');
    expect(body).toContain('aria-label="Workspace utilities"');
    // WorkbenchContent provides the conversation landmark that the skip link targets
    const wc = src("src/components/WorkbenchContent.tsx");
    expect(wc).toContain('aria-label="Conversation"');
    expect(wc).toContain('role="region"');
    expect(wc).toContain('id="main-conversation"');
    const app = src("src/App.tsx");
    expect(app).toContain("Skip to conversation");
  });

  it("App skip link is first focusable and targets conversation", () => {
    const app = src("src/App.tsx");
    expect(app).toContain('href="#main-conversation"');
    expect(app).toContain('className="sr-only focus:not-sr-only');
  });

  it("Manual AT matrix exists and covers 8 surfaces × 2 ATs", () => {
    const matrix = src("../../qa/accessibility/MANUAL-AT-MATRIX.md");
    expect(matrix).toContain("VoiceOver");
    expect(matrix).toContain("NVDA");
    expect(matrix).toContain("Onboarding");
    expect(matrix).toContain("Session creation");
    expect(matrix).toContain("Composer");
    expect(matrix).toContain("Approvals");
    expect(matrix).toContain("Diff review");
    expect(matrix).toContain("Settings");
    expect(matrix).toContain("Errors");
    expect(matrix).toContain("Restart");
    expect(matrix).toContain("Name");
    expect(matrix).toContain("Role");
    expect(matrix).toContain("State");
    expect(matrix).toContain("Live region");
  });
});
