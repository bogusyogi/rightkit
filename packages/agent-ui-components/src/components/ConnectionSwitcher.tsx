import { useEffect, useRef, useState } from "react";
import { Checkbox, Popover, Select } from "@rightkit/react-shell/react";

import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import type { InferenceSelection } from "@rightkit/agent-ui-core/lib/types/inference";
import {
  useConnectionProfilesStore,
  type ConnectionProfile,
  type ConnectionProfileKind,
  type EditableConnectionInput,
} from "@rightkit/agent-ui-core/state/connectionProfiles";

export interface ConnectionSwitcherProps {
  config: DaemonConfig | null;
  activeSelection?: InferenceSelection | null;
  disabled?: boolean;
  onSelectionChange?: (selection: InferenceSelection) => void;
  onOpenConnectionSettings?: () => void;
}

interface EditorState {
  id: string | null;
  name: string;
  normalizedBaseUrl: string;
  protocol: "anthropic_compatible" | "openai_compatible";
  authKind: "api_key_header" | "bearer" | "none" | "x_api_key";
  authHeaderName: string;
  secret: string;
  trustPrivateNetwork: boolean;
}

const EMPTY_EDITOR: EditorState = {
  id: null,
  name: "",
  normalizedBaseUrl: "",
  protocol: "openai_compatible",
  authKind: "none",
  authHeaderName: "",
  secret: "",
  trustPrivateNetwork: false,
};

function editorForProfile(profile: ConnectionProfile): EditorState {
  return {
    ...EMPTY_EDITOR,
    id: profile.connectionId,
    name: profile.label,
    normalizedBaseUrl: profile.url ?? "",
    protocol: profile.protocol,
    authKind: profile.authKind,
    authHeaderName: profile.authHeaderName ?? "",
  };
}

function kindLabel(kind: ConnectionProfileKind): string {
  if (kind === "local") return "Local";
  if (kind === "remote") return "Remote";
  return "Cloud";
}

function profileSummary(profile: ConnectionProfile): string {
  const state = profile.state.replaceAll("_", " ");
  const modelCount = `${profile.models.length} model${profile.models.length === 1 ? "" : "s"}`;
  return `${kindLabel(profile.kind)} · ${state} · ${modelCount}`;
}

export function ConnectionSwitcher({
  config,
  activeSelection = null,
  disabled = false,
  onSelectionChange,
  onOpenConnectionSettings,
}: ConnectionSwitcherProps) {
  const profiles = useConnectionProfilesStore((state) => state.profiles);
  const storeActiveProfileId = useConnectionProfilesStore((state) => state.activeProfileId);
  const loading = useConnectionProfilesStore((state) => state.loading);
  const pendingAction = useConnectionProfilesStore((state) => state.pendingAction);
  const error = useConnectionProfilesStore((state) => state.error);
  const load = useConnectionProfilesStore((state) => state.load);
  const selectProfile = useConnectionProfilesStore((state) => state.selectProfile);
  const save = useConnectionProfilesStore((state) => state.save);
  const remove = useConnectionProfilesStore((state) => state.remove);
  const probe = useConnectionProfilesStore((state) => state.probe);
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [removeTarget, setRemoveTarget] = useState<string | null>(null);

  const selectionKey = activeSelection ? JSON.stringify(activeSelection) : "";
  const configRef = useRef(config);
  const selectionRef = useRef(activeSelection);
  configRef.current = config;
  selectionRef.current = activeSelection;
  // biome-ignore lint/correctness/useExhaustiveDependencies: stable scalar keys intentionally reload ref-backed config & selection.
  useEffect(() => {
    void load(configRef.current, selectionRef.current);
  }, [config?.token, config?.url, load, selectionKey]);

  const activeProfile =
    profiles.find((profile) => profile.id === storeActiveProfileId) ?? profiles[0] ?? null;

  const choose = (profileId: string) => {
    const selection = selectProfile(profileId);
    if (selection) onSelectionChange?.(selection);
    setOpen(false);
  };

  const submitEditor = async () => {
    if (!editor) return;
    const input: EditableConnectionInput = {
      id: editor.id ?? undefined,
      name: editor.name.trim(),
      normalizedBaseUrl: editor.normalizedBaseUrl.trim(),
      protocol: editor.protocol,
      authKind: editor.authKind,
      authHeaderName: editor.authHeaderName.trim() || null,
      publicHeaders: {},
      manualModelIds: [],
      trustPrivateNetwork: editor.trustPrivateNetwork,
    };
    if (!input.name || !input.normalizedBaseUrl) return;
    if (editor.authKind === "api_key_header" && !editor.authHeaderName.trim()) return;
    if (await save(config, input, editor.secret)) setEditor(null);
  };

  const confirmRemove = async (profile: ConnectionProfile) => {
    if (removeTarget !== profile.id) {
      setRemoveTarget(profile.id);
      return;
    }
    setRemoveTarget(null);
    if (await remove(config, profile.id)) setEditor(null);
  };

  return (
    <div className="relative" data-testid="connection-switcher">
      <button
        ref={triggerRef}
        type="button"
        aria-label="Choose connection profile"
        aria-expanded={open}
        aria-haspopup="menu"
        disabled={disabled || loading}
        onClick={() => setOpen((value) => !value)}
        className="cr-control-pill inline-flex h-7 max-w-[220px] items-center gap-1.5 rounded-md border border-transparent px-2 text-[12px] text-text-2 hover:bg-panel-2 hover:text-text disabled:opacity-50"
      >
        <Icon
          name={activeProfile?.kind === "remote" ? "server" : "folder"}
          size={13}
        />
        <span className="truncate">{activeProfile?.label ?? "Choose connection"}</span>
        <Icon name="chevron" size={10} />
      </button>

      {open ? (
        <Popover
          anchor={triggerRef.current}
          onClose={() => setOpen(false)}
          label="Connection profiles"
          placement="bottom-end"
          width={330}
        >
          {profiles.length > 0 ? (
            <div className="flex flex-col gap-0.5">
              {profiles.map((profile) => {
                const active = profile.id === storeActiveProfileId;
                const isBusy = pendingAction?.endsWith(profile.id) ?? false;
                return (
                  <div className="flex items-center gap-1 rounded-md hover:bg-panel-2" key={profile.id}>
                    <button
                      type="button"
                      role="menuitemradio"
                      aria-checked={active}
                      className="flex min-w-0 flex-1 items-start gap-2 rounded-md px-2 py-1.5 text-left text-[12px] focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                      onClick={() => choose(profile.id)}
                    >
                      <Icon
                        name={profile.kind === "remote" ? "server" : "folder"}
                        size={13}
                      />
                      <span className="min-w-0 flex-1">
                        <strong className="block truncate text-text">{profile.label}</strong>
                        <span className="block truncate text-[11px] text-text-3">{profileSummary(profile)}</span>
                      </span>
                      {active ? <span aria-hidden="true" className="text-accent">✓</span> : null}
                    </button>
                    <button
                      type="button"
                      aria-label={`Probe ${profile.label}`}
                      disabled={Boolean(pendingAction) || isBusy}
                      className="rounded px-1.5 py-1 text-[11px] text-text-3 hover:bg-panel hover:text-text disabled:opacity-40"
                      onClick={() => void probe(config, profile.id)}
                    >
                      <Icon name="activity" size={12} />
                    </button>
                    {profile.authority === "direct" ? (
                      <>
                        <button
                          type="button"
                          aria-label={`Edit ${profile.label}`}
                          className="rounded px-1.5 py-1 text-[11px] text-text-3 hover:bg-panel hover:text-text"
                          onClick={() => setEditor(editorForProfile(profile))}
                        >
                          <Icon name="edit" size={12} />
                        </button>
                        <button
                          type="button"
                          aria-label={removeTarget === profile.id ? `Confirm remove ${profile.label}` : `Remove ${profile.label}`}
                          disabled={Boolean(pendingAction)}
                          className="rounded px-1.5 py-1 text-[11px] text-danger hover:bg-panel disabled:opacity-40"
                          onClick={() => void confirmRemove(profile)}
                        >
                          <Icon name={removeTarget === profile.id ? "check" : "trash"} size={12} />
                        </button>
                      </>
                    ) : null}
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="px-2 py-2 text-[12px] text-text-3">No connection profiles are available.</p>
          )}

          {error ? <p className="m-1 rounded border border-danger/40 bg-danger/10 p-2 text-[11px] text-danger">{error}</p> : null}

          {editor ? (
            <form
              aria-label={editor.id ? "Edit connection profile" : "Add connection profile"}
              className="mt-1 grid gap-2 border-t border-line p-2"
              onSubmit={(event) => {
                event.preventDefault();
                void submitEditor();
              }}
            >
              <strong className="text-[12px] text-text">{editor.id ? "Edit connection" : "Add connection"}</strong>
              <input
                aria-label="Connection name"
                className="h-8 rounded border border-line bg-bg px-2 text-[12px] text-text outline-none focus:border-accent"
                placeholder="Connection name"
                value={editor.name}
                onChange={(event) => setEditor({ ...editor, name: event.target.value })}
              />
              <input
                aria-label="Connection URL"
                className="h-8 rounded border border-line bg-bg px-2 text-[12px] text-text outline-none focus:border-accent"
                placeholder="https://gateway.example/v1"
                type="url"
                value={editor.normalizedBaseUrl}
                onChange={(event) => setEditor({ ...editor, normalizedBaseUrl: event.target.value })}
              />
              <div className="grid grid-cols-2 gap-2">
                <label className="grid gap-1 text-[11px] text-text-3">
                  Protocol
                  <Select value={editor.protocol} options={[{ value: "openai_compatible", label: "OpenAI-compatible" }, { value: "anthropic_compatible", label: "Anthropic-compatible" }]} onChange={(value) => setEditor({ ...editor, protocol: value as EditorState["protocol"] })} label="Connection protocol" className="h-8 rounded border border-line bg-bg px-1 text-[11px] text-text" />
                </label>
                <label className="grid gap-1 text-[11px] text-text-3">
                  Auth
                  <Select value={editor.authKind} options={[{ value: "none", label: "None" }, { value: "bearer", label: "Bearer" }, { value: "x_api_key", label: "X-API-Key" }, { value: "api_key_header", label: "Custom header" }]} onChange={(value) => setEditor({ ...editor, authKind: value as EditorState["authKind"] })} label="Connection authentication" className="h-8 rounded border border-line bg-bg px-1 text-[11px] text-text" />
                </label>
              </div>
              {editor.authKind === "api_key_header" ? (
                <input
                  aria-label="Connection auth header"
                  className="h-8 rounded border border-line bg-bg px-2 text-[12px] text-text outline-none focus:border-accent"
                  placeholder="Authorization header name"
                  value={editor.authHeaderName}
                  onChange={(event) => setEditor({ ...editor, authHeaderName: event.target.value })}
                />
              ) : null}
              {editor.authKind !== "none" ? (
                <input
                  aria-label="Connection secret"
                  className="h-8 rounded border border-line bg-bg px-2 text-[12px] text-text outline-none focus:border-accent"
                  placeholder="Leave empty to keep the saved secret"
                  type="password"
                  value={editor.secret}
                  onChange={(event) => setEditor({ ...editor, secret: event.target.value })}
                />
              ) : null}
              <Checkbox checked={editor.trustPrivateNetwork} onChange={(checked) => setEditor({ ...editor, trustPrivateNetwork: checked })} label="Trust private network" className="inline-flex items-center gap-2 text-[11px] text-text-3" />
              <div className="flex justify-end gap-1">
                <button type="button" className="rounded px-2 py-1 text-[11px] text-text-3 hover:bg-panel-2" onClick={() => setEditor(null)}>
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={
                    Boolean(pendingAction) ||
                    !editor.name.trim() ||
                    !editor.normalizedBaseUrl.trim() ||
                    (editor.authKind === "api_key_header" && !editor.authHeaderName.trim())
                  }
                  className="rounded bg-accent px-2 py-1 text-[11px] font-semibold text-on-accent disabled:opacity-40"
                >
                  {pendingAction ? "Saving…" : "Save connection"}
                </button>
              </div>
            </form>
          ) : (
            <div className="mt-1 flex gap-1 border-t border-line p-1">
              <button
                type="button"
                className="flex-1 rounded px-2 py-1.5 text-left text-[11px] text-text-2 hover:bg-panel-2"
                onClick={() => setEditor({ ...EMPTY_EDITOR })}
              >
                <Icon name="plus" size={12} /> Add connection
              </button>
              <button
                type="button"
                className="rounded px-2 py-1.5 text-[11px] text-text-2 hover:bg-panel-2"
                onClick={onOpenConnectionSettings}
              >
                Manage connections
              </button>
            </div>
          )}
        </Popover>
      ) : null}
    </div>
  );
}

export default ConnectionSwitcher;
