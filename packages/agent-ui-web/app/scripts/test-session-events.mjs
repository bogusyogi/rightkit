import assert from 'node:assert/strict'
import fs from 'node:fs'
import vm from 'node:vm'
import ts from '@typescript/typescript6'

const sourceUrl = new URL('../../src/contexts/sessionEventFormat.ts', import.meta.url)
const source = fs.readFileSync(sourceUrl, 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText

const module = { exports: {} }
vm.runInNewContext(compiled, { module, exports: module.exports, Math })

const { messageFromProtocolEvent, protocolEventKinds } = module.exports

const base = { sessionId: 's1', seq: 1, createdAt: 't' }
const fixtures = {
  sessionStarted: { ...base, $kind: 'sessionStarted', title: 'S1' },
  assistantDelta: { ...base, $kind: 'assistantDelta', text: 'hi', isReasoning: false },
  assistantMessageCompleted: { ...base, $kind: 'assistantMessageCompleted', message: { content: 'done' } },
  toolCallStarted: { ...base, $kind: 'toolCallStarted', call: { id: 'tc1', name: 'read_file' } },
  toolApprovalRequested: { ...base, $kind: 'toolApprovalRequested', call: { id: 'tc1', name: 'write_file' }, riskLabel: 'high' },
  toolApprovalResolved: { ...base, $kind: 'toolApprovalResolved', toolCallId: 'tc1', approved: true },
  toolResultAdded: { ...base, $kind: 'toolResultAdded', result: { content: 'ok' } },
  verificationFinished: { ...base, $kind: 'verificationFinished', summary: 'checked', passed: true },
  taskGraphUpdated: { ...base, $kind: 'taskGraphUpdated', summary: '2 tasks', tasks: [] },
  taskStatusUpdated: { ...base, $kind: 'taskStatusUpdated', taskId: 'task1', contextId: 'ctx1', state: 'running' },
  taskArtifactUpdated: { ...base, $kind: 'taskArtifactUpdated', taskId: 'task1', contextId: 'ctx1', artifactId: 'art1' },
  artifactLineageUpdated: { ...base, $kind: 'artifactLineageUpdated', taskId: 'task1', contextId: 'ctx1', artifactId: 'art1' },
  runInterrupted: { ...base, $kind: 'runInterrupted', reason: 'user' },
  runFailed: { ...base, $kind: 'runFailed', error: 'boom', hint: 'retry' },
  sessionCompacted: { ...base, $kind: 'sessionCompacted', summary: 'older turns' },
  usageUpdated: { ...base, $kind: 'usageUpdated', promptTokens: 1, completionTokens: 2, costUsd: 0.01 },
  budgetExceeded: { ...base, $kind: 'budgetExceeded', scope: 'session', currentUsd: 2, ceilingUsd: 1 },
  providerRouteUpdated: { ...base, $kind: 'providerRouteUpdated', state: 'fallback', provider: 'openrouter', model: 'model' },
  orchestrationObservationRecorded: { ...base, $kind: 'orchestrationObservationRecorded', observation: { summary: 'noted' } },
  agentQueryIssued: { ...base, $kind: 'agentQueryIssued', queryId: 'q1', fromRole: 'lead', toRole: 'verifier', queryType: 'check' },
  agentQueryResponded: { ...base, $kind: 'agentQueryResponded', queryId: 'q1', fromRole: 'verifier', toRole: 'lead', confidence: 0.8 },
  humanSteeringInjected: { ...base, $kind: 'humanSteeringInjected', injectedBy: 'operator', contentPreview: 'focus' },
  subAgentSpawned: { ...base, $kind: 'subAgentSpawned', subSessionId: 'sub1', role: 'Verifier', taskTitle: 'test' },
  subAgentCompleted: { ...base, $kind: 'subAgentCompleted', subSessionId: 'sub1', role: 'Verifier', success: true, summary: 'passed' },
  subAgentCancelled: { ...base, $kind: 'subAgentCancelled', subSessionId: 'sub1', role: 'Verifier', cancelledBy: 'user' },
  userMessageSubmitted: { ...base, $kind: 'userMessageSubmitted', text: 'hello', nonce: 'n1' },
  userQuestionAsked: { ...base, $kind: 'userQuestionAsked', questionId: 'uq1', questions: [{ header: 'Auth', question: 'Which method?' }] },
  userQuestionAnswered: { ...base, $kind: 'userQuestionAnswered', questionId: 'uq1' },
  planPresented: { ...base, $kind: 'planPresented', planId: 'p1', plan: 'Plan text' },
  agentRosterUpdated: { ...base, $kind: 'agentRosterUpdated', agents: [{ subSessionId: 'sub1', role: 'Verifier' }] },
  checkRunCompleted: { ...base, $kind: 'checkRunCompleted', checkId: 'c1', command: 'pnpm test', exitCode: 0, attempts: 1, passed: true, jurySkipped: false },
  goalProgressUpdated: { ...base, $kind: 'goalProgressUpdated', goalId: 'g1', status: 'running', tokensUsed: 10, budgetTokens: 100, continuationCount: 1, noProgressCount: 0 },
  dispatchDecided: { ...base, $kind: 'dispatchDecided', backend: 'native', reason: 'builtin' },
  modelRouteDecision: { ...base, $kind: 'modelRouteDecision', decision: { tier: 'cheap', reason: 'unit-test' } },
  selfReviewProposalReceived: { ...base, $kind: 'selfReviewProposalReceived', proposal: { summary: 'note' } },
  sessionTitleUpdated: { ...base, $kind: 'sessionTitleUpdated', title: 'New title' },
  sessionContextFolded: { ...base, $kind: 'sessionContextFolded', subtaskId: 'sub1', refId: 'fold-1' },
  sessionTurnFinished: { ...base, $kind: 'sessionTurnFinished', turnId: 'turn-1', status: 'completed' },
  streamResyncRequired: { ...base, $kind: 'streamResyncRequired', afterSeq: 4, oldestAvailableSeq: 9, highWaterMark: 20 },
  overloaded: { ...base, $kind: 'overloaded', scope: 'inference', limit: 4, current: 4 },
  runGovernanceRejected: { ...base, $kind: 'runGovernanceRejected', kind: 'unbacked', toolCallId: 'tc1' },
}

assert.deepEqual(JSON.parse(JSON.stringify(protocolEventKinds().sort())), Object.keys(fixtures).sort())

for (const kind of protocolEventKinds()) {
  assert.doesNotThrow(() => messageFromProtocolEvent(fixtures[kind]), kind)
}

assert.equal(messageFromProtocolEvent(fixtures.usageUpdated), null)
assert.equal(messageFromProtocolEvent(fixtures.userMessageSubmitted), null)
assert.equal(messageFromProtocolEvent(fixtures.budgetExceeded).role, 'error')
