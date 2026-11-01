import { open } from "@tauri-apps/plugin-dialog";
import type React from "react";
import {
  setPermissionMode,
  switchGitBranch,
  type Attachment,
  type PermissionMode,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, SessionRow } from "@rightkit/agent-ui-core/lib/types";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";
import { permissionLabel } from "@rightkit/agent-ui-core/lib/utils";
import { useReferenceStore } from "@rightkit/agent-ui-core/state/referenceStore";
import type { ConfirmationRequest } from "@rightkit/agent-ui-core/hooks/useConfirmationDialog";

const ACCEPTED_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

async function fileToAttachment(file: File): Promise<Attachment | null> {
  if (!ACCEPTED_IMAGE_TYPES.includes(file.type)) return null;
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result ?? "");
      const base64 = dataUrl.split(",", 2)[1];
      resolve(
        base64
          ? {
              type: "image",
              data: base64,
              mimeType: file.type,
              id: crypto.randomUUID(),
              filename: file.name,
              sizeBytes: file.size,
            }
          : null,
      );
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

interface UseAppShellActionsParams {
  workspaceRoot: string;
  gitInfo: { current: string; branches: string[] } | null;
  setWorkspaceRoot: (path: string) => void;
  setShowBranchMenu: (value: boolean) => void;
  setBranchSwitchPending: (value: string | null) => void;
  setGitInfo: (value: { current: string; branches: string[] } | null) => void;
  config: DaemonConfig | null;
  selected: SessionRow | null;
  permissionMode: PermissionMode;
  setPermissionModeState: (value: PermissionMode) => void;
  setStatus: (value: string) => void;
  messageOf: (error: unknown, fallback: string) => string;
  composerState: {
    addAttachments: (attachments: Attachment[]) => void;
  };
  confirm: (request: ConfirmationRequest) => Promise<boolean>;
}

export function useAppShellActions(params: UseAppShellActionsParams) {
  const {
    workspaceRoot,
    gitInfo,
    setWorkspaceRoot,
    setShowBranchMenu,
    setBranchSwitchPending,
    setGitInfo,
    config,
    selected,
    permissionMode,
    setPermissionModeState,
    setStatus,
    messageOf,
    composerState,
    confirm,
  } = params;

  function setWorkspace(path: string) {
    setWorkspaceRoot(path);
    localStorage.setItem("coderight.workspace", path);
  }

  async function switchBranch(branch: string) {
    const target = branch.trim();
    if (!target || !workspaceRoot.trim()) return;
    if (target === gitInfo?.current) {
      setShowBranchMenu(false);
      return;
    }

    setBranchSwitchPending(target);
    try {
      let result = await switchGitBranch(workspaceRoot, target, false);
      if (!result.switched && result.dirty) {
        const proceed = await confirm({
          title: "Switch branches with uncommitted changes?",
          description: `Switch to “${target}” even though this worktree has uncommitted changes? Your current changes will remain in the worktree.`,
          confirmLabel: "Switch branch",
        });
        if (!proceed) {
          setStatus("Branch switch cancelled");
          return;
        }
        result = await switchGitBranch(workspaceRoot, target, true);
      }
      setGitInfo({ current: result.current, branches: result.branches });
      setShowBranchMenu(false);
      setStatus(result.message || `Switched to ${result.current}`);
    } catch (err) {
      setStatus(messageOf(err, "Branch switch failed"));
    } finally {
      setBranchSwitchPending(null);
    }
  }

  async function pickDirectory() {
    const picked = await open({ directory: true, multiple: false });
    return typeof picked === "string" ? picked : null;
  }

  async function pickWorkspace() {
    try {
      const picked = await pickDirectory();
      if (picked) setWorkspace(picked);
      return picked;
    } catch (err) {
      setStatus(messageOf(err, "Open folder failed"));
      return null;
    }
  }

  async function pickRecipeFile(installRecipeFromFile: (path: string) => Promise<void>) {
    const picked = await open({
      directory: false,
      multiple: false,
      filters: [{ name: "Recipe document", extensions: ["json", "yaml", "yml"] }],
    });
    if (typeof picked === "string") {
      await installRecipeFromFile(picked);
    }
  }

  async function selectPermissionMode(next: PermissionMode) {
    if (!config || !selected || next === permissionMode) return;
    setPermissionModeState(next);
    try {
      await setPermissionMode(config, selected.id, next);
      setStatus(`Permission mode: ${permissionLabel(next)}`);
    } catch (err) {
      setPermissionModeState(permissionMode);
      setStatus(messageOf(err, "Permission mode failed"));
    }
  }

  async function cyclePermissionMode(permissionCycle: PermissionMode[]) {
    const next =
      permissionCycle[(permissionCycle.indexOf(permissionMode) + 1) % permissionCycle.length];
    await selectPermissionMode(next);
  }

  async function addComposerImageFiles(files: File[]) {
    if (files.length === 0) return;
    try {
      const attachments = await Promise.all(files.map(fileToAttachment));
      const valid = attachments.filter((attachment): attachment is Attachment =>
        Boolean(attachment),
      );
      if (valid.length === 0) {
        setStatus("No supported image attachments found");
        return;
      }
      composerState.addAttachments(valid);
      const createdAt = new Date().toISOString();
      const references: ReferenceObject[] = valid.map((attachment) => ({
        id: attachment.id ?? crypto.randomUUID(),
        kind: "image",
        destination: attachment.filename ?? "pasted image",
        mime: attachment.mimeType,
        filename: attachment.filename,
        sizeBytes: attachment.sizeBytes,
        provenance: {
          source: "composer.paste",
          createdAt,
          trust: "user",
        },
        actions: ["remove"],
        previewUrl: `data:${attachment.mimeType};base64,${attachment.data}`,
      }));
      useReferenceStore.getState().addReferences(references);
      setStatus(`${valid.length} image attachment${valid.length === 1 ? "" : "s"} ready`);
    } catch (err) {
      setStatus(messageOf(err, "Image paste failed"));
    }
  }

  async function pasteComposerImages(event: React.ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(event.clipboardData.items)
      .filter((item) => ACCEPTED_IMAGE_TYPES.includes(item.type))
      .map((item) => item.getAsFile())
      .filter((file): file is File => Boolean(file));
    if (files.length === 0) return;
    event.preventDefault();
    await addComposerImageFiles(files);
  }

  return {
    switchBranch,
    pickDirectory,
    pickWorkspace,
    pickRecipeFile,
    cyclePermissionMode,
    selectPermissionMode,
    addComposerImageFiles,
    pasteComposerImages,
  };
}
