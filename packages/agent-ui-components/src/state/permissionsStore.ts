import { create } from "zustand";

import {
  addPermissionRule,
  addPersistentPermissionRule,
  deletePermissionRule,
  deletePersistentPermissionRule,
  exportPermissionRules,
  getWorkspaceTrust,
  importPermissionRules,
  listPermissionDenials,
  listPermissionRules,
  listPersistentPermissionRules,
  setWorkspaceTrust,
} from "@rightkit/agent-ui-core/lib/daemon";
import type {
  PermissionRuleDraft,
  PersistentPermissionRuleDraft,
} from "../components/settings/types";
import type {
  DaemonConfig,
  PermissionDenialRow,
  PermissionRuleRow,
  PersistentPermissionRuleRow,
  PersistentPermissionRuleStore,
  WorkspaceTrustLevel,
  WorkspaceTrustStatus,
} from "@rightkit/agent-ui-core/lib/types";
import { messageOf, type SetStatus } from "@rightkit/agent-ui-core/lib/utils";

const DEFAULT_RULE_DRAFT: PermissionRuleDraft = { pattern: "", effect: "deny" };
const DEFAULT_PERSISTENT_DRAFT: PersistentPermissionRuleDraft = {
  scope: "user",
  pattern: "",
  effect: "deny",
};

function permissionContextKey(config: DaemonConfig | null, scope: string): string {
  return config ? `${config.url}\u0000${config.token}\u0000${scope}` : "";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isPermissionRuleRecord(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return (
    typeof value.pattern === "string" &&
    value.pattern.trim().length > 0 &&
    (value.effect === "allow" || value.effect === "ask" || value.effect === "deny")
  );
}

function isPermissionRuleStore(value: unknown): value is PersistentPermissionRuleStore {
  if (!isRecord(value) || !Array.isArray(value.user) || !isRecord(value.projects)) return false;
  return (
    value.user.every(isPermissionRuleRecord) &&
    Object.values(value.projects).every(
      (rules) => Array.isArray(rules) && rules.every(isPermissionRuleRecord),
    )
  );
}

/** Permission reads are replaceable projections. A newer session/workspace
 * selection must be allowed to invalidate an older daemon response. */
let rulesLoadGeneration = 0;
let rulesContextKey = "";
let rulesContextRevision = 0;
let denialsLoadGeneration = 0;
let denialsContextKey = "";
let persistentLoadGeneration = 0;
let persistentContextKey = "";
let persistentContextRevision = 0;

/** Permissions settings: session-scoped rules + draft, durable user/project
 * rules + draft, JSON import/export drafts, workspace trust, and denials.
 * `selectedId` and `activeWorkspace` are threaded into actions per call. */
export type PermissionsStoreState = {
  rules: PermissionRuleRow[];
  ruleDraft: PermissionRuleDraft;
  ruleActionPending: string | null;
  persistentDraft: PersistentPermissionRuleDraft;
  persistentActionPending: string | null;
  persistentStorePath: string;
  persistentUserRules: PersistentPermissionRuleRow[];
  persistentProjectRules: PersistentPermissionRuleRow[];
  persistentError: string | null;
  jsonDraft: string;
  jsonPending: string | null;
  workspaceTrust: WorkspaceTrustStatus | null;
  workspaceTrustRoot: string;
  workspaceTrustPending: WorkspaceTrustLevel | null;
  denials: PermissionDenialRow[];
  denialStorePath: string;
  denialsLoading: boolean;
  denialsError: string | null;

  setRuleDraft: (draft: PermissionRuleDraft) => void;
  setPersistentDraft: (draft: PersistentPermissionRuleDraft) => void;
  setJsonDraft: (json: string) => void;
  resetWorkspaceTrust: () => void;

  loadWorkspaceTrust: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  applyWorkspaceTrust: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    trust: WorkspaceTrustLevel,
    setStatus: SetStatus,
  ) => Promise<void>;
  loadRules: (
    config: DaemonConfig | null,
    selectedId: string | null,
    setStatus: SetStatus,
  ) => Promise<void>;
  loadDenials: (
    config: DaemonConfig | null,
    selectedId: string | null,
    setStatus: SetStatus,
  ) => Promise<void>;
  loadPersistentRules: (
    config: DaemonConfig | null,
    activeWorkspace: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  loadAll: (
    config: DaemonConfig | null,
    selectedId: string | null,
    activeWorkspace: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  submitRule: (
    config: DaemonConfig | null,
    selectedId: string | null,
    setStatus: SetStatus,
  ) => Promise<void>;
  removeRule: (
    config: DaemonConfig | null,
    selectedId: string | null,
    rule: PermissionRuleRow,
    setStatus: SetStatus,
  ) => Promise<void>;
  submitPersistentRule: (
    config: DaemonConfig | null,
    activeWorkspace: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  removePersistentRule: (
    config: DaemonConfig | null,
    activeWorkspace: string,
    rule: PersistentPermissionRuleRow,
    setStatus: SetStatus,
  ) => Promise<void>;
  exportJson: (config: DaemonConfig | null, setStatus: SetStatus) => Promise<void>;
  importJson: (
    config: DaemonConfig | null,
    activeWorkspace: string,
    setStatus: SetStatus,
  ) => Promise<void>;
};

export const usePermissionsStore = create<PermissionsStoreState>((set, get) => ({
  rules: [],
  ruleDraft: DEFAULT_RULE_DRAFT,
  ruleActionPending: null,
  persistentDraft: DEFAULT_PERSISTENT_DRAFT,
  persistentActionPending: null,
  persistentStorePath: "",
  persistentUserRules: [],
  persistentProjectRules: [],
  persistentError: null,
  jsonDraft: "",
  jsonPending: null,
  workspaceTrust: null,
  workspaceTrustRoot: "",
  workspaceTrustPending: null,
  denials: [],
  denialStorePath: "",
  denialsLoading: false,
  denialsError: null,

  setRuleDraft(draft) {
    set({ ruleDraft: draft });
  },
  setPersistentDraft(draft) {
    set({ persistentDraft: draft });
  },
  setJsonDraft(json) {
    set({ jsonDraft: json });
  },
  resetWorkspaceTrust() {
    set({ workspaceTrust: null, workspaceTrustRoot: "", workspaceTrustPending: null });
  },

  async loadWorkspaceTrust(config, workspaceRoot, setStatus) {
    const requestedRoot = workspaceRoot.trim();
    if (!config || !requestedRoot) {
      set({ workspaceTrust: null, workspaceTrustRoot: "", workspaceTrustPending: null });
      return;
    }
    set({ workspaceTrust: null, workspaceTrustRoot: requestedRoot, workspaceTrustPending: null });
    try {
      const next = await getWorkspaceTrust(config, requestedRoot);
      if (get().workspaceTrustRoot === requestedRoot) {
        set({ workspaceTrust: next });
      }
    } catch (err) {
      if (get().workspaceTrustRoot === requestedRoot) {
        set({ workspaceTrust: null });
        setStatus(messageOf(err, "Workspace trust failed"));
      }
    }
  },

  async applyWorkspaceTrust(config, workspaceRoot, trust, setStatus) {
    const requestedRoot = workspaceRoot.trim();
    if (!config || !requestedRoot) return;
    set({ workspaceTrustRoot: requestedRoot, workspaceTrustPending: trust });
    try {
      const next = await setWorkspaceTrust(config, requestedRoot, trust);
      if (get().workspaceTrustRoot === requestedRoot) {
        set({ workspaceTrust: next });
        setStatus(`Workspace trust set to ${trust}`);
      }
    } catch (err) {
      if (get().workspaceTrustRoot === requestedRoot) {
        setStatus(messageOf(err, "Workspace trust update failed"));
      }
    } finally {
      if (get().workspaceTrustRoot === requestedRoot) {
        set({ workspaceTrustPending: null });
      }
    }
  },

  async loadRules(config, selectedId, setStatus) {
    const contextKey = permissionContextKey(config, selectedId ?? "");
    const generation = ++rulesLoadGeneration;
    rulesContextKey = contextKey;
    rulesContextRevision += 1;
    if (!config || !selectedId) {
      set({ rules: [] });
      return;
    }
    set({ rules: [] });
    try {
      const next = await listPermissionRules(config, selectedId);
      if (generation === rulesLoadGeneration && rulesContextKey === contextKey) {
        set({ rules: next });
      }
    } catch (err) {
      if (generation === rulesLoadGeneration && rulesContextKey === contextKey) {
        setStatus(messageOf(err, "Permission rules failed"));
      }
    }
  },

  async loadDenials(config, selectedId, setStatus) {
    const contextKey = permissionContextKey(config, selectedId ?? "");
    const generation = ++denialsLoadGeneration;
    denialsContextKey = contextKey;
    if (!config || !selectedId) {
      set({ denials: [], denialStorePath: "", denialsError: null, denialsLoading: false });
      return;
    }
    set({ denials: [], denialStorePath: "", denialsLoading: true, denialsError: null });
    try {
      const next = await listPermissionDenials(config, selectedId, 25);
      if (generation === denialsLoadGeneration && denialsContextKey === contextKey) {
        set({ denials: next.denials, denialStorePath: next.permissionDenialStorePath });
      }
    } catch (err) {
      if (generation === denialsLoadGeneration && denialsContextKey === contextKey) {
        const message = messageOf(err, "Permission denials failed");
        set({ denialsError: message });
        setStatus(message);
      }
    } finally {
      if (generation === denialsLoadGeneration && denialsContextKey === contextKey) {
        set({ denialsLoading: false });
      }
    }
  },

  async loadPersistentRules(config, activeWorkspace, setStatus) {
    const workspace = activeWorkspace.trim();
    const contextKey = permissionContextKey(config, workspace);
    const generation = ++persistentLoadGeneration;
    persistentContextKey = contextKey;
    persistentContextRevision += 1;
    if (!config) {
      set({
        persistentUserRules: [],
        persistentProjectRules: [],
        persistentStorePath: "",
        persistentError: null,
      });
      return;
    }
    set({
      persistentUserRules: [],
      persistentProjectRules: [],
      persistentStorePath: "",
      persistentError: null,
    });
    try {
      const userRules = await listPersistentPermissionRules(config, "user");
      if (generation !== persistentLoadGeneration || persistentContextKey !== contextKey) return;
      set({
        persistentStorePath: userRules.permissionRuleStorePath,
        persistentUserRules: userRules.rules,
      });
      if (workspace) {
        const projectRules = await listPersistentPermissionRules(config, "project", workspace);
        if (generation !== persistentLoadGeneration || persistentContextKey !== contextKey) return;
        set({
          persistentStorePath: projectRules.permissionRuleStorePath,
          persistentProjectRules: projectRules.rules,
        });
      }
    } catch (err) {
      if (generation === persistentLoadGeneration && persistentContextKey === contextKey) {
        const message = messageOf(err, "Durable permission rules failed");
        set({ persistentError: message });
        setStatus(message);
      }
    }
  },

  async loadAll(config, selectedId, activeWorkspace, setStatus) {
    await Promise.all([
      get().loadRules(config, selectedId, setStatus),
      get().loadPersistentRules(config, activeWorkspace, setStatus),
      get().loadDenials(config, selectedId, setStatus),
    ]);
  },

  async submitRule(config, selectedId, setStatus) {
    if (!config || !selectedId || get().ruleActionPending) return;
    const pattern = get().ruleDraft.pattern.trim();
    if (!pattern) {
      setStatus("Permission rule needs a tool pattern");
      return;
    }
    const contextKey = permissionContextKey(config, selectedId);
    const contextRevision = rulesContextRevision;
    set({ ruleActionPending: "add" });
    try {
      await addPermissionRule(config, selectedId, pattern, get().ruleDraft.effect);
      const contextIsCurrent =
        rulesContextRevision === contextRevision && rulesContextKey === contextKey;
      if (contextIsCurrent || contextRevision === 0) {
        await get().loadRules(config, selectedId, setStatus);
        if (rulesContextKey === contextKey) {
          set((state) => ({ ruleDraft: { ...state.ruleDraft, pattern: "" } }));
        }
      }
      setStatus("Permission rule added");
    } catch (err) {
      setStatus(messageOf(err, "Permission rule add failed"));
    } finally {
      set({ ruleActionPending: null });
    }
  },

  async removeRule(config, selectedId, rule, setStatus) {
    if (!config || !selectedId || get().ruleActionPending) return;
    const contextKey = permissionContextKey(config, selectedId);
    const contextRevision = rulesContextRevision;
    set({ ruleActionPending: String(rule.index) });
    try {
      await deletePermissionRule(config, selectedId, rule.index);
      const contextIsCurrent =
        rulesContextRevision === contextRevision && rulesContextKey === contextKey;
      if (contextIsCurrent || contextRevision === 0) {
        await get().loadRules(config, selectedId, setStatus);
      }
      setStatus("Permission rule removed");
    } catch (err) {
      setStatus(messageOf(err, "Permission rule remove failed"));
    } finally {
      set({ ruleActionPending: null });
    }
  },

  async submitPersistentRule(config, activeWorkspace, setStatus) {
    if (!config || get().persistentActionPending) return;
    const draft = get().persistentDraft;
    const pattern = draft.pattern.trim();
    if (!pattern) {
      setStatus("Durable permission rule needs a pattern");
      return;
    }
    if (draft.scope === "project" && !activeWorkspace.trim()) {
      setStatus("Project permission rules need an active workspace");
      return;
    }
    const contextKey = permissionContextKey(config, activeWorkspace.trim());
    const contextRevision = persistentContextRevision;
    set({ persistentActionPending: "add" });
    try {
      await addPersistentPermissionRule(config, {
        scope: draft.scope,
        pattern,
        effect: draft.effect,
        workspaceRoot: draft.scope === "project" ? activeWorkspace.trim() : undefined,
      });
      const contextIsCurrent =
        persistentContextRevision === contextRevision && persistentContextKey === contextKey;
      if (contextIsCurrent || contextRevision === 0) {
        await get().loadPersistentRules(config, activeWorkspace, setStatus);
        if (persistentContextKey === contextKey) {
          set((state) => ({ persistentDraft: { ...state.persistentDraft, pattern: "" } }));
        }
      }
      setStatus("Durable permission rule added");
    } catch (err) {
      setStatus(messageOf(err, "Durable permission rule add failed"));
    } finally {
      set({ persistentActionPending: null });
    }
  },

  async removePersistentRule(config, activeWorkspace, rule, setStatus) {
    if (!config || get().persistentActionPending) return;
    const contextKey = permissionContextKey(config, activeWorkspace.trim());
    const contextRevision = persistentContextRevision;
    set({ persistentActionPending: `${rule.scope}:${rule.workspaceRoot ?? ""}:${rule.index}` });
    try {
      await deletePersistentPermissionRule(config, rule);
      const contextIsCurrent =
        persistentContextRevision === contextRevision && persistentContextKey === contextKey;
      if (contextIsCurrent || contextRevision === 0) {
        await get().loadPersistentRules(config, activeWorkspace, setStatus);
      }
      setStatus("Durable permission rule removed");
    } catch (err) {
      setStatus(messageOf(err, "Durable permission rule remove failed"));
    } finally {
      set({ persistentActionPending: null });
    }
  },

  async exportJson(config, setStatus) {
    if (!config) return;
    set({ jsonPending: "export" });
    try {
      const store = await exportPermissionRules(config);
      set({ jsonDraft: JSON.stringify(store, null, 2) });
      setStatus("Permission rules exported");
    } catch (err) {
      setStatus(messageOf(err, "Permission rule export failed"));
    } finally {
      set({ jsonPending: null });
    }
  },

  async importJson(config, activeWorkspace, setStatus) {
    if (!config) return;
    let parsed: PersistentPermissionRuleStore;
    try {
      parsed = JSON.parse(get().jsonDraft) as unknown as PersistentPermissionRuleStore;
    } catch {
      setStatus("Permission rule import needs valid JSON");
      return;
    }
    if (!isPermissionRuleStore(parsed)) {
      setStatus("Permission rule import needs a user/projects rule store");
      return;
    }
    const contextKey = permissionContextKey(config, activeWorkspace.trim());
    const contextRevision = persistentContextRevision;
    set({ jsonPending: "import" });
    try {
      await importPermissionRules(config, parsed);
      const contextIsCurrent =
        persistentContextRevision === contextRevision && persistentContextKey === contextKey;
      if (contextIsCurrent || contextRevision === 0) {
        await get().loadPersistentRules(config, activeWorkspace, setStatus);
      }
      setStatus("Permission rules imported");
    } catch (err) {
      setStatus(messageOf(err, "Permission rule import failed"));
    } finally {
      set({ jsonPending: null });
    }
  },
}));
