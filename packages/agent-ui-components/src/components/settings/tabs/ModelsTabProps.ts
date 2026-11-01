import type {
  LocalModelCatalog,
  LocalModelChatSmokeResult,
  LocalModelProbeResult,
  LocalProviderRouteSuggestion,
  OrchestrationSettings,
} from "@rightkit/agent-ui-core/lib/types";
import type { SettingsDraft } from "../types";
import type { CatalogGroup } from "@rightkit/agent-ui-core/lib/types/inference";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";

export interface ModelsTabProps {
  catalogGroups?: CatalogGroup[];
  config?: DaemonConfig | null;
  settings: OrchestrationSettings | null;
  settingsDraft: SettingsDraft | null;
  workspaceRoot: string;
  localModelCatalog: LocalModelCatalog | null;
  localModelProbeBaseUrl: string;
  localModelProbeResult: LocalModelProbeResult | null;
  localModelChatSmokeResult: LocalModelChatSmokeResult | null;
  localProviderSuggestion: LocalProviderRouteSuggestion | null;
  localModelActionPending: string | null;
  localModelError: string | null;
  onSettingsDraft: (draft: SettingsDraft) => void;
  onRefreshLocalModels: () => void;
  onLocalModelProbeBaseUrl: (value: string) => void;
  onProbeLocalModelEndpoint: () => void;
  onSmokeLocalModelEndpoint: (model?: string) => void;
  onSuggestLocalProvider: (baseUrl: string, model: string) => void;
  onOpenWorkspace: () => void;
}
