import { useMemo, type ReactNode } from "react";
import { ModalShell } from "@rightkit/react-shell/react";
import {
  SettingsPanel as PublishedSettingsPanel,
  mergeSections,
  type SettingsSection,
} from "@rightkit/settings-ui";
import { AboutSection } from "./sections/AboutSection.js";
import { DiagnosticsSection } from "./sections/DiagnosticsSection.js";
import { EditorSection } from "@rightkit/document-core/settings/sections/EditorSection";
import { FileAssociationsSection } from "@rightkit/document-core/settings/sections/FileAssociationsSection";
import { PrivacySection } from "./sections/PrivacySection.js";
import { ThemeSection } from "@rightkit/document-core/settings/sections/ThemeSection";
import { TypographySection } from "@rightkit/document-core/settings/sections/TypographySection";
import { isMac, isWindows } from "@rightkit/document-core/platform";
import { inTauri, isBrowserQaMode } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../appName.js";

const SETTINGS_MODAL_SLOTS = {
  overlay: "z-[60] bg-black/50 p-4 transition-opacity duration-[80ms] ease-out",
  root: "vr-pop vr-pop-modal flex h-[76vh] max-h-[620px] max-w-[calc(100vw-2rem)] overflow-hidden rounded-xl border border-line bg-panel-2 shadow-2xl",
} as const;

function Icon({ children }: { children: ReactNode }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </svg>
  );
}

const ICONS = {
  about: (
    <Icon>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </Icon>
  ),
  diagnostics: (
    <Icon>
      <path d="M4 18V6M8 18v-5M12 18V9M16 18v-8M20 18V4" />
    </Icon>
  ),
  files: (
    <Icon>
      <path d="M5 4h10l4 4v12H5z" />
      <path d="M15 4v4h4M8 13h8M8 16h6" />
    </Icon>
  ),
  theme: (
    <Icon>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 3a9 9 0 0 1 0 18" />
    </Icon>
  ),
  typography: (
    <Icon>
      <path d="M5 5h14M12 5v14M8 19h8" />
    </Icon>
  ),
  editor: (
    <Icon>
      <path d="M4 20h4l11-11-4-4L4 16v4Z" />
      <path d="m13.5 6.5 4 4" />
    </Icon>
  ),
  privacy: (
    <Icon>
      <rect x="5" y="10" width="14" height="10" rx="2" />
      <path d="M8 10V7a4 4 0 0 1 8 0v3" />
    </Icon>
  ),
};

/** A section body with the one-line description the old panel showed under the heading. */
function Body({ subtitle, children }: { subtitle?: string; children: ReactNode }) {
  return (
    <>
      {subtitle && <p className="mb-5 text-xs text-muted-2">{subtitle}</p>}
      {children}
    </>
  );
}

function viewRightSections(onHistoryPurged?: () => void): SettingsSection[] {
  const showFileAssociations = (isMac() || isWindows()) && (inTauri() || isBrowserQaMode());
  return [
    {
      id: "about",
      label: "About",
      group: "App",
      order: 100,
      icon: ICONS.about,
      hideSectionHeading: true,
      render: () => <AboutSection />,
    },
    {
      id: "files",
      label: "File associations",
      group: "App",
      order: 200,
      icon: ICONS.files,
      visible: () => showFileAssociations,
      render: () => (
        <Body subtitle={`Choose which document formats open in ${hostAppName()} by default.`}>
          <FileAssociationsSection />
        </Body>
      ),
    },
    {
      id: "diagnostics",
      label: "Diagnostics",
      group: "App",
      order: 300,
      icon: ICONS.diagnostics,
      render: () => (
        <Body subtitle="Local troubleshooting information and support tools.">
          <DiagnosticsSection />
        </Body>
      ),
    },
    {
      id: "theme",
      label: "Theme",
      group: "Preferences",
      order: 400,
      icon: ICONS.theme,
      render: () => (
        <Body subtitle={`Choose the surfaces and accents used across ${hostAppName()}.`}>
          <ThemeSection />
        </Body>
      ),
    },
    {
      id: "typography",
      label: "Typography",
      group: "Preferences",
      order: 500,
      icon: ICONS.typography,
      render: () => (
        <Body subtitle="Set interface and document reading type.">
          <TypographySection />
        </Body>
      ),
    },
    {
      id: "editor",
      label: "Editor",
      group: "Preferences",
      order: 600,
      icon: ICONS.editor,
      render: () => (
        <Body subtitle="Control writing and source-editing behavior.">
          <EditorSection />
        </Body>
      ),
    },
    {
      id: "privacy",
      label: "Privacy",
      group: "Preferences",
      order: 700,
      icon: ICONS.privacy,
      render: () => (
        <Body subtitle={`Choose what ${hostAppName()} remembers on this device.`}>
          <PrivacySection onHistoryPurged={onHistoryPurged} />
        </Body>
      ),
    },
  ];
}

/** Settings dialog: the published `SettingsPanel` (nav, headings, `aria-current`) inside the shell
 *  `ModalShell` (backdrop, Escape, focus trap and restore). Section bodies read and write through
 *  `useSettings()` (the native-document backend in `settings.tsx`), so the panel keeps no store. */
export function SettingsPanel({
  onClose,
  onHistoryPurged,
}: {
  onClose: () => void;
  onHistoryPurged?: () => void;
}) {
  const sections = useMemo(
    () => mergeSections([], viewRightSections(onHistoryPurged)),
    [onHistoryPurged],
  );
  return (
    <ModalShell
      label="Settings"
      onClose={onClose}
      width={800}
      dismissOn="click"
      slots={SETTINGS_MODAL_SLOTS}
    >
      <div data-settings-shell className="vr-rk h-full min-w-0 flex-1">
        <PublishedSettingsPanel
          className="h-full"
          sections={sections}
          title="Settings"
          defaultActiveId="about"
          hideSearch
          navFooter={
            <button
              type="button"
              onClick={onClose}
              aria-label="Close settings"
              title="Close settings"
              className="vr-press vr-focus mt-2 w-full rounded-md border border-line px-2 py-1.5 font-sans text-xs text-muted hover:bg-panel-2 hover:text-paper"
            >
              Close
            </button>
          }
        />
      </div>
    </ModalShell>
  );
}
