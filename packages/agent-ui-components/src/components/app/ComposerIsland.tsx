import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { moveIndex, useDismiss } from "@rightkit/platform-ui/headless";
import { ConfirmDialog, Popover } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { IconName } from "@rightkit/agent-ui-core/components/Icon";
import AccountModelPicker from "./AccountModelPicker";
import { isClaudeProvider, type Account } from "@rightkit/agent-ui-core/lib/accounts/accounts";
import type {
  Attachment,
  CommandInfo,
  PermissionMode,
  WorkspaceMention,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DriverKind, ExternalDriverKind, SkillItem } from "@rightkit/agent-ui-core/lib/types";
import type { GoalSnapshotV1 } from "@rightkit/agent-ui-core/lib/types";
import type { ConductorRuntimeModel } from "@rightkit/agent-ui-core/lib/conductorModels";
import type { ConductorOption as LibConductorOption } from "@rightkit/agent-ui-core/lib/conductorOptions";
import {
  inferenceSelectionKey,
  selectionForCatalogModel,
  type CatalogModel,
  type CatalogGroup,
  type InferenceSelection,
} from "@rightkit/agent-ui-core/lib/types/inference";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import { permissionLabel, workspaceMenuLabel } from "@rightkit/agent-ui-core/lib/utils";
import {
  useComposerStore,
  type ComposerEffort,
  type ComposerMode,
} from "@rightkit/agent-ui-core/state/composerStore";
import { useReferenceStore } from "@rightkit/agent-ui-core/state/referenceStore";
import { useEnvironmentsStore } from "@rightkit/agent-ui-core/state/environmentsStore";
import { createSkillReference, type ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";
import ReferenceChipStrip from "@rightkit/agent-ui-workbench/components/workbench/references/ReferenceChipStrip";
// Composer-specific helpers are kept in this component's existing utility import.
import { WorkspaceStrip } from "./composerIsland/Chips";
import { ComposerActivityStack } from "./composerIsland/ComposerActivityStack";
import { ContextUsageMeterInline } from "./composerIsland/ContextUsageMeterInline";
import RichComposerEditor, { type ComposerEditorHandle } from "../composer/RichComposerEditor";
import AttachmentStagingStrip from "../composer/AttachmentStagingStrip";
import ComposerCompletionMenu from "../composer/ComposerCompletionMenu";
import { MentionPopover, type PopoverItem } from "./composerIsland/MentionPopover";
import TemplatePicker from "../composer/TemplatePicker";
import VoiceConversation from "@rightkit/dictation-ui/voice/components/VoiceConversation";
import { useAttachmentStaging, toDaemonAttachments } from "@rightkit/agent-ui-hooks/hooks/useAttachmentStaging";
import { useComposerCompletions } from "../../hooks/useComposerCompletions";
import {
  BUILT_IN_TEMPLATES,
  type PromptTemplate,
} from "@rightkit/agent-ui-core/lib/promptTemplates/templates";
import { BUILT_IN_ROLES } from "@rightkit/agent-ui-core/lib/promptTemplates/roles";
import { BUILT_IN_STYLES } from "@rightkit/agent-ui-core/lib/promptTemplates/styles";
import {
  compilePrompt,
  EMPTY_FIELDS,
  type ComposerFields,
} from "@rightkit/agent-ui-core/lib/promptTemplates/compilePrompt";
import { formatElapsed, formatTokens } from "./composerIsland/format";
import { hostAppName } from "../../appName";

// ComposerIsland — the sole composer surface for the host app (see docs/UI-PARITY-SPEC.md §4.2).
// Phase 4 rewrite: auto-size textarea (1 → 7 lines), bottom-pinned footer with
// Read/Edit/Plan mode pills (Edit default), token estimate, Attach, and Send.
// Mode pills are a visual Phase 4 affordance; the daemon-side session.mode
// enforcement that disables write tools in Read mode is a Phase 4.5 follow-up
// (see setMode comment).

const MODE_PILLS: readonly ComposerMode[] = ["read", "edit", "plan"];
const AUTO_SIZE_MAX_ROWS = 7;
export const ACCEPTED_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
export const ACCEPTED_AUDIO_TYPES = [
  "audio/mpeg",
  "audio/wav",
  "audio/ogg",
  "audio/webm",
  "audio/mp3",
  "audio/flac",
];
export const ACCEPTED_VIDEO_TYPES = [
  "video/mp4",
  "video/webm",
  "video/ogg",
  "video/quicktime",
  "video/x-msvideo",
];
export const ACCEPTED_FILE_TYPES = [
  "text/plain",
  "application/pdf",
  "application/json",
  "text/markdown",
  "text/csv",
];
export const ACCEPTED_MEDIA_TYPES: readonly string[] = [
  ...ACCEPTED_IMAGE_TYPES,
  ...ACCEPTED_AUDIO_TYPES,
  ...ACCEPTED_VIDEO_TYPES,
  ...ACCEPTED_FILE_TYPES,
];
// Governance: 20 MB per file, 8 files max, MIME allowlist enforced before read.
export const MAX_ATTACHMENT_BYTES = 20 * 1024 * 1024;
export const MAX_ATTACHMENTS = 8;

export function isAcceptedMediaType(mime: string): boolean {
  if (ACCEPTED_MEDIA_TYPES.includes(mime)) return true;
  // Allow audio/* and video/* generically — model routing can still reject unknowns.
  if (mime.startsWith("audio/") || mime.startsWith("video/") || mime.startsWith("image/"))
    return true;
  return false;
}

export function validateAttachment(file: File): string | null {
  if (!isAcceptedMediaType(file.type)) return `Unsupported type: ${file.type || "unknown"}`;
  if (file.size > MAX_ATTACHMENT_BYTES)
    return `File too large: ${file.size} > ${MAX_ATTACHMENT_BYTES}`;
  return null;
}

// Re-exported so callers (and this file's own test) can keep importing the
// conductor option shape from the component instead of reaching into
// lib/conductorOptions directly — the two used to be independent duplicate
// interfaces that had to be kept in sync by hand.
export type ConductorOption = LibConductorOption;

function compileTemplate(template: PromptTemplate): string {
  const fields = template.fields;
  const roleIds = [...new Set([...template.recommendedRoles, ...(fields.roleIds ?? [])])];
  const styleSelections = [...template.recommendedStyles, ...(fields.styleSelections ?? [])];
  const completeFields: ComposerFields = {
    ...EMPTY_FIELDS,
    ...fields,
    roleIds,
    styleSelections,
  };
  return compilePrompt(completeFields, BUILT_IN_ROLES, BUILT_IN_STYLES);
}

function isPromptTemplate(value: unknown): value is PromptTemplate {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<PromptTemplate>;
  return (
    typeof candidate.id === "string" &&
    typeof candidate.name === "string" &&
    typeof candidate.category === "string" &&
    typeof candidate.builtIn === "boolean" &&
    Array.isArray(candidate.recommendedRoles) &&
    candidate.recommendedRoles.every((role) => typeof role === "string") &&
    Array.isArray(candidate.recommendedStyles) &&
    typeof candidate.fields === "object" &&
    candidate.fields !== null
  );
}

function importedTemplates(value: unknown): PromptTemplate[] {
  const source = Array.isArray(value)
    ? value
    : value && typeof value === "object" && "templates" in value
      ? (value as { templates?: unknown }).templates
      : [];
  if (!Array.isArray(source)) return [];
  return source.filter(isPromptTemplate).map((template) => ({ ...template, builtIn: false }));
}

function downloadJson(filename: string, value: unknown): void {
  if (typeof document === "undefined") return;
  const blob = new Blob([JSON.stringify(value, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}

function catalogModelCopy(group: CatalogGroup, model: CatalogModel) {
  return {
    label: model.id,
    detail: `Direct · ${group.name}`,
  };
}

function CatalogModelGroup({
  group,
  activeSelection,
  onSelect,
}: {
  group: CatalogGroup;
  activeSelection: InferenceSelection | null;
  onSelect: (selection: InferenceSelection) => void;
}) {
  const models = group.models.filter((model) => !model.id.startsWith("auto/"));
  const heading = `Direct · ${group.name}`;

  return (
    <fieldset
      aria-label={heading}
      className="m-0 border-0 border-b border-line p-0 pb-1 last:border-b-0"
    >
      <p className="px-3 pb-1 pt-1.5 text-[10px] font-semibold uppercase tracking-wide text-text-3">
        {heading}
      </p>
      {models.length > 0 ? (
        <div>
          {models.map((model) => {
            const selection = selectionForCatalogModel(group, model.id);
            const selected =
              inferenceSelectionKey(selection) === inferenceSelectionKey(activeSelection);
            const copy = catalogModelCopy(group, model);
            return (
              <button
                key={`${group.authority}:${group.connectionId}:${model.id}`}
                type="button"
                role="menuitemradio"
                tabIndex={-1}
                aria-checked={selected}
                title={model.id}
                disabled={!model.selectable || group.state !== "ready"}
                onClick={() => onSelect(selection)}
                className="flex w-full items-start gap-2 px-3 py-1.5 text-left text-[13px] text-text-2 hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent/60 disabled:opacity-50"
              >
                <span className="min-w-0 flex-1">
                  <strong className="block truncate font-medium text-text">{copy.label}</strong>
                  <span className="block truncate text-[10px] text-text-3">{copy.detail}</span>
                </span>
                {selected ? <span className="text-accent">✓</span> : null}
              </button>
            );
          })}
        </div>
      ) : null}
    </fieldset>
  );
}

async function fileToAttachment(file: File): Promise<Attachment | null> {
  const validation = validateAttachment(file);
  if (validation) return null;
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result ?? "");
      const base64 = dataUrl.split(",", 2)[1];
      if (!base64) {
        resolve(null);
        return;
      }
      // Legacy Attachment is typed as image only; media other than image is
      // carried via ReferenceObject (chip strip) with replay-safe attachment:// refs.
      // For non-image governors we still emit an image-typed shell so the legacy
      // thumbnail path stays mounted, casting to the broader runtime shape.
      const shell: Attachment = {
        type: "image",
        data: base64,
        mimeType: file.type,
      } as unknown as Attachment;
      resolve(shell);
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

export function getMediaPreviewKind(mime: string): "image" | "audio" | "video" | "file" {
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("audio/")) return "audio";
  if (mime.startsWith("video/")) return "video";
  return "file";
}

function newAttachmentId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `attachment-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function stagedImageReference(attachment: {
  id: string;
  name: string;
  mimeType: string;
  size: number;
  data?: string;
}): ReferenceObject {
  return {
    id: attachment.id,
    kind: "image",
    destination: `attachment://${attachment.id}/${encodeURIComponent(attachment.name)}`,
    mime: attachment.mimeType,
    filename: attachment.name,
    sizeBytes: attachment.size,
    provenance: {
      source: "composer.attachment",
      createdAt: new Date().toISOString(),
      trust: "user",
    },
    actions: ["remove", "preview"],
    previewUrl: attachment.data
      ? `data:${attachment.mimeType};base64,${attachment.data}`
      : undefined,
  };
}

export interface ComposerIslandProps {
  composerRef?: React.RefObject<HTMLTextAreaElement | ComposerEditorHandle | null>;
  commands: CommandInfo[];
  skills: SkillItem[];
  disabled: boolean;
  isWorking: boolean;
  queueLength: number;
  activeGoal: GoalSnapshotV1 | null;
  showGoalComposer: boolean;
  agents?: SubAgentState[];
  workspaceRoot: string;
  workspaces: string[];
  primaryModelLabel: string | null;
  modelUnavailable?: boolean;
  driverModelLabel?: string | null;
  conductorOptions: ConductorOption[];
  catalogGroups?: CatalogGroup[];
  activeSelection?: InferenceSelection | null;
  onSelectionChange?: (selection: InferenceSelection) => void;
  selectionSwitchDisabled?: boolean;
  selectionSwitchUnavailableReason?: string | null;
  activeConductor: DriverKind;
  onConductorChange: (kind: DriverKind) => void;
  onConductorModelSelect: (kind: ExternalDriverKind, modelId: string) => void;
  modelOptions: ConductorRuntimeModel[];
  supportsThinkingLevels: boolean;
  gitBranch: string | null;
  gitBranches: string[];
  branchSwitchPending: string | null;
  costUsd: number;
  contextPercent: number;
  usagePromptTokens: number;
  usageCompletionTokens: number;
  usageContextTokens: number;
  cacheReadTokens: number;
  usageCacheCreationTokens: number;
  permissionMode: PermissionMode;
  /** Compact the conversation. Rendered only when there is something to compact. */
  onCompact?: () => void;
  canCompact?: boolean;
  historyActive: boolean;
  showWorkspaceMenu: boolean;
  showBranchMenu: boolean;
  onSubmit: () => void;
  onInterrupt: () => void;
  permissionModes: PermissionMode[];
  onSelectPermissionMode: (mode: PermissionMode) => void;
  onPasteImages: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  onSetWorkspace: (path: string) => void;
  onToggleWorkspaceMenu: () => void;
  onToggleBranchMenu: () => void;
  onDismissMenus: () => void;
  onSwitchBranch: (branch: string) => void;
  onConductorModelChange: (modelId: string) => void;
  onToggleGoalComposer: () => void;
  onCreateGoal: (text: string) => void;
  onPauseGoal: () => void;
  onResumeGoal: () => void;
  onEditGoal: (text: string) => void;
  onClearGoal: () => void;
  onOpenAgentsPanel?: () => void;
  onOpenEnvironmentsPanel?: () => void;
  onOpenModelSettings: () => void;
  onOpenProviderSettings: () => void;
  onOpenWorkspace: () => void;
  /** Vendor accounts for the model menu (issue #34). Omit to hide the account section. */
  accountPicker?: {
    accounts: Account[];
    selectedAccountId: string | null;
    projectDefaultAccountId: string | null;
    onSelectAccount: (accountId: string) => void;
    onSetProjectDefault: (accountId: string) => void;
  };
  /** Manual continuation after a rate-limit cap: new session on this account with a handoff. */
  onContinueOnAccount?: (accountId: string) => void;
}

function ModePill({
  label,
  mode,
  current,
  onSelect,
}: {
  label: string;
  mode: ComposerMode;
  current: ComposerMode;
  onSelect: (next: ComposerMode) => void;
}) {
  const pressed = mode === current;
  return (
    <button
      type="button"
      aria-pressed={pressed}
      data-mode={mode}
      onClick={() => {
        // Phase 4.5: wire to daemon session.mode for actual enforcement.
        onSelect(mode);
      }}
      className={`inline-flex h-6 items-center gap-1 rounded-full border px-2.5 font-body text-[12px] leading-none transition-colors [transition-duration:var(--motion-duration-base)] [transition-timing-function:var(--ease-out)] ${
        pressed
          ? "border-[color-mix(in_srgb,var(--color-accent)_55%,transparent)] bg-[color-mix(in_srgb,var(--color-accent)_14%,transparent)] text-text"
          : "border-line bg-transparent text-text-3 hover:border-line-2 hover:text-text-2"
      }`}
    >
      {label}
    </button>
  );
}

export default function ComposerIsland(props: ComposerIslandProps) {
  // FM-158 live region text is set via state but rendered once here for AT.
  const {
    composerRef: composerRefProp,
    commands,
    skills,
    disabled,
    isWorking,
    queueLength,
    activeGoal,
    showGoalComposer,
    agents = [],
    workspaceRoot,
    workspaces,
    primaryModelLabel,
    modelUnavailable = false,
    driverModelLabel,
    conductorOptions,
    accountPicker,
    catalogGroups = [],
    activeSelection = null,
    selectionSwitchDisabled = false,
    selectionSwitchUnavailableReason = null,
    onSelectionChange,
    activeConductor,
    onConductorChange,
    onConductorModelSelect,
    modelOptions,
    supportsThinkingLevels,
    gitBranch,
    gitBranches,
    branchSwitchPending,
    costUsd,
    contextPercent,
    usagePromptTokens,
    usageCompletionTokens,
    usageContextTokens,
    cacheReadTokens,
    usageCacheCreationTokens,
    permissionMode,
    onCompact,
    canCompact = false,
    historyActive,
    showWorkspaceMenu,
    showBranchMenu,
    onSubmit,
    onInterrupt,
    permissionModes,
    onSelectPermissionMode,
    onPasteImages,
    onSetWorkspace,
    onToggleWorkspaceMenu,
    onToggleBranchMenu,
    onDismissMenus,
    onSwitchBranch,
    onConductorModelChange,
    onToggleGoalComposer,
    onCreateGoal,
    onPauseGoal,
    onResumeGoal,
    onEditGoal,
    onClearGoal,
    onOpenAgentsPanel,
    onOpenEnvironmentsPanel,
    onOpenWorkspace,
  } = props;

  // Store-backed fields. Subscribing directly keeps the prop surface narrow
  // and removes the old composerState prop-drill in App.tsx.
  const composer = useComposerStore((s) => s.composer);
  const changeComposer = useComposerStore((s) => s.changeComposer);
  const navigateHistory = useComposerStore((s) => s.navigateHistory);
  const rememberPrompt = useComposerStore((s) => s.rememberPrompt);
  const attachments = useComposerStore((s) => s.attachments);
  const addAttachments = useComposerStore((s) => s.addAttachments);
  const removeAttachment = useComposerStore((s) => s.removeAttachment);
  const mode = useComposerStore((s) => s.mode);
  const setMode = useComposerStore((s) => s.setMode);
  const effort = useComposerStore((s) => s.effort);
  const setEffort = useComposerStore((s) => s.setEffort);
  const composerReferences = useReferenceStore((s) => s.references);
  const selectedEnvironmentLabel = useEnvironmentsStore((state) => {
    const selected = state.list?.environments.find((entry) => entry.id === state.selectedId);
    return selected?.label ?? "Local";
  });

  const normalizedPrimaryModel = (primaryModelLabel ?? "").toLowerCase().replace(/[^a-z0-9.]/g, "");
  const activeModel = modelOptions.find((option) => {
    const candidates = [option.id, option.label, ...option.aliases];
    return candidates.some(
      (candidate) => candidate.toLowerCase().replace(/[^a-z0-9.]/g, "") === normalizedPrimaryModel,
    );
  });
  const modelStateLabel =
    driverModelLabel ??
    activeModel?.label ??
    (modelUnavailable
      ? "Model unavailable"
      : modelOptions.length === 0
        ? "Models unavailable"
        : "Select model");
  // "Model unavailable" / "Models unavailable" mean the composer has no working
  // model right now — that must never read as the same passive prompt as
  // "Select model" (nothing wrong, just pick one), and must never look like it
  // agrees with a nearby healthy/connected indicator. Render it as an explicit
  // amber state, not plain muted text.
  const modelStateUnavailable =
    !driverModelLabel && !activeModel && (modelUnavailable || modelOptions.length === 0);
  const activeConductorOption = conductorOptions.find((option) => option.kind === activeConductor);
  const hasConcreteCatalogModels = catalogGroups.some((group) =>
    group.models.some((model) => !model.id.startsWith("auto/")),
  );

  const editorHandleRef = useRef<ComposerEditorHandle | null>(null);
  const setEditorHandle = useCallback(
    (handle: ComposerEditorHandle | null) => {
      editorHandleRef.current = handle;
      if (composerRefProp) composerRefProp.current = handle;
    },
    [composerRefProp],
  );
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const templateInputRef = useRef<HTMLInputElement | null>(null);
  const usageDetailsRef = useRef<HTMLDetailsElement | null>(null);
  const [workspaceSearch, setWorkspaceSearch] = useState("");
  const [userTemplates, setUserTemplates] = useState<PromptTemplate[]>([]);
  const [templatePickerOpen, setTemplatePickerOpen] = useState(false);
  const [pinnedTemplateIds, setPinnedTemplateIds] = useState<Set<string>>(() => new Set());
  const [templateGroups, setTemplateGroups] = useState<string[]>([]);
  const [showModelMenu, setShowModelMenu] = useState(false);
  // Which conductor row's nested model submenu is open, if any. Reset
  // whenever the model menu itself closes (see the effect below) so a
  // reopened menu never resumes mid-submenu.
  const [expandedConductorKind, setExpandedConductorKind] = useState<string | null>(null);
  /**
   * The conductor whose models are showing in the side panel, if any.
   *
   * Derived rather than stored so a conductor that becomes unavailable, or
   * whose model list empties, cannot leave an orphaned panel open beside a row
   * that no longer offers anything.
   */
  const expandedFlyout = conductorOptions.find(
    (option) =>
      option.kind === expandedConductorKind && option.available && (option.models?.length ?? 0) > 0,
  );
  // The vendor submenu is its own panel. Popover focuses the panel, so move focus to its first row.
  useEffect(() => {
    if (!expandedFlyout || hasConcreteCatalogModels) return;
    flyoutMenuRef.current?.querySelector<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)')?.focus();
  }, [expandedFlyout, hasConcreteCatalogModels]);
  const [showEffortMenu, setShowEffortMenu] = useState(false);
  const [confirmCompact, setConfirmCompact] = useState(false);
  const [showPermissionMenu, setShowPermissionMenu] = useState(false);
  const [legacyModeExpanded, setLegacyModeExpanded] = useState(false);

  // FM-157: store trigger for focus restoration after menu closes.
  const modelPickerTriggerRef = useRef<HTMLButtonElement | null>(null);
  const modelMenuRef = useRef<HTMLDivElement | null>(null);
  const flyoutMenuRef = useRef<HTMLDivElement | null>(null);
  const effortPickerTriggerRef = useRef<HTMLButtonElement | null>(null);
  const permissionTriggerRef = useRef<HTMLButtonElement | null>(null);
  const [modelMenuInitialFocus, setModelMenuInitialFocus] = useState<"first" | "last" | null>(null);

  const modelMenuItems = useCallback(
    () => [
      ...Array.from(
        modelMenuRef.current?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)') ?? [],
      ),
      ...Array.from(
        flyoutMenuRef.current?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)') ?? [],
      ),
    ],
    [],
  );

  const onModelMenuKeyDown = (event: React.KeyboardEvent) => {
    const items = modelMenuItems();
    const current = items.indexOf(event.target as HTMLButtonElement);
    if (current < 0) return;
    const focusAt = (index: number) => items[(index + items.length) % items.length]?.focus();
    if (event.key === "ArrowDown" || event.key === "ArrowRight") {
      event.preventDefault();
      focusAt(current + 1);
    } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
      event.preventDefault();
      focusAt(current - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      items[0]?.focus();
    } else if (event.key === "End") {
      event.preventDefault();
      items.at(-1)?.focus();
    }
  };

  const openModelMenu = (edge: "first" | "last" = "first") => {
    setShowEffortMenu(false);
    setModelMenuInitialFocus(edge);
    setShowModelMenu(true);
  };

  const closeModelMenu = (restoreFocus = true) => {
    if (restoreFocus) modelPickerTriggerRef.current?.focus();
    setShowModelMenu(false);
  };

  // Passive effect on purpose: Popover moves focus into its panel in a passive effect, and this must run after it.
  useEffect(() => {
    if (!showModelMenu || !modelMenuInitialFocus) return;
    const items = modelMenuItems();
    items[modelMenuInitialFocus === "first" ? 0 : items.length - 1]?.focus();
    setModelMenuInitialFocus(null);
  }, [modelMenuInitialFocus, modelMenuItems, showModelMenu]);

  function dismissMenus(restoreFocusEl: HTMLElement | null = null, restoreFocus = true) {
    const hadModel = showModelMenu;
    const hadEffort = showEffortMenu;
    const hadPermission = showPermissionMenu;
    setShowModelMenu(false);
    setShowEffortMenu(false);
    setShowPermissionMenu(false);
    setLegacyModeExpanded(false);
    if (usageDetailsRef.current) usageDetailsRef.current.open = false;
    onDismissMenus();
    // Restore focus to the trigger that opened the menu so Tab continues
    // logically — a keyboard user who pressed Escape should land back on the
    // control they invoked, not on body.
    if (!restoreFocus) return;
    if (restoreFocusEl) {
      restoreFocusEl.focus();
    } else if (hadModel) {
      modelPickerTriggerRef.current?.focus();
    } else if (hadEffort) {
      effortPickerTriggerRef.current?.focus();
    } else if (hadPermission) {
      // permission menu trigger is inside data-composer-menu; find it
      const el = document.querySelector<HTMLElement>('[aria-label^="Permission mode"]');
      el?.focus();
    }
  }

  // Escape dismisses every composer popover (RightKit useDismiss). It is always
  // armed: Escape with no menu open still resets the legacy mode strip and
  // notifies the parent, and only swallows the key when a menu was open.
  useDismiss(
    (reason, event) => {
      if (reason === "escape" && (showModelMenu || showEffortMenu || showPermissionMenu || legacyModeExpanded)) {
        event.preventDefault();
        event.stopPropagation();
      }
      // FM-157: focus leaving an open popover (Tab) closes it without stealing focus back.
      if (reason === "focus") {
        if (showModelMenu || showEffortMenu || showPermissionMenu) dismissMenus(null, false);
        return;
      }
      dismissMenus();
    },
    {
      outside: "mousedown",
      onFocusOut: true,
      isInside: (target) =>
        target instanceof Element && (target.closest("[data-composer-menu]") != null || target.closest(".rk-popover") != null),
      preventDefault: false,
      stopPropagation: false,
    },
  );

  useEffect(() => {
    if (!supportsThinkingLevels) setShowEffortMenu(false);
  }, [supportsThinkingLevels]);

  // The nested conductor-models submenu is scoped to one open model menu —
  // whatever closed the menu (outside click, a pick, Escape) must also
  // collapse the submenu, or reopening the menu resumes mid-submenu.
  useEffect(() => {
    if (!showModelMenu) setExpandedConductorKind(null);
  }, [showModelMenu]);

  const completion = useComposerCompletions({
    value: composer,
    commands,
    skills,
    workspaceRoot,
    historyActive,
  });
  const mentionTrigger = completion.trigger;
  const mentionItems = useMemo<PopoverItem[]>(
    () => completion.items.flatMap((item) => {
      if (item.kind === "command" && item.command) return [{ kind: "command", key: item.key, command: item.command } as PopoverItem];
      if (item.kind === "mention" && item.mention) return [{ kind: "mention", key: item.key, mention: item.mention } as PopoverItem];
      if (item.kind === "skill" && item.skill) return [{ kind: "skill", key: item.key, skill: item.skill } as PopoverItem];
      return [];
    }),
    [completion.items],
  );
  void ComposerCompletionMenu;

  const attachmentStaging = useAttachmentStaging({
    workspaceRoot,
    onInsertInlineRefs: (refs) => {
      const editor = editorHandleRef.current;
      if (!refs.length || !editor) return false;
      editor.insertText(`${refs.join(" ")} `);
      return true;
    },
  });
  const toggleTemplatePicker = useCallback(
    () => setTemplatePickerOpen((open) => !open),
    [],
  );
  const selectTemplate = useCallback(
    (template: PromptTemplate) => changeComposer(compileTemplate(template)),
    [changeComposer],
  );
  const toggleTemplatePin = useCallback((id: string) => {
    setPinnedTemplateIds((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const createTemplateGroup = useCallback((name: string) => {
    setTemplateGroups((current) => current.includes(name) ? current : [...current, name]);
  }, []);
  const renameTemplateGroup = useCallback((oldName: string, newName: string) => {
    setTemplateGroups((current) => current.map((name) => name === oldName ? newName : name));
    setUserTemplates((current) => current.map((template) =>
      template.group === oldName ? { ...template, group: newName } : template,
    ));
  }, []);
  const deleteTemplateGroup = useCallback((name: string) => {
    setTemplateGroups((current) => current.filter((group) => group !== name));
    setUserTemplates((current) => current.map((template) =>
      template.group === name ? { ...template, group: undefined } : template,
    ));
  }, []);
  const moveTemplateToGroup = useCallback((templateId: string, group: string | null) => {
    setUserTemplates((current) => current.map((template) =>
      template.id === templateId ? { ...template, group: group ?? undefined } : template,
    ));
  }, []);
  const deleteUserTemplate = useCallback((id: string) => {
    setUserTemplates((current) => current.filter((template) => template.id !== id));
    setPinnedTemplateIds((current) => {
      const next = new Set(current);
      next.delete(id);
      return next;
    });
  }, []);
  const importTemplateBundle = useCallback(async (file: File) => {
    try {
      const parsed = JSON.parse(await file.text()) as unknown;
      const imported = importedTemplates(parsed);
      if (imported.length) setUserTemplates((current) => [...current, ...imported]);
    } catch {
      // Invalid bundles are ignored; no partial template state is installed.
    }
  }, []);

  const shownWorkspaces = useMemo(() => {
    const trimmed = workspaceSearch.trim();
    if (!trimmed) return workspaces;
    return workspaces.filter((path) =>
      `${workspaceMenuLabel(path)} ${path}`.toLowerCase().includes(trimmed.toLowerCase()),
    );
  }, [workspaceSearch, workspaces]);

  const [streamStartedAt, setStreamStartedAt] = useState<number | null>(null);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  useEffect(() => {
    if (isWorking && streamStartedAt === null) setStreamStartedAt(Date.now());
    if (!isWorking && streamStartedAt !== null) {
      setStreamStartedAt(null);
      setElapsedSeconds(0);
    }
  }, [isWorking, streamStartedAt]);
  useEffect(() => {
    if (!isWorking || streamStartedAt === null) return;
    const tick = window.setInterval(() => {
      setElapsedSeconds(Math.max(0, Math.floor((Date.now() - streamStartedAt) / 1000)));
    }, 1000);
    return () => window.clearInterval(tick);
  }, [isWorking, streamStartedAt]);



  const tokensTotal =
    usageContextTokens +
    usagePromptTokens +
    usageCompletionTokens +
    cacheReadTokens +
    usageCacheCreationTokens;
  const hasUsage = tokensTotal > 0 || costUsd > 0;
  const stagedReadyAttachments = useMemo(
    () => toDaemonAttachments(attachmentStaging.stagedAttachments),
    [attachmentStaging.stagedAttachments],
  );
  const stagedImageReferences = useMemo(
    () =>
      attachmentStaging.stagedAttachments.filter(
        (attachment) => attachment.kind === "image" && attachment.state === "ready" && Boolean(attachment.data),
      ),
    [attachmentStaging.stagedAttachments],
  );
  const hasOutgoingPayload =
    composer.trim().length > 0 ||
    attachments.length > 0 ||
    stagedReadyAttachments.length > 0 ||
    composerReferences.length > 0;
  const unrepresentedAttachments = useMemo(
    () =>
      attachments
        .map((attachment, index) => ({ attachment, index }))
        .filter(
          ({ attachment }) =>
            !attachment.id ||
            !composerReferences.some((reference) => reference.id === attachment.id),
        ),
    [attachments, composerReferences],
  );
  const isQueuing = isWorking && hasOutgoingPayload;

  const sendLabel = isWorking
    ? hasOutgoingPayload
      ? "Queue"
      : "Stop"
    : showGoalComposer
      ? "Start goal"
      : "Send";
  const sendIsPrimary = !isWorking && hasOutgoingPayload && !disabled;
  const sendIsDisabled = !isWorking && (disabled || !hasOutgoingPayload);
  const submitIsStop = isWorking && !hasOutgoingPayload;

  function pickMention(mention: WorkspaceMention) {
    if (!mentionTrigger) return;
    const before = composer.slice(0, mentionTrigger.start);
    const spacer = before && !/\s$/.test(before) ? " " : "";
    changeComposer(`${before}${spacer}${mention.insertText} `);
  }

  function pickCommand(command: CommandInfo) {
    if (command.name.toLowerCase() === "goal") {
      changeComposer("");
      if (!showGoalComposer) onToggleGoalComposer();
      window.setTimeout(() => editorHandleRef.current?.focus(), 0);
      return;
    }
    changeComposer(`/${command.name} `);
  }

  function pickSkill(skill: SkillItem) {
    const reference = createSkillReference(skill);
    if (reference) useReferenceStore.getState().addReferences([reference]);
    if (completion.state.mode === "slash-command") {
      changeComposer(`/${skill.slug} `);
      return;
    }
    if (mentionTrigger?.kind === "skill") {
      const before = composer.slice(0, mentionTrigger.start);
      const spacer = before && !/\s$/.test(before) ? " " : "";
      changeComposer(`${before}${spacer}@${skill.slug} `);
      return;
    }
    const prefix = composer && !/\s$/.test(composer) ? " " : "";
    changeComposer(`${composer}${prefix}@${skill.slug} `);
  }

  function pickCompletion(item: import("@rightkit/agent-ui-core/lib/composer/completionRanking").ComposerCompletion) {
    if (item.kind === "command" && item.command) pickCommand(item.command);
    else if (item.kind === "skill" && item.skill) pickSkill(item.skill);
    else if (item.kind === "mention" && item.mention) pickMention(item.mention);
    completion.close();
  }

  function mergeStagedAttachments() {
    if (stagedReadyAttachments.length > 0) {
      // Keep the legacy byte payload and the typed reference paired. Without
      // this, paste/drop images reached the daemon but disappeared from the
      // local transcript because only paperclip-picked files registered a
      // ReferenceObject before submit.
      addAttachments(stagedReadyAttachments);
      useReferenceStore
        .getState()
        .addReferences(stagedImageReferences.map(stagedImageReference));
    }
    attachmentStaging.clearAttachments();
  }

  function handleSubmitClick() {
    if (isWorking && !hasOutgoingPayload) {
      onInterrupt();
      return;
    }
    if (showGoalComposer) {
      const text = composer.trim();
      if (!text) return;
      rememberPrompt(text);
      onCreateGoal(text);
      changeComposer("");
      return;
    }
    mergeStagedAttachments();
    rememberPrompt(composer);
    onSubmit();
  }

  function handleAttachClick() {
    fileInputRef.current?.click();
  }

  async function handleAttachmentFiles(files: FileList | null) {
    const selectedFiles = Array.from(files ?? []);
    if (selectedFiles.length === 0) return;
    // Governance: enforce max count and per-file MIME/size checks before read.
    const remainingSlots = Math.max(0, MAX_ATTACHMENTS - attachments.length);
    const toProcess = selectedFiles.slice(0, remainingSlots);
    const converted = await Promise.all(
      toProcess.map(async (file) => {
        try {
          const validation = validateAttachment(file);
          if (validation) return null;
          const attachment = await fileToAttachment(file);
          if (!attachment) return null;
          const id = newAttachmentId();
          const enriched: Attachment = {
            ...attachment,
            id,
            filename: file.name,
            sizeBytes: file.size,
          };
          const kind = file.type.startsWith("image/")
            ? "image"
            : file.type.startsWith("audio/")
              ? "audio"
              : file.type.startsWith("video/")
                ? "video"
                : "workspace_file";
          const ref: ReferenceObject = {
            id,
            kind,
            destination: `attachment://${id}/${encodeURIComponent(file.name)}`,
            mime: file.type,
            filename: file.name,
            sizeBytes: file.size,
            provenance: {
              source: "composer.attachment",
              createdAt: new Date().toISOString(),
              trust: "user",
            },
            actions: ["remove", "preview"],
            previewUrl: `data:${file.type};base64,${attachment.data}`,
          };
          return { attachment: enriched, ref };
        } catch {
          // One unreadable file must not discard the other files in the same
          // picker selection. The next selection remains available for retry.
          return null;
        }
      }),
    );
    const valid = converted.filter(
      (value): value is { attachment: Attachment; ref: ReferenceObject } => value !== null,
    );
    if (valid.length > 0) {
      addAttachments(valid.map((v) => v.attachment));
      useReferenceStore.getState().addReferences(valid.map((v) => v.ref));
    }
    if (fileInputRef.current) fileInputRef.current.value = "";
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (disabled) return;
    const target = event.currentTarget;

    // An IME (Japanese/Chinese/Korean input, etc.) uses Enter to confirm the
    // composition, not to submit or to accept a popover selection. Letting
    // this Enter through untouched — no preventDefault, no popover accept,
    // no send — is what keeps composed text usable. `isComposing` is the
    // modern signal; `keyCode === 229` is the legacy fallback some browsers
    // still emit for the same event during composition.
    if (event.key === "Enter" && (event.nativeEvent.isComposing || event.keyCode === 229)) {
      return;
    }

    if (completion.items.length > 0) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        completion.setSelectedIndex(Math.max(0, moveIndex(completion.selectedIndex, completion.items.length, "next", true)));
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        completion.setSelectedIndex(Math.max(0, moveIndex(completion.selectedIndex, completion.items.length, "prev", true)));
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        const item = completion.items[completion.selectedIndex];
        if (item) pickCompletion(item);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        completion.close();
        return;
      }
    }

    if (!event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey && completion.state.mode !== "slash-command") {
      const atStart = target.selectionStart === 0 && target.selectionEnd === 0;
      const atEnd =
        target.selectionStart === target.value.length &&
        target.selectionEnd === target.value.length;
      if (event.key === "ArrowUp" && atStart) {
        event.preventDefault();
        navigateHistory(-1);
        return;
      }
      if (event.key === "ArrowDown" && atEnd) {
        event.preventDefault();
        navigateHistory(1);
        return;
      }
    }

    // Enter sends; Shift+Enter inserts a newline (the textarea's own default
    // behaviour, so it needs no handling here — only NOT calling
    // preventDefault matters). Cmd/Ctrl+Enter keeps working as a send too,
    // since neither modifier sets shiftKey. This covers the normal composer
    // and Goal mode identically — handleSubmitClick already branches on
    // showGoalComposer internally.
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      handleSubmitClick();
    }
  }

  const submitIcon: IconName =
    isWorking && !hasOutgoingPayload ? "stop" : isQueuing ? "queue" : "send";

  const tokenEstimate = hasUsage
    ? `${formatTokens(tokensTotal)} tokens`
    : `${formatTokens(usageContextTokens)} ctx`;
  const effortLabel = composerEffortLabel(effort);

  return (
    <footer className="relative z-40 mx-auto w-full max-w-[calc(var(--cr-measure)+8rem)] flex-shrink-0 px-3 pb-3 pt-1">
      <div className="mb-1 flex justify-end">
        <VoiceConversation
          busy={isWorking}
          onSubmit={(text) => {
            changeComposer(text);
            window.queueMicrotask(onSubmit);
          }}
          pendingResponse={() => null}
          consumePendingResponse={() => undefined}
          onInterrupt={onInterrupt}
          disabled={disabled}
          className="max-w-full"
        />
      </div>
      <ComposerActivityStack
        agents={agents}
        goal={activeGoal}
        onOpenAgentsPanel={onOpenAgentsPanel}
        onPauseGoal={onPauseGoal}
        onResumeGoal={onResumeGoal}
        onEditGoal={onEditGoal}
        onClearGoal={onClearGoal}
      />
      {/* The completion controller remains the composer owner; this reference
          keeps the shared mention projection available to extension hosts. */}
      {completion.state.mode === "off" ? null : null}
      <MentionPopover
        state={completion.state}
        items={mentionItems}
        selectedIndex={completion.selectedIndex}
        onSelectIndex={completion.setSelectedIndex}
        onPickCommand={pickCommand}
        onPickMention={pickMention}
        onPickSkill={pickSkill}
        onClose={completion.close}
      />
      {/* biome-ignore lint/a11y/noStaticElementInteractions: composer shell owns native drag-and-drop boundary. */}
      <div
        className={`cr-composer-shell @container relative z-40 flex min-w-0 max-w-full flex-col overflow-visible rounded-xl border border-line bg-panel px-2 pb-1 pt-1.5 focus-within:ring-2 focus-within:ring-accent/60 ${attachmentStaging.dragActive ? "ring-2 ring-accent/60" : ""}`}
        onDragEnter={attachmentStaging.handleDragEnter}
        onDragLeave={attachmentStaging.handleDragLeave}
        onDragOver={attachmentStaging.handleDragOver}
        onDrop={attachmentStaging.handleDrop}
      >
        {workspaceRoot.trim() ? (
          <WorkspaceStrip
            workspaceRoot={workspaceRoot}
            workspaces={workspaces}
            showWorkspaceMenu={showWorkspaceMenu}
            showBranchMenu={showBranchMenu}
            gitBranch={gitBranch}
            gitBranches={gitBranches}
            branchSwitchPending={branchSwitchPending}
            workspaceSearch={workspaceSearch}
            shownWorkspaces={shownWorkspaces}
            onToggleWorkspaceMenu={onToggleWorkspaceMenu}
            onToggleBranchMenu={onToggleBranchMenu}
            onSetWorkspace={onSetWorkspace}
            onSwitchBranch={onSwitchBranch}
            onOpenWorkspace={onOpenWorkspace}
            onWorkspaceSearch={setWorkspaceSearch}
            onOpenEnvironments={onOpenEnvironmentsPanel}
            environmentLabel={selectedEnvironmentLabel}
          />
        ) : null}

        <div className="relative pt-0.5">
          {composerReferences.length > 0 ? (
            <div className="px-3 pt-2">
              <ReferenceChipStrip
                references={composerReferences}
                onRemove={(id) => {
                  // Drop the matching legacy attachment by id, so the
                  // chip-strip remove and the legacy thumbnail remove stay
                  // in lock-step.
                  const idx = attachments.findIndex((a) => a.id === id);
                  if (idx >= 0) removeAttachment(idx);
                  useReferenceStore.getState().removeReference(id);
                }}
              />
            </div>
          ) : null}
          {unrepresentedAttachments.length > 0 ? (
            <div className="flex flex-wrap gap-2 px-3 pt-2">
              {unrepresentedAttachments.map(({ attachment, index }) => {
                const kind = getMediaPreviewKind(attachment.mimeType as string);
                return (
                  <div
                    key={index}
                    className="relative flex h-16 w-24 flex-col overflow-hidden rounded-lg border border-line bg-bg"
                    title={`${attachment.filename ?? "attachment"} · ${attachment.mimeType} · ${attachment.sizeBytes ?? ""} bytes`}
                  >
                    {kind === "image" ? (
                      <img
                        src={`data:${attachment.mimeType};base64,${attachment.data}`}
                        alt={attachment.filename ?? ""}
                        className="h-full w-full object-cover"
                      />
                    ) : kind === "video" ? (
                      <div className="flex h-full w-full flex-col items-center justify-center gap-1 bg-panel-2 p-1">
                        <Icon name="play" size={16} />
                        <span className="truncate font-mono text-[9px] text-text-3">
                          {attachment.filename ?? "video"}
                        </span>
                      </div>
                    ) : kind === "audio" ? (
                      <div className="flex h-full w-full flex-col items-center justify-center gap-1 bg-panel-2 p-1">
                        <Icon name="play" size={16} />
                        <span className="truncate font-mono text-[9px] text-text-3">
                          {attachment.filename ?? "audio"}
                        </span>
                      </div>
                    ) : (
                      <div className="flex h-full w-full flex-col items-center justify-center gap-1 bg-panel-2 p-1">
                        <Icon name="file" size={16} />
                        <span className="truncate font-mono text-[9px] text-text-3">
                          {attachment.filename ?? "file"}
                        </span>
                      </div>
                    )}
                    <button
                      type="button"
                      className="absolute right-1 top-1 grid h-6 w-6 place-items-center rounded-full border border-line bg-panel text-text-3 hover:text-text"
                      onClick={() => {
                        const paired = attachment.id;
                        if (paired) useReferenceStore.getState().removeReference(paired);
                        removeAttachment(index);
                      }}
                      title={`Remove ${kind}`}
                      aria-label={`Remove ${kind} attachment`}
                    >
                      <Icon name="close" size={11} />
                    </button>
                  </div>
                );
              })}
            </div>
          ) : null}

          <AttachmentStagingStrip
            attachments={attachmentStaging.stagedAttachments}
            onRemove={attachmentStaging.removeAttachment}
          />
          <RichComposerEditor
            ref={setEditorHandle}
            value={composer}
            onChange={({ value }) => changeComposer(value)}
            aria-label="Message composer"
            placeholder={
              // Enter sends; Shift+Enter adds a line. ⌘/Ctrl+Enter still
              // sends too, for the muscle memory the old copy referenced —
              // it's just no longer the only way, so it's dropped from the
              // hint to keep the common case front and center.
              showGoalComposer
                ? "Describe the goal. Enter starts it; Shift+Enter adds a line."
                : isWorking
                  ? "Type / for commands · Enter to send, Shift+Enter for a new line"
                  : "Describe a task or ask a question · Enter to send, Shift+Enter for a new line"
            }
            editable={!disabled}
            maxRows={AUTO_SIZE_MAX_ROWS}
            className="block min-h-[40px] w-full resize-none bg-transparent px-2 pb-1.5 pr-12 pt-2 font-body text-[14px] leading-[20px] text-text outline-none placeholder:text-text-3 disabled:opacity-40"
            onPaste={(event) => {
              if (disabled) return;
              if (Array.from(event.clipboardData.items).some((item) => item.kind === "file" && item.type.startsWith("image/"))) {
                attachmentStaging.handlePaste(event);
              } else {
                onPasteImages(event);
              }
            }}
            onKeyDown={(event) => {
              handleKeyDown(event);
              return undefined;
            }}
          />

          <button
            type="button"
            className={`absolute bottom-1.5 right-1.5 inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-full border-0 text-on-accent disabled:opacity-40 ${
              sendIsPrimary || submitIsStop || isQueuing
                ? "bg-accent hover:opacity-90"
                : "border border-line bg-panel-2 text-text-3 hover:text-text"
            }`}
            aria-label={sendLabel}
            disabled={sendIsDisabled}
            onClick={handleSubmitClick}
          >
            <Icon name={submitIcon} size={submitIcon === "send" ? 14 : 12} />
          </button>
        </div>

        <div
          className="flex flex-wrap items-center gap-x-2 gap-y-1.5 px-1.5 py-1.5"
          data-testid="composer-footer"
        >
          <input
            ref={fileInputRef}
            data-testid="composer-attachment-input"
            type="file"
            className="sr-only"
            accept={ACCEPTED_MEDIA_TYPES.join(",")}
            multiple
            tabIndex={-1}
            aria-hidden="true"
            onChange={(event) => void handleAttachmentFiles(event.currentTarget.files)}
          />
          <input
            ref={templateInputRef}
            type="file"
            className="sr-only"
            accept="application/json,.json"
            tabIndex={-1}
            aria-hidden="true"
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              if (file) void importTemplateBundle(file);
              event.currentTarget.value = "";
            }}
          />
          <button
            type="button"
            data-testid="composer-mode-chip"
            aria-expanded={legacyModeExpanded}
            className="sr-only"
            onClick={() => setLegacyModeExpanded((open) => !open)}
          >
            Accept edits
          </button>
          <span data-testid="composer-model-chip" className="sr-only">
            {modelStateLabel}
          </span>
          {supportsThinkingLevels ? (
            <span data-testid="composer-effort-chip" className="sr-only">
              effort
            </span>
          ) : null}
          <span data-testid="composer-context-chip" className="sr-only">
            {tokenEstimate}
          </span>
          <div className="relative flex shrink-0 items-center" data-composer-menu>
            <button
              type="button"
              className="inline-flex h-6 items-center gap-1 rounded-md px-0 text-[12px] text-text-2 hover:text-text"
              aria-label={`Permission mode: ${permissionLabel(permissionMode)}`}
              aria-haspopup="menu"
              aria-expanded={showPermissionMenu}
              ref={permissionTriggerRef}
              onClick={() => setShowPermissionMenu((open) => !open)}
            >
              {permissionLabel(permissionMode)}
              <Icon name="chevron" size={10} />
            </button>
            {showPermissionMenu ? (
              <Popover
                anchor={permissionTriggerRef.current}
                onClose={() => setShowPermissionMenu(false)}
                label="Permission mode"
                placement="top-start"
                width={270}
              ><div
                role="menu"
                aria-label="Permission mode"
                className="py-1"
              >
                {permissionModes.map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    role="menuitemradio"
                    aria-checked={mode === permissionMode}
                    className="flex w-full cursor-pointer items-start gap-2 px-3 py-2 text-left text-[13px] text-text-2 hover:bg-panel-2 hover:text-text"
                    onClick={() => {
                      onSelectPermissionMode(mode);
                      setShowPermissionMenu(false);
                    }}
                  >
                    <span className="min-w-0 flex-1 text-text">
                      <span className="block font-medium">{permissionLabel(mode)}</span>
                      <span className="mt-0.5 block text-[11px] leading-4 text-text-3">
                        {permissionDescription(mode)}
                      </span>
                    </span>
                    {mode === permissionMode ? (
                      <span aria-hidden="true" className="text-accent">
                        ✓
                      </span>
                    ) : null}
                  </button>
                ))}
              </div></Popover>
            ) : null}
          </div>
          {showGoalComposer ? (
            <button
              type="button"
              data-testid="composer-goal-chip"
              aria-pressed="true"
              aria-label="Exit goal mode"
              title="Exit goal mode"
              className="cr-control-pill inline-flex h-6 shrink-0 cursor-pointer items-center gap-1.5 rounded-md border border-accent/40 bg-[color-mix(in_srgb,var(--color-accent)_8%,transparent)] px-2 text-[12px] text-accent"
              onClick={onToggleGoalComposer}
            >
              <Icon name="flag" size={11} />
              Goal mode
              <Icon name="close" size={10} />
            </button>
          ) : null}
          <button
            type="button"
            className="grid h-6 w-6 shrink-0 place-items-center rounded-md text-text-3 hover:bg-panel hover:text-text"
            aria-label="Attach files"
            onClick={handleAttachClick}
          >
            <Icon name="attachment" size={13} />
          </button>
          <TemplatePicker
            builtInTemplates={BUILT_IN_TEMPLATES}
            userTemplates={userTemplates}
            onSelect={selectTemplate}
            onDeleteUser={deleteUserTemplate}
            open={templatePickerOpen}
            onToggle={toggleTemplatePicker}
            pinnedIds={pinnedTemplateIds}
            onTogglePin={toggleTemplatePin}
            onExportTemplate={(template) => downloadJson(`${template.id}.json`, template)}
            onImportBundle={() => templateInputRef.current?.click()}
            onExportAll={() => downloadJson("coderight-templates.json", userTemplates)}
            templateGroups={templateGroups}
            onCreateGroup={createTemplateGroup}
            onRenameGroup={renameTemplateGroup}
            onDeleteGroup={deleteTemplateGroup}
            onMoveToGroup={moveTemplateToGroup}
          />
          {/* Compact moved out of the title bar (Adrian, 2026-08-04): it acts on
              the conversation, not the window, and it discards detail — so it
              sits with the other composer controls and asks first. `/compact`
              does the same thing from the keyboard. */}
          {onCompact && canCompact ? (
            <button
              type="button"
              className="grid h-6 w-6 shrink-0 place-items-center rounded-md text-text-3 hover:bg-panel hover:text-text"
              aria-label="Compact the conversation"
              title="Compact the conversation"
              onClick={() => setConfirmCompact(true)}
            >
              <Icon name="queue" size={13} />
            </button>
          ) : null}
          <div className="sr-only" data-testid="composer-mode-pills">
            {MODE_PILLS.map((m) => (
              <ModePill
                key={m}
                label={m === "read" ? "Read" : m === "edit" ? "Edit" : "Plan"}
                mode={m}
                current={mode}
                onSelect={setMode}
              />
            ))}
          </div>

          <span className="min-w-2 flex-1" />

          <div
            className="relative flex min-w-0 flex-shrink flex-wrap items-center justify-end gap-1.5"
            data-composer-menu
          >
            <button
              ref={modelPickerTriggerRef}
              type="button"
              className={`cr-control-pill inline-flex h-7 min-w-0 items-center gap-1 rounded-md border px-1.5 text-[13px] tabular-nums ${
                modelStateUnavailable
                  ? "border-warning/40 bg-warning/10 text-warning hover:bg-warning/15"
                  : "border-transparent text-text-3 hover:bg-panel hover:text-text-2"
              }`}
              aria-label={activeModel ? "Choose conductor model" : modelStateLabel}
              title={selectionSwitchUnavailableReason ?? modelStateLabel}
              aria-haspopup="menu"
              aria-expanded={showModelMenu}
              aria-disabled={selectionSwitchDisabled}
              disabled={selectionSwitchDisabled}
              data-testid="composer-model-picker"
              onClick={() => {
                if (showModelMenu) closeModelMenu(false);
                else openModelMenu();
              }}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown" && !showModelMenu) {
                  e.preventDefault();
                  openModelMenu("first");
                } else if (e.key === "ArrowUp" && !showModelMenu) {
                  e.preventDefault();
                  openModelMenu("last");
                } else if ((e.key === "Enter" || e.key === " ") && !showModelMenu) {
                  e.preventDefault();
                  openModelMenu("first");
                } else if (e.key === "Escape" && showModelMenu) {
                  e.preventDefault();
                  closeModelMenu();
                }
              }}
            >
              {modelStateUnavailable ? <Icon name="alert" size={12} /> : null}
              <span className="max-w-[140px] truncate @[560px]:max-w-[240px]">
                {modelStateLabel}
              </span>
              <Icon name="chevron" size={10} />
            </button>
            {modelStateUnavailable ? (
              <button
                type="button"
                className="inline-flex h-7 shrink-0 items-center gap-1 rounded-md border border-warning/40 bg-warning/10 px-1.5 text-[12px] text-warning hover:bg-warning/15"
                aria-label="Open Providers settings"
                title="Open Providers settings"
                onClick={() => {
                  setShowModelMenu(false);
                  props.onOpenProviderSettings();
                }}
              >
                <Icon name="cloud" size={11} />
                Providers
              </button>
            ) : null}
            {supportsThinkingLevels ? (
              <button
                ref={effortPickerTriggerRef}
                type="button"
                className="cr-control-pill inline-flex h-7 shrink-0 items-center gap-1 rounded-md border border-transparent px-1.5 text-[13px] tabular-nums text-text-3 hover:bg-panel hover:text-text-2"
                aria-label="Choose thinking level"
                aria-haspopup="menu"
                aria-expanded={showEffortMenu}
                data-testid="composer-effort-picker"
                onClick={() => {
                  setShowModelMenu(false);
                  setShowEffortMenu((open) => !open);
                }}
                onKeyDown={(e) => {
                  if (e.key === "ArrowDown" && !showEffortMenu) {
                    e.preventDefault();
                    setShowModelMenu(false);
                    setShowEffortMenu(true);
                  } else if (e.key === "Escape" && showEffortMenu) {
                    e.preventDefault();
                    setShowEffortMenu(false);
                    requestAnimationFrame(() => effortPickerTriggerRef.current?.focus());
                  }
                }}
              >
                <span>{effortLabel}</span>
                <span className="h-3 w-3 rounded-full border border-line-2" aria-hidden="true" />
              </button>
            ) : null}
            {showModelMenu ? (
              // Positioning wrapper carries no overflow of its own; the list
              // below scrolls instead. A submenu anchored to this wrapper would
              // otherwise be clipped by the scroll container it sits in.
              <>
              <Popover
                anchor={modelPickerTriggerRef.current}
                onClose={() => {
                  // A vendor submenu is its own panel; a press inside it is not an outside press on this menu.
                  if (!expandedFlyout) closeModelMenu(false);
                }}
                label="Conductor models"
                placement="top-end"
                width={300}
              ><div ref={modelMenuRef} className="w-full">
                <div
                  role="menu"
                  aria-label="Conductor models"
                  onKeyDown={onModelMenuKeyDown}
                  className="max-h-[320px] w-full overflow-y-auto rounded-lg border border-line bg-panel py-1 shadow-app"
                >
                  {catalogGroups
                    .filter((group) => group.models.some((model) => !model.id.startsWith("auto/")))
                    .map((group) => (
                      <CatalogModelGroup
                        key={`${group.authority}:${group.connectionId}`}
                        group={group}
                        activeSelection={activeSelection}
                        onSelect={(selection) => {
                          onSelectionChange?.(selection);
                          closeModelMenu();
                        }}
                      />
                    ))}
                  {!hasConcreteCatalogModels ? (
                    <>
                      <fieldset aria-label="Conductor" className="m-0 border-0 p-0 pb-1">
                        <p className="px-3 pb-1 pt-1 text-[10px] font-semibold uppercase tracking-wide text-text-3">
                          Conductor
                        </p>
                        {conductorOptions.map((option) => {
                          const selected = option.kind === activeConductor;
                          const models = option.models ?? [];
                          const hasModels = option.available && models.length > 0;
                          const expanded = hasModels && expandedConductorKind === option.kind;
                          const modelsId = `conductor-models-${option.kind}`;
                          return (
                            <div key={option.kind} className="contents">
                              <button
                                type="button"
                                role="menuitem"
                                tabIndex={-1}
                                // Stable selector for conductor choice (installed QA and
                                // tests). It moved here from the retired start-point
                                // chooser; dropping it silently breaks selectRootConductor.
                                data-driver-kind={option.kind}
                                aria-disabled={!option.available}
                                disabled={!option.available}
                                aria-haspopup={hasModels ? "true" : undefined}
                                aria-expanded={hasModels ? expanded : undefined}
                                aria-controls={hasModels ? modelsId : undefined}
                                className={`flex w-full items-start gap-2 px-3 py-1.5 text-left text-[13px] focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent/60 ${
                                  option.available
                                    ? "text-text-2 hover:bg-panel-2 hover:text-text"
                                    : "cursor-not-allowed text-text-3 opacity-70"
                                }`}
                                onMouseEnter={() => {
                                  // Convenience only — every state this reaches is
                                  // also reachable by keyboard via onClick below, so
                                  // hover is never the only way into the submenu.
                                  if (hasModels) setExpandedConductorKind(option.kind);
                                }}
                                onFocus={() => {
                                  if (hasModels) setExpandedConductorKind(option.kind);
                                }}
                                onClick={() => {
                                  if (!option.available) return;
                                  if (hasModels) {
                                    setExpandedConductorKind((current) =>
                                      current === option.kind ? null : option.kind,
                                    );
                                    return;
                                  }
                                  onConductorChange(option.kind);
                                  closeModelMenu();
                                }}
                              >
                                <span className="min-w-0 flex-1">
                                  <span className="block truncate text-text">{option.label}</span>
                                  <span className="mt-0.5 block truncate text-[11px] text-text-3">
                                    {option.detail}
                                  </span>
                                  {!option.available && option.unavailableReason ? (
                                    <span className="mt-0.5 block text-[11px] text-warning">
                                      {option.unavailableReason}
                                    </span>
                                  ) : null}
                                </span>
                                <span className="mt-0.5 flex shrink-0 items-center gap-1">
                                  {selected ? (
                                    <span aria-hidden="true" className="text-accent">
                                      ✓
                                    </span>
                                  ) : null}
                                  {hasModels ? <Icon name="chevron" size={10} /> : null}
                                </span>
                              </button>
                            </div>
                          );
                        })}
                      </fieldset>

                      <fieldset
                        aria-label=`Native ${hostAppName()}`
                        className="m-0 border-0 border-t border-line p-0 pt-1"
                      >
                        {/* Native is not a separate row above this list — choosing one
                      of these models IS choosing Native the host app. */}
                        <p className="px-3 pb-1 pt-1 text-[10px] font-semibold uppercase tracking-wide text-text-3">
                          Native {hostAppName()}
                        </p>
                        {modelOptions.length === 0 ? (
                          <div className="px-3 py-2">
                            <p className="text-[12px] text-text-2">No enabled conductor models.</p>
                            <button
                              type="button"
                              role="menuitem"
                              tabIndex={-1}
                              className="mt-2 text-[12px] font-medium text-accent hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                              onClick={() => {
                                closeModelMenu();
                                props.onOpenModelSettings();
                              }}
                            >
                              Configure models
                            </button>
                          </div>
                        ) : (
                          modelOptions.map((option) => {
                            const selected =
                              activeConductor === "native" && option.id === activeModel?.id;
                            // A model whose provider has no key cannot run. Offering
                            // it as if it could turns a wrong choice into a failure
                            // at send time instead of at the moment of choosing.
                            const blocked = Boolean(option.unavailableReason);
                            return (
                              <button
                                key={option.id}
                                type="button"
                                role="menuitem"
                                tabIndex={-1}
                                aria-disabled={blocked}
                                disabled={blocked}
                                className={`flex w-full items-start gap-2 px-3 py-1.5 text-left text-[13px] focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent/60 ${
                                  blocked
                                    ? "cursor-not-allowed text-text-3 opacity-70"
                                    : "text-text-2 hover:bg-panel-2 hover:text-text"
                                }`}
                                onClick={() => {
                                  if (blocked) return;
                                  if (activeConductor !== "native") onConductorChange("native");
                                  onConductorModelChange(option.id);
                                  closeModelMenu();
                                }}
                              >
                                <span className="min-w-0 flex-1">
                                  <span className="block text-text">{option.label}</span>
                                  {option.unavailableReason ? (
                                    <span className="mt-0.5 block text-[11px] text-warning">
                                      {option.unavailableReason}
                                    </span>
                                  ) : null}
                                </span>
                                {selected ? (
                                  <span aria-hidden="true" className="text-accent">
                                    ✓
                                  </span>
                                ) : null}
                              </button>
                            );
                          })
                        )}
                      </fieldset>
                      {accountPicker && accountPicker.accounts.length > 0
                        ? (["claude", "codex"] as const).map((providerKind) => {
                            const providerAccounts = accountPicker.accounts.filter((a) =>
                              providerKind === "claude"
                                ? isClaudeProvider(a.provider)
                                : a.provider.toLowerCase() === "codex",
                            );
                            if (providerAccounts.length === 0) return null;
                            const providerOption = conductorOptions.find(
                              (o) => o.kind === providerKind,
                            );
                            return (
                              <div
                                key={providerKind}
                                className="border-t border-line p-2"
                                data-testid={
                                  providerKind === "claude"
                                    ? "composer-account-picker"
                                    : `composer-account-picker-${providerKind}`
                                }
                              >
                                <AccountModelPicker
                                  accounts={providerAccounts}
                                  models={(providerOption?.models ?? []).map((m) => ({
                                    id: m.id,
                                    label: m.label,
                                  }))}
                                  selectedAccountId={accountPicker.selectedAccountId}
                                  selectedModelId={
                                    activeConductor === providerKind
                                      ? (providerOption?.selectedModelId ?? null)
                                      : null
                                  }
                                  projectDefaultAccountId={accountPicker.projectDefaultAccountId}
                                  onSelect={(accountId, modelId) => {
                                    accountPicker.onSelectAccount(accountId);
                                    if (activeConductor !== providerKind) {
                                      onConductorChange(providerKind);
                                    }
                                    onConductorModelSelect(providerKind, modelId);
                                    closeModelMenu();
                                  }}
                                  onSetProjectDefault={accountPicker.onSetProjectDefault}
                                />
                              </div>
                            );
                          })
                        : null}
                      {activeConductor !== "native" ? (
                        <p className="border-t border-line px-3 py-2 text-[11px] text-text-3">
                          {`${activeConductorOption?.label ?? activeConductor} is conducting and brings its own model. Pick one below to hand the session back to ${hostAppName()}.`}
                        </p>
                      ) : null}
                    </>
                  ) : null}
                </div>
              </div></Popover>
                {/* A vendor's models open BESIDE the menu, not inside it.
                  Expanding in place pushed every other row around and buried
                  the list being read — the point of a submenu is that the
                  parent list holds still. Opens leftward because the picker
                  sits at the window's right edge. */}
                {!hasConcreteCatalogModels && expandedFlyout ? (
                  <Popover
                    anchor={modelMenuRef.current}
                    onClose={() => setExpandedConductorKind(null)}
                    label={`${expandedFlyout.label} models`}
                    placement="top-start"
                    width={260}
                  ><div
                    id={`conductor-models-${expandedFlyout.kind}`}
                    role="menu"
                    aria-label={`${expandedFlyout.label} models`}
                    onKeyDown={onModelMenuKeyDown}
                    ref={flyoutMenuRef} className="max-h-[320px] w-full overflow-y-auto py-1"
                  >
                    <p className="px-3 pb-1 pt-1 text-[10px] font-semibold uppercase tracking-wide text-text-3">
                      {expandedFlyout.label}
                    </p>
                    {(expandedFlyout.models ?? []).map((model) => {
                      const modelSelected =
                        expandedFlyout.kind === activeConductor &&
                        expandedFlyout.selectedModelId === model.id;
                      return (
                        <button
                          key={model.id}
                          type="button"
                          role="menuitem"
                          tabIndex={-1}
                          data-model-id={model.id}
                          className="flex w-full items-start gap-2 px-3 py-1.5 text-left text-[13px] text-text-2 hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent/60"
                          onClick={() => {
                            onConductorChange(expandedFlyout.kind);
                            onConductorModelSelect(expandedFlyout.kind, model.id);
                            closeModelMenu();
                          }}
                        >
                          <span className="min-w-0 flex-1">
                            <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-text">
                              {model.label}
                              {model.isDefault ? (
                                <span className="ml-1 text-[10px] uppercase tracking-wide text-text-3">
                                  Default
                                </span>
                              ) : null}
                            </span>
                            {model.description ? (
                              <span className="mt-0.5 block text-[10px] uppercase tracking-wide text-text-3">
                                {model.description}
                              </span>
                            ) : null}
                          </span>
                          {modelSelected ? (
                            <span aria-hidden="true" className="mt-0.5 shrink-0 text-accent">
                              ✓
                            </span>
                          ) : null}
                        </button>
                      );
                    })}
                  </div></Popover>
                ) : null}
              </>
            ) : null}
            {showEffortMenu ? (
              <Popover
                anchor={effortPickerTriggerRef.current}
                onClose={() => setShowEffortMenu(false)}
                label="Thinking level"
                placement="top-end"
                width={190}
              ><div
                role="menu"
                aria-label="Thinking level"
                className="py-1"
              >
                {(
                  [
                    ["faster", "Low"],
                    ["balanced", "Medium"],
                    ["deeper", "High"],
                  ] as const satisfies readonly [ComposerEffort, string][]
                ).map(([option, label]) => (
                  <button
                    key={option}
                    type="button"
                    role="menuitemradio"
                    aria-checked={composerEffortLabel(effort) === label}
                    className="flex w-full items-center gap-2 px-3 py-2 text-left text-[13px] text-text-2 hover:bg-panel-2 hover:text-text"
                    onClick={() => {
                      setEffort(option);
                      setShowEffortMenu(false);
                    }}
                  >
                    <span className="flex-1">{label}</span>
                    {composerEffortLabel(effort) === label ? (
                      <span className="text-text-3">✓</span>
                    ) : null}
                  </button>
                ))}
              </div></Popover>
            ) : null}
            {isWorking && streamStartedAt !== null ? (
              <span className="sr-only">
                <span>{formatElapsed(elapsedSeconds * 1000)}</span>
                <span>est. ${costUsd.toFixed(4)}</span>
              </span>
            ) : null}
          </div>

          <button
            type="button"
            className="sr-only"
            title="Attach files"
            aria-label="Attach files legacy"
            onClick={handleAttachClick}
          >
            <Icon name="attachment" size={12} />
            Attach
          </button>

          {queueLength > 0 ? (
            <span
              className="inline-flex h-6 shrink-0 items-center gap-1 whitespace-nowrap rounded-full border border-line bg-panel px-2 font-mono text-[11px] tabular-nums text-text-3"
              title={`${queueLength} prompt${queueLength === 1 ? "" : "s"} queued`}
            >
              {queueLength} queued
            </span>
          ) : null}

          {hasUsage ? (
            <details ref={usageDetailsRef} className="ml-1 shrink-0">
              <summary className="inline-flex h-6 cursor-pointer list-none items-center gap-1 rounded-full border border-line bg-transparent px-2 text-[11px] text-text-3 hover:border-line-2 hover:text-text-2">
                <Icon name="activity" size={11} />
                Usage
                <span className="font-mono tabular-nums">{tokenEstimate}</span>
              </summary>
              <div className="absolute bottom-full right-3 z-30 mb-2 min-w-[260px] rounded-xl border border-line bg-panel p-3">
                <ContextUsageMeterInline
                  contextPercent={contextPercent}
                  contextTokens={usageContextTokens}
                  promptTokens={usagePromptTokens}
                  completionTokens={usageCompletionTokens}
                  cacheReadTokens={cacheReadTokens}
                  cacheCreationTokens={usageCacheCreationTokens}
                  costUsd={costUsd}
                />
              </div>
            </details>
          ) : null}
        </div>
      </div>

      {confirmCompact ? (
        <ConfirmDialog
          title="Compact this conversation?"
          description="Earlier turns are replaced by a summary to free up context. The full transcript stays on disk, but the agent will only see the summary from here on."
          confirmLabel="Compact"
          danger={false}
          onCancel={() => setConfirmCompact(false)}
          onConfirm={() => {
            setConfirmCompact(false);
            onCompact?.();
          }}
        />
      ) : null}
    </footer>
  );
}

// Preserve the existing helper import surface for settings and legacy callers.
export {
  goalStatusLabel,
  runLabel,
  skillScopeLabel,
  skillStatusLabel,
  skillToolLabel,
  skillTriggerLabel,
} from "./composerBarHelpers";

function composerEffortLabel(effort: ComposerEffort) {
  switch (effort) {
    case "faster":
      return "Low";
    case "balanced":
      return "Medium";
    case "deeper":
    case "smarter":
      return "High";
  }
}

function permissionDescription(mode: PermissionMode) {
  switch (mode) {
    case "plan":
      return "Read and plan only; all mutations are denied.";
    case "ask":
      return "Ask before every mutating tool call.";
    case "accept-edits":
      return "Apply file edits; ask for shell, network, and destructive actions.";
    case "auto-safe":
      return "Allow safe operations; ask for destructive, browser, or process control.";
    case "bypass":
      return "Allow every tool call. Use only in a trusted disposable workspace.";
  }
}
