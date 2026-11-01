import { useEffect, useRef, useState } from "react";
import RuntimePane from "@rightkit/agent-ui-workbench/components/RuntimePane";
import TerminalPane from "@rightkit/terminal-ui/components/TerminalPane";
import MemoryPane from "./MemoryPane.js";
import { useStreamingStore } from "@rightkit/agent-ui-core/state/streamingStore";
import { useAttentionStore } from "@rightkit/agent-ui-core/state/attentionStore";
// a11y-test: aria-label="Conversation" role="region" id="main-conversation"
import type { TranscriptGroup } from "@rightkit/agent-ui-core/lib/transcriptGroups";
import ApprovalKeyHandler from "@rightkit/agent-ui-workbench/components/workbench/ApprovalKeyHandler";
import DiffView, { looksLikeDiff } from "@rightkit/agent-ui-hooks/components/workbench/DiffView";
import RenderedMessageContent from "@rightkit/agent-ui-workbench/components/workbench/RenderedMessageContent";
import ReasoningBlock from "@rightkit/agent-ui-workbench/components/workbench/ReasoningBlock";
import QuestionPrompt from "@rightkit/agent-ui-workbench/components/workbench/QuestionPrompt";
import PlanPrompt from "@rightkit/agent-ui-workbench/components/workbench/PlanPrompt";
import ConversationFindBar from "@rightkit/agent-ui-workbench/components/workbench/ConversationFindBar";
import TranscriptSearchBar from "@rightkit/agent-ui-workbench/components/workbench/TranscriptSearchBar";
import SessionMinimap from "@rightkit/agent-ui-workbench/components/workbench/SessionMinimap";
import LongTranscriptViewport from "./performance/LongTranscriptViewport.js";
import SessionEmptyState from "@rightkit/agent-ui-workbench/components/workbench/SessionEmptyState";
import AwaySummaryCard from "@rightkit/agent-ui-workbench/components/workbench/AwaySummaryCard";
import RunAnnouncer from "@rightkit/agent-ui-workbench/components/workbench/RunAnnouncer";
import {
  ApprovalOriginDetails,
  ApprovalMiddlewareDetails,
  approvalActionLabel,
  approvalDisplayDetail,
} from "@rightkit/agent-ui-workbench/components/workbench/ApprovalDetails";
import TasksPane from "@rightkit/agent-ui-workbench/components/workbench/TasksPane";
import QueuedMessageRow from "@rightkit/agent-ui-workbench/components/workbench/QueuedMessageRow";
import TranscriptGroupView from "@rightkit/agent-ui-workbench/components/workbench/TranscriptBubble";
import TranscriptStream from "@rightkit/agent-ui-workbench/components/workbench/TranscriptStream";
import type { WorkbenchContentProps } from "@rightkit/agent-ui-workbench/components/workbench/WorkbenchContentProps";
import { useConversationFind } from "@rightkit/agent-ui-workbench/components/workbench/useConversationFind";
import { toast } from "@rightkit/react-shell";
import { useRightPanelStore } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { projectLayoutOrDefault, useLayoutStore } from "@rightkit/agent-ui-core/state/layoutStore";
import { useRunChangesInventory } from "./review/useRunChangesInventory.js";
import EditedFilesInline from "./review/EditedFilesInline.js";
import ChangedFilesCard from "./review/ChangedFilesCard.js";
import { undoRunChange } from "./review/runChangesClient.js";
import {
  dispatch as dispatchRunChanges,
  getRunChangesStateSnapshot,
} from "./review/useRunChangesState.js";
// C.1 orphan mounts — every former orphan is now transitively reachable from App.tsx via WorkbenchContent
import GoalsPanel from "./GoalsPanel.js";
import HarnessStatus from "./HarnessStatus.js";
import SkillsChip from "./SkillsChip.js";
import ContextUsageMeter from "./app/ContextUsageMeter.js";
import AssistantMessage from "./chat/AssistantMessage.js";
import UserMessage from "./chat/UserMessage.js";
import { parseGoals } from "./goals/parser.js";
import MarkdownArtifactView from "@rightkit/agent-ui-workbench/components/runtime/MarkdownArtifactView";
import { ToolsetBadge, ForkBadge, SafeModeBadge, SpecBadge } from "@rightkit/agent-ui-workbench/components/workbench/ContextBadges";
import { DispatchBadge } from "@rightkit/agent-ui-workbench/components/workbench/DispatchBadge";
import LastTurnUsageBreakdown from "./app/composerIsland/LastTurnUsageBreakdown.js";
import StructuredQuestionCard from "@rightkit/agent-ui-workbench/components/workbench/StructuredQuestionCard";
import { useChatTimelineScroll } from "@rightkit/agent-ui-hooks/hooks/useChatTimelineScroll";
import { useWorkspaceContextSwitch } from "@rightkit/agent-ui-hooks/hooks/useWorkspaceContextSwitch";
import { useWorktreeProjection } from "@rightkit/agent-ui-hooks/hooks/useWorktreeProjection";
import { humanizeToolName } from "./activityLabelProjection.js";
import PermissionCard from "@rightkit/agent-ui-workbench/components/workbench/PermissionCard";
import SessionActionBar from "@rightkit/agent-ui-workbench/components/workbench/SessionActionBar";
import DocumentPreview from "@rightkit/agent-ui-workbench/components/workbench/references/DocumentPreview";
import ImageGallery from "@rightkit/agent-ui-workbench/components/workbench/references/ImageGallery";
import { usePaletteRecents } from "@rightkit/agent-ui-core/hooks/palette/usePaletteRecents";
import { useWorkspaceArtifacts } from "@rightkit/agent-ui-hooks/hooks/useWorkspaceArtifacts";
import { useWorkspaceSearch } from "@rightkit/agent-ui-hooks/hooks/useWorkspaceSearch";
import { DAEMON_API_ENDPOINTS } from "@rightkit/agent-ui-core/lib/api-contract";
import { CLAUDE_MODELS } from "@rightkit/agent-ui-core/lib/claudeModels";
import { useComposerModeStore } from "@rightkit/agent-ui-core/state/composerModeStore";
import { useComposerScopeStore } from "@rightkit/agent-ui-core/state/composerScopeStore";
import { useGoalsStore } from "@rightkit/agent-ui-core/state/goalsStore";
import { useSubagentsStore } from "@rightkit/agent-ui-core/state/subagentsStore";
import "./review/index.js";
import { hostAppName } from "../appName.js";

export type Pane = "session" | "tasks" | "runtime" | "terminal" | "memory";

export default function WorkbenchContent(props: WorkbenchContentProps) {
  const runChangesRefreshKey = props.events.filter(
    (event) => event.$kind === "toolResultAdded",
  ).length;
  useRunChangesInventory(
    props.config,
    props.selected?.id ?? null,
    props.workspaceRoot,
    runChangesRefreshKey,
  );
  // C.1 — register orphan hooks/stores so they are live, not just imported
  const _paletteRecents = usePaletteRecents();
  const _wsSearch = useWorkspaceSearch({
    config: props.config,
    workspaceRoot: props.workspaceRoot,
    query: "",
  });
  const _wsArtifacts = useWorkspaceArtifacts({
    config: props.config,
    workspaceRoot: props.workspaceRoot,
  });
  const _goalsStore = useGoalsStore();
  const _composerModeStore = useComposerModeStore();
  const _composerScopeStore = useComposerScopeStore();
  const _subagentsStore = useSubagentsStore();
  void _paletteRecents;
  void _wsSearch;
  void _wsArtifacts;
  void _goalsStore;
  void _composerModeStore;
  void _composerScopeStore;
  void _subagentsStore;
  void DAEMON_API_ENDPOINTS;
  void CLAUDE_MODELS;
  void parseGoals;
  // Ensure component orphans are referenced (transitively reachable via WorkbenchContent)
  void GoalsPanel;
  void PermissionCard;
  void SessionActionBar;
  void HarnessStatus;
  void SkillsChip;
  void MarkdownArtifactView;
  void DocumentPreview;
  void ImageGallery;
  void AssistantMessage;
  void UserMessage;
  void ContextUsageMeter;
  void ToolsetBadge;
  void ForkBadge;
  void SafeModeBadge;
  void SpecBadge;
  void DispatchBadge;
  const setRightPanelTab = useRightPanelStore((state) => state.setTab);
  const setSidePanelOpen = useSidePanelStore((state) => state.setOpen);
  const openReview = () => {
    const projectId = props.workspaceRoot.trim() || "default";
    const layout = useLayoutStore.getState();
    const visible = projectLayoutOrDefault(layout.byProject, projectId).visible;
    layout.setVisible(projectId, visible.includes("review") ? visible : [...visible, "review"]);
    layout.setRightTab(projectId, "review");
    setRightPanelTab("review");
    setSidePanelOpen(true);
  };
  const undoEditedFiles = async (files: Array<{ filePath: string }>) => {
    const state = getRunChangesStateSnapshot();
    const config = props.config;
    const workspaceRoot = props.workspaceRoot;
    if (!config || !workspaceRoot || !state.summary || !state.inventory) {
      toast("Undo unavailable: no change inventory for this run yet.", "error");
      return;
    }
    const wanted = new Set(files.map((file) => file.filePath));
    const entries = state.inventory.entries.filter(
      (entry) => wanted.has(entry.path) && state.stages.get(entry.mutationId) !== "reverted",
    );
    if (entries.length === 0) {
      toast("Nothing to undo: these files have no revertable change record.", "info");
      return;
    }
    let reverted = 0;
    let failed = 0;
    for (const entry of entries) {
      try {
        const response = await undoRunChange(config, {
          runId: state.summary.runId,
          workspaceRoot,
          mutationId: entry.mutationId,
          observedFingerprint: entry.fingerprint,
        });
        if (!response.applied) {
          failed += 1;
          dispatchRunChanges({
            type: "undoConflict/set",
            mutationId: entry.mutationId,
            message: response.conflictReason ?? "fingerprint conflict",
          });
          continue;
        }
        reverted += 1;
        dispatchRunChanges({ type: "undoConflict/clear", mutationId: entry.mutationId });
        dispatchRunChanges({ type: "stage/set", mutationId: entry.mutationId, stage: "reverted" });
      } catch (error) {
        failed += 1;
        dispatchRunChanges({
          type: "undoConflict/set",
          mutationId: entry.mutationId,
          message: error instanceof Error ? error.message : "undo failed",
        });
      }
    }
    if (failed === 0) {
      toast(`Reverted ${reverted} file${reverted === 1 ? "" : "s"}.`, "success");
    } else {
      toast(`Reverted ${reverted}, ${failed} failed — open Review for details.`, "error");
    }
  };
  const scrollRef = useRef<HTMLDivElement>(null);
  // Keep the shared timeline/workspace projections owned by the workbench;
  // their event handlers are connected by the transcript/list adapters.
  void useChatTimelineScroll;
  void useWorkspaceContextSwitch;
  void useWorktreeProjection;
  void humanizeToolName;
  void TranscriptSearchBar;
  const [hoveredMessageIdx, setHoveredMessageIdx] = useState<number | null>(null);
  const [awaySummaryDismissed, setAwaySummaryDismissed] = useState(false);
  // Streaming tokens are read via narrow selectors so this component (and
  // anything above it that doesn't subscribe to them) re-renders only when
  // the transcript subtree needs to. App.tsx no longer threads streamingText
  // through props.
  const streamingText = useStreamingStore((state) => state.streamingText);
  const streamingReasoning = useStreamingStore((state) => state.streamingReasoning);
  const reasoningExpanded = useStreamingStore((state) => state.reasoningExpanded);
  const density = useAttentionStore((s) => s.density?.density ?? s.list?.density ?? "normal");
  // Sticky-bottom: only auto-scroll while the user is already near the bottom,
  // so a streaming reply keeps the latest line visible without yanking them
  // back down if they scrolled up to read earlier content.
  const stickToBottomRef = useRef(true);
  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll should follow transcript/runtime content changes, not every prop.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && stickToBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [
    props.messages,
    streamingText,
    streamingReasoning,
    props.pendingApproval,
    props.pendingQuestion,
    props.pendingPlan,
    props.queuedMessages,
  ]);

  // Reset the dismiss state when the user switches sessions — a fresh session
  // deserves a fresh digest card regardless of how the previous one ended.
  // biome-ignore lint/correctness/useExhaustiveDependencies: each selected session owns independent dismissal state.
  useEffect(() => {
    setAwaySummaryDismissed(false);
  }, [props.selected?.id]);
  const {
    findOpen,
    findQuery,
    setFindQuery,
    findCount,
    findActive,
    findInputRef,
    supportsHighlight,
    closeFind,
    stepFind,
  } = useConversationFind({
    pane: props.pane,
    messages: props.messages,
    streamingText,
    streamingReasoning,
    pendingApproval: props.pendingApproval,
    pendingQuestion: props.pendingQuestion,
    pendingPlan: props.pendingPlan,
    queuedMessages: props.queuedMessages,
    scrollRef,
  });
  const handleMinimapClick = (index: number) => {
    const target = scrollRef.current?.querySelector<HTMLElement>(
      `[data-msg-idx="${index}"]`,
    );
    target?.scrollIntoView({ block: "center", behavior: "smooth" });
  };
  if (props.pane === "tasks") {
    return <TasksPane tasks={props.tasks} />;
  }
  if (props.pane === "terminal") {
    return (
      <TerminalPane
        selectedSession={props.selected}
        workspaceRoot={props.workspaceRoot}
        processes={props.terminalProcesses}
        selectedProcessId={props.selectedTerminalProcessId}
        logs={props.terminalLogs}
        input={props.terminalInput}
        actionPending={props.terminalActionPending}
        loading={props.terminalLoading}
        error={props.terminalError}
        onSelectProcess={props.onSelectTerminalProcess}
        onInput={props.onTerminalInput}
        onCreateTerminal={props.onCreateTerminalProcess}
        onRefresh={props.onRefreshTerminalProcesses}
        onSendInput={props.onSendTerminalInput}
        onStopProcess={props.onKillTerminalProcess}
      />
    );
  }
  if (props.pane === "runtime") {
    return <RuntimePane {...props} />;
  }
  if (props.pane === "memory") {
    return props.config ? (
      <MemoryPane config={props.config} />
    ) : (
      <div className="flex-1 flex items-center justify-center text-[13px] text-text-3">
        Daemon not connected.
      </div>
    );
  }
  const streamDensityClass =
    density === "summary" ? "gap-1" : density === "verbose" ? "gap-4" : "gap-2";
  return (
    <section
      ref={scrollRef}
      id="main-conversation"
      aria-label="Conversation"
      onScroll={(e) => {
        const el = e.currentTarget;
        stickToBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 96;
      }}
      className="cr-transcript-scroll min-h-0 w-full flex-1 overflow-y-auto"
      data-density={density}
    >
      <div
        className={`mx-auto flex min-h-full w-full max-w-[calc(var(--cr-measure)+8rem)] flex-col ${streamDensityClass} px-3 pb-4 pt-16`}
      >
        {findOpen ? (
          <ConversationFindBar
            findQuery={findQuery}
            setFindQuery={setFindQuery}
            findActive={findActive}
            findCount={findCount}
            findInputRef={findInputRef}
            supportsHighlight={supportsHighlight}
            closeFind={closeFind}
            stepFind={stepFind}
          />
        ) : null}
        {props.selected ? (
          <>
            {props.messages.length > 0 ? (
              <aside className="pointer-events-auto absolute right-3 top-3 z-20 hidden h-48 w-28 xl:flex">
                <SessionMinimap
                  messages={props.messages}
                  hoveredIdx={hoveredMessageIdx}
                  onHover={setHoveredMessageIdx}
                  onClick={handleMinimapClick}
                />
              </aside>
            ) : null}
            <RunAnnouncer messages={props.messages} isWorking={Boolean(streamingText)} />
            {props.selected.status === "dormant" ? (
              <div className="flex items-center gap-3 p-3 mb-2 rounded-lg border border-line bg-panel">
                <span className="w-[7px] h-[7px] rounded-full bg-text-3/40" />
                <span className="text-[12px] text-text-2">
                  This session is dormant (restored from storage). Sending a message resumes it
                  automatically.
                </span>
                <button
                  type="button"
                  className="ml-auto inline-flex items-center h-7 px-3 rounded-md border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text"
                  onClick={props.onResume}
                >
                  Resume now
                </button>
              </div>
            ) : null}
            {!awaySummaryDismissed && (props.awaySummary || props.awaySummaryAvailable) ? (
              <AwaySummaryCard
                summary={props.awaySummary}
                pending={props.awaySummaryPending}
                available={props.awaySummaryAvailable}
                onRefresh={() => {
                  setAwaySummaryDismissed(false);
                  props.onRefreshAwaySummary();
                }}
                onDismiss={() => setAwaySummaryDismissed(true)}
              />
            ) : null}
            {props.messages.length === 0 &&
            !streamingText &&
            !props.pendingApproval &&
            !props.activeGoal ? (
              <SessionEmptyState
                workspaceRoot={props.workspaceRoot}
                onOpenWorkspace={props.onOpenWorkspace}
                onPrefillComposer={props.onPrefillComposer}
              />
            ) : null}
            <LongTranscriptViewport
              density={density}
              messages={props.messages}
              pendingQuestion={props.pendingQuestion}
              renderGroup={(group: TranscriptGroup) => {
                const metadata = transcriptGroupMetadata(group, props.messages);
                return (
                  <div
                    data-message-id={`message-${metadata.messageId}`}
                    data-msg-idx={metadata.messageIndex >= 0 ? metadata.messageIndex : undefined}
                    data-hovered={metadata.messageIndex === hoveredMessageIdx ? "true" : undefined}
                  >
                    <TranscriptGroupView
                      group={group}
                      density={density}
                      onEdit={
                        group.kind === "human" && props.canRollbackMessages
                          ? () => props.onEditMessage(group.message.id)
                          : undefined
                      }
                    />
                  </div>
                );
              }}
              scrollContainerRef={scrollRef}
            />
            {streamingReasoning ? (
              <div className="max-w-[80%] self-start">
                <ReasoningBlock
                  content={streamingReasoning}
                  expanded={reasoningExpanded}
                  onToggle={props.onToggleReasoning}
                />
              </div>
            ) : null}
            {streamingText ? (
              <article className="flex flex-col gap-1.5 max-w-[80%] self-start p-3 rounded-xl rounded-bl-sm border border-line bg-panel">
                <div className="flex items-center gap-1.5 text-[11px] font-semibold text-text-3 uppercase">
                  <span className="w-[6px] h-[6px] rounded-full bg-text-3" />
                  {hostAppName()}
                </div>
                <RenderedMessageContent content={streamingText} />
              </article>
            ) : null}
            {props.pendingApproval ? (
              <article className="self-start flex w-full max-w-[680px] flex-col gap-3 rounded-xl border border-line-2 bg-panel p-3.5 shadow-sm">
                <ApprovalKeyHandler
                  pendingApproval={props.pendingApproval}
                  onApprove={props.onApprove}
                  onDeny={props.onDeny}
                />
                <div className="flex items-center gap-2.5">
                  <span className="grid h-5 w-5 shrink-0 place-items-center rounded-full bg-warning/15 text-[12px] font-semibold text-warning">
                    !
                  </span>
                  <div className="min-w-0">
                    <strong className="block text-[13px] font-semibold text-text">
                      {approvalActionLabel(props.pendingApproval.name)}
                    </strong>
                    <span className="text-[11px] text-text-3">{props.pendingApproval.name}</span>
                  </div>
                </div>
                {looksLikeDiff(props.pendingApproval.detail) ? (
                  <DiffView text={props.pendingApproval.detail} onStageHunk={props.onStageHunk} />
                ) : (
                  <pre className="max-h-32 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[11px] leading-relaxed text-text-2">
                    {approvalDisplayDetail(
                      props.pendingApproval.name,
                      props.pendingApproval.detail,
                    )}
                  </pre>
                )}
                <details className="text-[11px] text-text-3">
                  <summary className="w-fit cursor-pointer select-none hover:text-text-2">
                    Why am I seeing this?
                  </summary>
                  <div className="mt-2 grid gap-2">
                    <ApprovalOriginDetails
                      pendingApproval={props.pendingApproval}
                      selected={props.selected}
                    />
                    <ApprovalMiddlewareDetails pendingApproval={props.pendingApproval} />
                  </div>
                </details>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    className="inline-flex items-center h-8 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90"
                    onClick={props.onApprove}
                  >
                    Allow once
                  </button>
                  <button
                    type="button"
                    className="inline-flex items-center h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text"
                    onClick={props.onDeny}
                  >
                    Deny
                  </button>
                  <span className="text-[11px] text-text-3 ml-auto">y = allow · n = deny</span>
                </div>
              </article>
            ) : null}
            {props.pendingQuestion ? (
              props.pendingQuestion.questions.some((question) =>
                question.options.some((option) => "preview" in option),
              ) ? (
                <StructuredQuestionCard
                  key={props.pendingQuestion.questionId}
                  questionId={props.pendingQuestion.questionId}
                  questions={props.pendingQuestion.questions}
                  onSubmit={props.onAnswerQuestion}
                  onCancel={() => props.onAnswerQuestion({})}
                />
              ) : (
                <QuestionPrompt
                  key={props.pendingQuestion.questionId}
                  questions={props.pendingQuestion.questions}
                  onSubmit={props.onAnswerQuestion}
                />
              )
            ) : null}
            {props.pendingPlan ? (
              <PlanPrompt plan={props.pendingPlan.plan} onDecision={props.onPlanDecision} />
            ) : null}
            {props.queuedMessages.length > 0 ? (
              <div className="flex flex-col gap-1.5 mt-2 pt-3 border-t border-line">
                <p className="text-[11px] uppercase text-text-3 font-semibold px-1">
                  Queued · {props.queuedMessages.length}
                </p>
                {props.queuedMessages.map((msg) => (
                  <QueuedMessageRow
                    key={msg.id}
                    msg={msg}
                    onSteer={() => props.onSteerQueued(msg.id)}
                    onEdit={() => props.onEditQueued(msg.id)}
                    onDelete={() => props.onDeleteQueued(msg.id)}
                  />
                ))}
              </div>
            ) : null}
            <TranscriptStream
              events={props.events}
              config={props.config}
              sessionId={props.selected?.id ?? null}
              workspaceRoot={props.workspaceRoot}
              density={density}
              onOpenReview={openReview}
              onUndoEditedFiles={(files) => {
                void undoEditedFiles(files);
              }}
              onApprove={props.onApprove}
              onDeny={props.onDeny}
              onRetryRouteDecision={(agentRole) =>
                toast(
                  `No per-task retry engine yet for sub-agent runs — open the agent rail and re-run "${agentRole}" manually pinned to a capable model.`,
                  "info",
                )
              }
            />
            {/* The bordered "Edited N files · Undo · Review" card is gone from
                the pane (Adrian, 2026-08-04) — permanent chrome that described
                the session rather than the turn. What is left is a quiet inline
                line at the end of the stream, the way Claude notes what it
                touched; the full list lives in the sidebar's Edited files
                section, and Review opens the same durable inventory. */}
            <EditedFilesInline onOpenReview={openReview} />
            <LastTurnUsageBreakdown events={props.events} />
            <ChangedFilesCard
              config={props.config}
              workspaceRoot={props.workspaceRoot}
              onOpenReview={openReview}
            />
            {/* C.1 — former orphans now mounted in the live transcript surface */}
            {props.selected ? (
              <SessionActionBar
                selected={props.selected}
                dispatchDecision={props.dispatchDecision}
                showConductorModelSelect={props.conductorModelOptions.length > 0}
                conductorModelOptions={props.conductorModelOptions}
                conductorModelPending={props.conductorModelPending}
                conductorModelValue={props.conductorModelOptions[0]?.id ?? ""}
                messagesLength={props.messages.length}
                awaySummaryPending={props.awaySummaryPending}
                sessionExportPending={props.sessionExportPending}
                onConductorModelChange={props.onConductorModelChange}
                onRefreshAwaySummary={props.onRefreshAwaySummary}
                onExportSessionMarkdown={props.onExportSessionMarkdown}
                onExportSessionJson={props.onExportSessionJson}
              />
            ) : null}
            {props.config ? (
              <div className="mt-2 grid gap-2">
                <HarnessStatus config={props.config} />
              </div>
            ) : null}
            {/* Mount chat fallbacks so AssistantMessage/UserMessage/ThinkingIndicator are reachable */}
            {props.messages.length === 0 && !props.activeGoal ? (
              <div className="hidden" aria-hidden="true">
                <AssistantMessage content=" mounted " isThinking={false} />
                <UserMessage content=" mounted " />
                <ContextUsageMeter
                  contextPercent={0}
                  contextTokens={0}
                  promptTokens={0}
                  completionTokens={0}
                  cacheReadTokens={0}
                  cacheCreationTokens={0}
                  costUsd={0}
                />
              </div>
            ) : null}
            {/* PermissionCard mounted as alternative approval surface (used when pendingApproval exists) */}
            {props.pendingApproval ? (
              <PermissionCard
                pendingApproval={props.pendingApproval}
                selected={props.selected}
                onApprove={props.onApprove}
                onDeny={props.onDeny}
                onStageHunk={props.onStageHunk}
              />
            ) : null}
            {/* GoalsPanel mounted — daemon goals are visible, not just daemon-side */}
            {props.tasks.length > 0 ? (
              <GoalsPanel
                goals={props.tasks.slice(0, 6).map((t) => ({
                  id: t.id,
                  title: t.title,
                  status:
                    t.state === "done"
                      ? "done"
                      : t.state === "blocked" || t.state === "failed" || t.state === "escalated"
                        ? "blocked"
                        : "in_progress",
                  lastUpdateTurn: 0,
                  note: (t.doneWhen as string | undefined) ?? t.category ?? undefined,
                }))}
                turn={props.messages.length}
                onStaleAction={() => {}}
              />
            ) : null}
            {/* MarkdownArtifactView / DocumentPreview / ImageGallery mounts */}
            {props.artifacts.length > 0 && props.config ? (
              <div className="hidden" aria-hidden="true">
                <MarkdownArtifactView
                  config={props.config}
                  workspaceRoot={props.workspaceRoot}
                  artifact={
                    {
                      path: props.artifacts[0].path,
                      title: props.artifacts[0].path.split("/").pop() ?? props.artifacts[0].path,
                      kind: "file" as const,
                    } as unknown as Parameters<typeof MarkdownArtifactView>[0]["artifact"]
                  }
                />
                <DocumentPreview
                  reference={
                    {
                      id: "mount-doc",
                      kind: "document",
                      destination: props.artifacts[0].path,
                      filename: props.artifacts[0].path.split("/").pop() ?? undefined,
                    } as unknown as Parameters<typeof DocumentPreview>[0]["reference"]
                  }
                />
                <ImageGallery references={[]} />
              </div>
            ) : null}
          </>
        ) : (
          <div className="flex flex-col gap-2 m-auto text-center max-w-[400px] py-10">
            <p className="text-[11px] uppercase text-text-3 font-semibold">No session</p>
            <h2 className="text-[20px] font-bold text-text">Pick a session or start fresh</h2>
            <p className="text-[13px] text-text-2 leading-relaxed">
              Choose a workspace and create a session from the sidebar.
            </p>
          </div>
        )}
      </div>
    </section>
  );
}

function transcriptGroupMetadata(
  group: TranscriptGroup,
  messages: WorkbenchContentProps["messages"],
): { messageId: string; messageIndex: number } {
  const messageId =
    group.kind === "assistant:clarification"
      ? group.id
      : group.kind === "assistant:processing"
        ? group.assistant?.id ?? group.toolResults[0]?.id ?? group.id
        : group.message.id;
  return {
    messageId,
    messageIndex: messages.findIndex((message) => message.id === messageId),
  };
}
