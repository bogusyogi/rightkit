export interface BaseProtocolEvent {
  $kind: string
  sessionId: string
  seq: number
  createdAt?: string
}

export interface ToolCall {
  id: string
  name: string
  argumentsJson?: string
}

export interface TranscriptLikeMessage {
  $kind?: string
  id?: string
  content?: string
  summary?: string
}

export interface UsageUpdated extends BaseProtocolEvent {
  $kind: 'usageUpdated'
  promptTokens: number
  completionTokens: number
  costUsd: number
  cacheReadTokens?: number
  cacheCreationTokens?: number
}

export type ProtocolEvent =
  | (BaseProtocolEvent & { $kind: 'sessionStarted'; title?: string })
  | (BaseProtocolEvent & { $kind: 'sessionTitleUpdated'; title: string })
  | (BaseProtocolEvent & { $kind: 'sessionContextFolded'; subtaskId: string; startMarker: string; endMarker: string; fold: TranscriptLikeMessage })
  | (BaseProtocolEvent & { $kind: 'selfReviewProposalReceived'; proposal: { id: string; kind: string; summary: string; evidence: string } })
  | (BaseProtocolEvent & { $kind: 'assistantDelta'; text: string; isReasoning: boolean })
  | (BaseProtocolEvent & { $kind: 'assistantMessageCompleted'; message?: TranscriptLikeMessage })
  | (BaseProtocolEvent & { $kind: 'toolCallStarted'; call: ToolCall })
  | (BaseProtocolEvent & { $kind: 'toolApprovalRequested'; call: ToolCall; riskLabel: string; proposedChange?: string })
  | (BaseProtocolEvent & { $kind: 'toolApprovalResolved'; toolCallId: string; approved: boolean })
  | (BaseProtocolEvent & { $kind: 'toolResultAdded'; result: TranscriptLikeMessage })
  | (BaseProtocolEvent & { $kind: 'sessionTurnFinished'; turnId: string; status: string })
  | (BaseProtocolEvent & { $kind: 'streamResyncRequired'; afterSeq: number; oldestAvailableSeq: number; highWaterMark: number })
  | (BaseProtocolEvent & { $kind: 'overloaded'; scope: string; limit: number; current: number; retryAfterMs?: number })
  | (BaseProtocolEvent & { $kind: 'runGovernanceRejected'; kind: string; toolCallId: string; budget?: unknown; ownership?: unknown; origin?: unknown })
  | (BaseProtocolEvent & { $kind: 'verificationFinished'; summary: string; passed: boolean; issueCount?: number })
  | (BaseProtocolEvent & { $kind: 'taskGraphUpdated'; summary: string; tasks?: unknown[] })
  | (BaseProtocolEvent & { $kind: 'taskStatusUpdated'; taskId: string; contextId: string; state: string; task?: unknown })
  | (BaseProtocolEvent & { $kind: 'taskArtifactUpdated'; taskId: string; contextId: string; artifactId: string; artifact?: unknown })
  | (BaseProtocolEvent & { $kind: 'artifactLineageUpdated'; taskId: string; contextId: string; artifactId: string; lineage?: unknown })
  | (BaseProtocolEvent & { $kind: 'runInterrupted'; reason?: string })
  | (BaseProtocolEvent & { $kind: 'runFailed'; error: string; kind?: string; hint?: string })
  | (BaseProtocolEvent & { $kind: 'sessionCompacted'; summary: string; droppedCount?: number })
  | UsageUpdated
  | (BaseProtocolEvent & { $kind: 'budgetExceeded'; scope?: string; currentUsd: number; ceilingUsd: number; hint?: string })
  | (BaseProtocolEvent & { $kind: 'providerRouteUpdated'; state?: string; provider?: string; model?: string; failureKind?: string; detail?: string })
  | (BaseProtocolEvent & { $kind: 'dispatchDecided'; strategy: string; rationale: string })
  | (BaseProtocolEvent & { $kind: 'modelRouteDecision'; agentRole: string; modelId: string; tier: string; reason: string; signalsDigest: string; priceSnapshot: unknown; estCostUsd: number; schemaVersion: number })
  | (BaseProtocolEvent & { $kind: 'orchestrationObservationRecorded'; observation: { kind?: string; subject?: string; summary?: string } })
  | (BaseProtocolEvent & { $kind: 'agentQueryIssued'; queryId: string; fromRole: string; toRole: string; queryType: string })
  | (BaseProtocolEvent & { $kind: 'agentQueryResponded'; queryId: string; fromRole: string; toRole: string; confidence: number })
  | (BaseProtocolEvent & { $kind: 'humanSteeringInjected'; injectedBy: string; contentPreview: string })
  | (BaseProtocolEvent & { $kind: 'subAgentSpawned'; subSessionId: string; role: string; taskTitle: string })
  | (BaseProtocolEvent & { $kind: 'subAgentCompleted'; subSessionId: string; role: string; success: boolean; summary: string })
  | (BaseProtocolEvent & { $kind: 'subAgentCancelled'; subSessionId: string; role: string; cancelledBy: string })
  | (BaseProtocolEvent & { $kind: 'userMessageSubmitted'; text: string; nonce?: string })
  | (BaseProtocolEvent & { $kind: 'userQuestionAsked'; questionId: string; questions: Array<{ question: string; header?: string }> })
  | (BaseProtocolEvent & { $kind: 'userQuestionAnswered'; questionId: string })
  | (BaseProtocolEvent & { $kind: 'planPresented'; planId: string; plan: string })
  | (BaseProtocolEvent & { $kind: 'agentRosterUpdated'; agents: Array<{ subSessionId: string; role: string }> })
  | (BaseProtocolEvent & { $kind: 'checkRunCompleted'; checkId: string; command: string; exitCode: number; attempts: number; passed: boolean; jurySkipped: boolean })
  | (BaseProtocolEvent & { $kind: 'goalProgressUpdated'; goalId: string; status: string; tokensUsed: number; budgetTokens: number; continuationCount: number; noProgressCount: number })

export type DisplayRole = 'user' | 'assistant' | 'tool' | 'approval' | 'error'

export interface DisplayEventMessage {
  role: DisplayRole
  text: string
  toolCallId?: string
}

export function protocolEventKinds(): string[] {
  return [
    'sessionStarted',
    'sessionTitleUpdated',
    'sessionContextFolded',
    'selfReviewProposalReceived',
    'assistantDelta',
    'assistantMessageCompleted',
    'toolCallStarted',
    'toolApprovalRequested',
    'toolApprovalResolved',
    'toolResultAdded',
    'sessionTurnFinished',
    'streamResyncRequired',
    'overloaded',
    'runGovernanceRejected',
    'verificationFinished',
    'taskGraphUpdated',
    'taskStatusUpdated',
    'taskArtifactUpdated',
    'artifactLineageUpdated',
    'runInterrupted',
    'runFailed',
    'sessionCompacted',
    'usageUpdated',
    'budgetExceeded',
    'providerRouteUpdated',
    'dispatchDecided',
    'modelRouteDecision',
    'orchestrationObservationRecorded',
    'agentQueryIssued',
    'agentQueryResponded',
    'humanSteeringInjected',
    'subAgentSpawned',
    'subAgentCompleted',
    'subAgentCancelled',
    'userMessageSubmitted',
    'userQuestionAsked',
    'userQuestionAnswered',
    'planPresented',
    'agentRosterUpdated',
    'checkRunCompleted',
    'goalProgressUpdated',
  ]
}

export function messageFromProtocolEvent(ev: ProtocolEvent): DisplayEventMessage | null {
  switch (ev.$kind) {
    case 'sessionStarted':
    case 'sessionTitleUpdated':
    case 'sessionContextFolded':
    case 'selfReviewProposalReceived':
    case 'assistantDelta':
    case 'usageUpdated':
    case 'dispatchDecided':
    case 'modelRouteDecision':
    case 'userMessageSubmitted':
      return null
    case 'assistantMessageCompleted':
      return ev.message?.content
        ? { role: 'assistant', text: ev.message.content }
        : null
    case 'toolCallStarted':
      return { role: 'tool', text: ev.call.name, toolCallId: ev.call.id }
    case 'toolApprovalRequested':
      return {
        role: 'approval',
        text: `${ev.call.name} (${ev.riskLabel})${ev.proposedChange ? `\n${ev.proposedChange}` : ''}`,
        toolCallId: ev.call.id,
      }
    case 'toolApprovalResolved':
      return {
        role: 'tool',
        text: `${ev.approved ? 'Approved' : 'Skipped'} tool call ${ev.toolCallId}`,
        toolCallId: ev.toolCallId,
      }
    case 'toolResultAdded':
      return { role: 'tool', text: ev.result.content ?? ev.result.summary ?? 'Tool result' }
    case 'sessionTurnFinished':
      return { role: 'tool', text: `Turn ${ev.turnId} ${ev.status}` }
    case 'streamResyncRequired':
      return { role: 'error', text: `Stream resync required: events after ${ev.afterSeq} are no longer available (oldest ${ev.oldestAvailableSeq}, high water ${ev.highWaterMark})` }
    case 'overloaded':
      return { role: 'error', text: `Overloaded (${ev.scope}): ${ev.current}/${ev.limit}${ev.retryAfterMs ? ` — retry after ${ev.retryAfterMs}ms` : ''}` }
    case 'runGovernanceRejected':
      return { role: 'error', text: `Run governance rejected (${ev.kind}): tool call ${ev.toolCallId}`, toolCallId: ev.toolCallId }
    case 'verificationFinished':
      return { role: ev.passed ? 'tool' : 'error', text: `${ev.passed ? 'Verification passed' : 'Verification failed'}: ${ev.summary}` }
    case 'taskGraphUpdated':
      return { role: 'tool', text: `Task graph updated: ${ev.summary || `${ev.tasks?.length ?? 0} task(s)`}` }
    case 'taskStatusUpdated':
      return { role: 'tool', text: `A2A task ${ev.taskId} ${ev.state}` }
    case 'taskArtifactUpdated':
      return { role: 'tool', text: `A2A artifact ${ev.artifactId} updated for ${ev.taskId}`, toolCallId: ev.artifactId }
    case 'artifactLineageUpdated':
      return { role: 'tool', text: `A2A artifact lineage updated for ${ev.artifactId}`, toolCallId: ev.artifactId }
    case 'runInterrupted':
      return { role: 'error', text: `Interrupted: ${ev.reason ?? ''}`.trim() }
    case 'runFailed':
      return { role: 'error', text: [ev.error, ev.hint].filter(Boolean).join('\n') }
    case 'sessionCompacted':
      return { role: 'tool', text: `Session compacted: ${ev.summary}` }
    case 'budgetExceeded':
      return { role: 'error', text: `Budget exceeded${ev.scope ? ` (${ev.scope})` : ''}: $${ev.currentUsd.toFixed(4)} of $${ev.ceilingUsd.toFixed(4)}${ev.hint ? `\n${ev.hint}` : ''}` }
    case 'providerRouteUpdated':
      return { role: 'tool', text: `Provider route ${ev.state || 'updated'}: ${[ev.provider, ev.model].filter(Boolean).join(' / ')}${ev.detail ? `\n${ev.detail}` : ''}` }
    case 'orchestrationObservationRecorded':
      return { role: 'tool', text: `Observation: ${ev.observation.summary || ev.observation.subject || ev.observation.kind || 'recorded'}` }
    case 'agentQueryIssued':
      return { role: 'tool', text: `${ev.fromRole} asked ${ev.toRole}: ${ev.queryType}` }
    case 'agentQueryResponded':
      return { role: 'tool', text: `${ev.fromRole} answered ${ev.toRole} (${Math.round(ev.confidence * 100)}% confidence)` }
    case 'humanSteeringInjected':
      return { role: 'user', text: `Steering from ${ev.injectedBy}: ${ev.contentPreview}` }
    case 'subAgentSpawned':
      return { role: 'tool', text: `Sub-agent started: ${ev.role} — ${ev.taskTitle}`, toolCallId: ev.subSessionId }
    case 'subAgentCompleted':
      return { role: ev.success ? 'tool' : 'error', text: `Sub-agent ${ev.role} completed: ${ev.summary}`, toolCallId: ev.subSessionId }
    case 'subAgentCancelled':
      return { role: 'error', text: `Sub-agent ${ev.role} cancelled by ${ev.cancelledBy}`, toolCallId: ev.subSessionId }
    case 'userQuestionAsked':
      return { role: 'approval', text: ev.questions.map((q) => q.header ? `${q.header}: ${q.question}` : q.question).join('\n'), toolCallId: ev.questionId }
    case 'userQuestionAnswered':
      return { role: 'tool', text: `Question answered: ${ev.questionId}`, toolCallId: ev.questionId }
    case 'planPresented':
      return { role: 'approval', text: ev.plan, toolCallId: ev.planId }
    case 'agentRosterUpdated':
      return { role: 'tool', text: `Agent roster updated: ${ev.agents.length} agent(s)` }
    case 'checkRunCompleted':
      return { role: ev.passed ? 'tool' : 'error', text: `Check ${ev.passed ? 'passed' : 'failed'}: ${ev.command}` }
    case 'goalProgressUpdated':
      return { role: 'tool', text: `Goal ${ev.status}: ${ev.tokensUsed} token(s) used`, toolCallId: ev.goalId }
  }
}
