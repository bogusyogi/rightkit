import { openExternal } from "@rightkit/agent-ui-core/lib/secureStore";
import type { ReferenceAction, ReferenceMeta } from "@rightkit/agent-ui-core/lib/types/references";
import {
  projectLayoutOrDefault,
  useLayoutStore,
  type DockSurface,
} from "@rightkit/agent-ui-core/state/layoutStore";
import { useRightPanelStore, type RightPanelTab } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { normalizePath, useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";

export type ActionableReferenceAction = Exclude<ReferenceAction, "remove">;

function relativeWorkspacePath(destination: string): string {
  const root = useWorkspaceStore.getState().workspaceRoot.replaceAll("\\", "/").replace(/\/+$/, "");
  const path = destination.replaceAll("\\", "/");
  return normalizePath(root && path.startsWith(`${root}/`) ? path.slice(root.length + 1) : path);
}

function openDock(surface: DockSurface, tab: RightPanelTab) {
  const projectId = useWorkspaceStore.getState().workspaceRoot.trim() || "default";
  const layout = useLayoutStore.getState();
  const visible = projectLayoutOrDefault(layout.byProject, projectId).visible;
  layout.setVisible(projectId, visible.includes(surface) ? visible : [...visible, surface]);
  layout.setRightTab(projectId, tab);
  useRightPanelStore.getState().setTab(tab);
  useSidePanelStore.getState().setOpen(true);
}

function isSafeExternal(destination: string): boolean {
  try {
    const protocol = new URL(destination).protocol;
    return protocol === "https:" || protocol === "http:" || protocol === "mailto:";
  } catch {
    return false;
  }
}

/** Execute a transcript reference action through the desktop's existing,
 * bounded host surfaces. External destinations cross the secure browser
 * boundary; local paths only open the daemon-rooted workspace viewer. */
export async function performReferenceAction(
  reference: ReferenceMeta,
  action: ActionableReferenceAction,
): Promise<void> {
  if (action === "copy_path") {
    await navigator.clipboard.writeText(reference.destination);
    return;
  }

  if (reference.kind === "external_link" || reference.kind === "repository") {
    if (isSafeExternal(reference.destination)) await openExternal(reference.destination);
    return;
  }

  if (reference.kind === "change_set") {
    openDock("review", "review");
    return;
  }

  if (reference.kind === "artifact") {
    openDock("artifacts", "artifacts");
    return;
  }

  if (
    reference.kind === "workspace_file" ||
    reference.kind === "generated_document" ||
    reference.kind === "image"
  ) {
    if (isSafeExternal(reference.destination)) {
      await openExternal(reference.destination);
      return;
    }
    useWorkspaceStore.getState().openFile(relativeWorkspacePath(reference.destination));
    openDock("files", "files");
  }
}
