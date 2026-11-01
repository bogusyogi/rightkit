import { useState } from "react";
import { Checkbox, ConfirmDialog, Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { ExecutionNetworkDefault, ExecutionNetworkProfile } from "@rightkit/agent-ui-core/lib/types";
import type { NetworkProfileDraft } from "./types.js";
import { TextSettingInput } from "./TextSettingInput.js";

interface ExecutionNetworkProfilesPanelProps {
  profiles: ExecutionNetworkProfile[];
  pendingId: string | null;
  workspaceRoot: string;
  onSaveDefinition: (
    profileId: string,
    body: {
      name?: string;
      default?: ExecutionNetworkDefault;
      allowHosts?: string[];
      allowPrivateNetworks?: boolean;
      denyPrivateNetworks?: boolean;
    },
  ) => void;
  onDeleteDefinition: (profileId: string) => void;
}

export function ExecutionNetworkProfilesPanel({
  profiles,
  pendingId,
  workspaceRoot,
  onSaveDefinition,
  onDeleteDefinition,
}: ExecutionNetworkProfilesPanelProps) {
  const [draft, setDraft] = useState<NetworkProfileDraft>(() => emptyNetworkProfileDraft());
  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);
  const draftId = draft.id.trim();
  const saving = draftId ? pendingId === `profile:${draftId}` : false;
  const deleting = draftId ? pendingId === `profile-delete:${draftId}` : false;
  const saveDisabled = !draftId || saving || deleting;
  const deleteDisabled = !draftId || saving || deleting;
  return (
    <section className="grid gap-4 rounded-xl border border-line bg-panel p-4 max-w-[1100px]">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <h4 className="text-[13px] font-semibold text-text">Workspace network profile</h4>
          <p className="mt-1 text-[12px] text-text-3 truncate">
            {workspaceRoot || "workspace not selected"}
          </p>
        </div>
        <div className="flex gap-2">
          <button
            type="button"
            className="inline-flex items-center justify-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40 whitespace-nowrap"
            disabled={saveDisabled}
            onClick={() =>
              onSaveDefinition(draftId, {
                name: draft.name.trim() || draftId,
                default: draft.defaultMode,
                allowHosts: splitNetworkProfileHosts(draft.allowHosts),
                allowPrivateNetworks: draft.allowPrivateNetworks,
                denyPrivateNetworks: draft.denyPrivateNetworks,
              })
            }
          >
            <Icon name="check" />
            {saving ? "Saving profile" : "Save profile"}
          </button>
          <button
            type="button"
            className="inline-flex items-center justify-center gap-2 h-8 px-3 rounded-lg border border-danger/40 bg-transparent text-danger text-[13px] cursor-pointer hover:bg-danger/10 disabled:opacity-40 whitespace-nowrap"
            disabled={deleteDisabled}
            onClick={() => setConfirmDeleteOpen(true)}
          >
            <Icon name="trash" />
            {deleting ? "Deleting" : "Delete profile"}
          </button>
        </div>
      </div>
      <div className="grid gap-3 lg:grid-cols-[minmax(0,0.8fr)_minmax(0,1fr)_minmax(0,1.2fr)]">
        <TextSettingInput
          label="Profile ID"
          value={draft.id}
          onChange={(id) => setDraft((current) => ({ ...current, id }))}
        />
        <TextSettingInput
          label="Name"
          value={draft.name}
          onChange={(name) => setDraft((current) => ({ ...current, name }))}
        />
        <label className="flex flex-col gap-1 min-w-0">
          <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">Default</span>
          <Select<ExecutionNetworkDefault>
            label="Default network mode"
            value={draft.defaultMode}
            onChange={(defaultMode) => setDraft((current) => ({ ...current, defaultMode }))}
            options={[
              { value: "deny", label: "Deny" },
              { value: "allow", label: "Allow" },
            ]}
          />
        </label>
      </div>
      <label className="flex flex-col gap-1 min-w-0">
        <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">Allow hosts</span>
        <textarea
          className="min-h-[74px] resize-y rounded-lg border border-line bg-panel-2 px-3 py-2 text-[12px] text-text outline-none focus:border-accent/60"
          value={draft.allowHosts}
          onChange={(event) =>
            setDraft((current) => ({ ...current, allowHosts: event.target.value }))
          }
        />
      </label>
      <div className="flex flex-wrap gap-3">
        <Checkbox
          label="Allow private networks"
          checked={draft.allowPrivateNetworks}
          onChange={(allowPrivateNetworks) =>
            setDraft((current) => ({ ...current, allowPrivateNetworks }))
          }
        />
        <Checkbox
          label="Deny private networks"
          checked={draft.denyPrivateNetworks}
          onChange={(denyPrivateNetworks) =>
            setDraft((current) => ({ ...current, denyPrivateNetworks }))
          }
        />
      </div>
      <div className="flex flex-wrap gap-2">
        {profiles.map((profile) => (
          <button
            type="button"
            key={profile.id}
            className="inline-flex items-center gap-2 max-w-full rounded-lg border border-line bg-panel-2 px-3 py-2 text-left text-[12px] text-text-2 hover:text-text"
            onClick={() => setDraft(networkProfileDraftFrom(profile))}
          >
            <span className="font-semibold text-text truncate">{profile.name}</span>
            <span className="text-text-3 truncate">{profile.id}</span>
            <span className="text-text-3">{profile.default}</span>
          </button>
        ))}
      </div>
      {confirmDeleteOpen ? (
        <ConfirmDialog
          title="Delete network profile?"
          description={`Delete network profile "${draft.name.trim() || draftId}"? Sessions using it fall back to the workspace default.`}
          confirmLabel="Delete"
          danger
          onCancel={() => setConfirmDeleteOpen(false)}
          onConfirm={() => {
            setConfirmDeleteOpen(false);
            onDeleteDefinition(draftId);
          }}
        />
      ) : null}
    </section>
  );
}

function emptyNetworkProfileDraft(): NetworkProfileDraft {
  return {
    id: "workspace-egress",
    name: "Workspace egress",
    defaultMode: "deny",
    allowHosts: "",
    allowPrivateNetworks: false,
    denyPrivateNetworks: true,
  };
}

function networkProfileDraftFrom(profile: ExecutionNetworkProfile): NetworkProfileDraft {
  return {
    id: profile.id,
    name: profile.name,
    defaultMode: profile.default,
    allowHosts: profile.allowHosts.join("\n"),
    allowPrivateNetworks: profile.allowPrivateNetworks,
    denyPrivateNetworks: profile.denyPrivateNetworks,
  };
}

function splitNetworkProfileHosts(value: string): string[] {
  return value
    .split(/[\n,]+/)
    .map((host) => host.trim())
    .filter(Boolean);
}
