import type { PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type {
  PermissionDenialRow,
  PermissionRuleRow,
  PersistentPermissionRuleRow,
  SessionRow,
} from "@rightkit/agent-ui-core/lib/types";
import type { PermissionRuleDraft, PersistentPermissionRuleDraft } from "../types";

export interface PermissionsTabProps {
  selectedSession: SessionRow | null;
  permissionMode: PermissionMode;
  rules: PermissionRuleRow[];
  draft: PermissionRuleDraft;
  pendingId: string | null;
  persistentDraft: PersistentPermissionRuleDraft;
  persistentPendingId: string | null;
  persistentStorePath: string;
  persistentUserRules: PersistentPermissionRuleRow[];
  persistentProjectRules: PersistentPermissionRuleRow[];
  persistentError: string | null;
  jsonDraft: string;
  jsonPending: string | null;
  denials: PermissionDenialRow[];
  denialStorePath: string;
  denialsLoading: boolean;
  denialsError: string | null;
  onDraft: (draft: PermissionRuleDraft) => void;
  onAdd: () => void;
  onRemove: (rule: PermissionRuleRow) => void;
  onPersistentDraft: (draft: PersistentPermissionRuleDraft) => void;
  onPersistentAdd: () => void;
  onPersistentRemove: (rule: PersistentPermissionRuleRow) => void;
  onJsonDraft: (json: string) => void;
  onJsonExport: () => void;
  onJsonImport: () => void;
  onRefresh: () => void;
  onCycleMode: () => void;
}
