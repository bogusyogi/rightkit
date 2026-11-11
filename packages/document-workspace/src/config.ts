// Host configuration of the document workspace. The workspace is a shell a host composes: it names no
// product, holds no product's legal, licence or update wiring, and reads everything product-specific
// from the one object the host passes to `configureDocumentWorkspace` at startup, before the first
// render. Values are read where they are used, never at module load.
import type { ReactNode } from "react";
import type { LegalStepConfig } from "@rightkit/onboarding";
import type { SettingsSection } from "@rightkit/settings-ui";
import { configureDocumentHost } from "@rightkit/document-core/host";

/** The first-run legal gate, shown in place of the workspace until the acceptance receipt in
 *  settings is current. The documents, acceptance version and material hash come from
 *  `configureLegal` (`state/legal`); everything shown on the gate comes from here. */
export interface WorkspaceFirstRunConfig {
  /** The gate's look and copy: `theme`, `trustLine`, `agreementStatement`, `wordmarkFontUrl` and the
   *  rest of `LegalStepConfig`. `appName` defaults to the host's `appName`; `documents` to the
   *  documents given to `configureLegal`; `onQuit` closes the window. */
  legal: Omit<LegalStepConfig, "appName" | "documents" | "onQuit"> &
    Partial<Pick<LegalStepConfig, "appName" | "documents" | "onQuit">>;
  /** The gate's own fixed window size, in logical pixels. Absent: the window is left as it is. */
  gateWindow?: { width: number; height: number };
}

/** An update the host's background check found, announced once with a toast. */
export interface WorkspaceBackgroundUpdate {
  version: string;
  /** `"patch"` reads "patch" in the toast; anything else reads "update". */
  tier: string;
}

export interface DocumentWorkspaceConfig {
  /** Product name in user-facing copy. Sets the one name store of every document package. */
  appName: string;
  /** The host's native identifier (event, scheme, header, storage and QA-global prefix); see
   *  `DocumentHostConfig.id` in `@rightkit/document-core/host`. Default `"rk-document"`. */
  id?: string;
  /** The running build's version, shown in diagnostics. */
  appVersion: string;
  /** Address for "Email diagnostics". Absent: the action is not offered. */
  supportEmail?: string;
  /** First-run legal gate. Absent: no gate; the workspace opens directly. */
  firstRun?: WorkspaceFirstRunConfig;
  /** Settings the host owns. */
  settings?: {
    /** The host's About page (brand card, updates, licence, legal documents). Absent: no About section. */
    about?: () => ReactNode;
    /** Further sections, merged after the workspace's own (same id replaces the workspace's). */
    sections?: readonly SettingsSection[];
  };
  /** Advisory background update check, run once per launch while automatic update checks are on.
   *  Resolve null when there is nothing to announce. Absent: no background check. */
  backgroundUpdateCheck?: () => Promise<WorkspaceBackgroundUpdate | null>;
}

let current: DocumentWorkspaceConfig = { appName: "App", appVersion: "0.0.0" };

/** Configure the workspace once at startup, before the first render. */
export function configureDocumentWorkspace(config: DocumentWorkspaceConfig): void {
  if (!config.appVersion?.trim()) throw new Error("configureDocumentWorkspace: appVersion is required");
  configureDocumentHost({ appName: config.appName, ...(config.id === undefined ? {} : { id: config.id }) });
  current = config;
}

/** The configuration the host passed (defaults until it does). */
export function workspaceConfig(): Readonly<DocumentWorkspaceConfig> {
  return current;
}
