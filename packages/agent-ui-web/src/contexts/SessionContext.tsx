/* eslint-disable react-refresh/only-export-components */
import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import { useDaemon, daemonHeaders, eventStreamUrl } from './DaemonContext'
import { type UsageState, EMPTY_USAGE } from '../components/UsagePanel'
import { buildSessionMessageBody, hasOutgoingPayload } from './messagePayload'
import {
  messageFromProtocolEvent,
  type ProtocolEvent,
  type UsageUpdated,
} from './sessionEventFormat'

// ── UI message model ──────────────────────────────────────────────────────────

export type Role = 'user' | 'assistant' | 'tool' | 'approval' | 'error'

export interface Attachment {
  type: 'image'
  data: string
  mimeType: string
}

export interface Msg {
  id: string
  role: Role
  text: string
  toolCallId?: string
  streaming?: boolean
  nonce?: string
  attachments?: Attachment[]
}

export type SessionEffort = 'low' | 'medium' | 'high'

export interface CreateSessionRequest {
  workspaceRoot: string
  title?: string
  model?: string
  allowedTools?: string[]
  disallowedTools?: string[]
  executionBackend?: string
  effort?: SessionEffort
  thinkingBudgetTokens?: number
  disableThinking?: boolean
}

interface SessionRow {
  id: string
  title: string
  model: string
  activeSkills?: string[]
  executionBackend?: string | null
  executionNetworkProfile?: string | null
  thinkingBudgetTokens?: number | null
  disableThinking?: boolean
}

function readSseMessages(buffer: string, onData: (data: string) => void): string {
  let remaining = buffer
  while (true) {
    const lfLf = remaining.indexOf('\n\n')
    const crlfCrlf = remaining.indexOf('\r\n\r\n')
    const nextBreak = [lfLf, crlfCrlf].filter(index => index >= 0).sort((a, b) => a - b)[0]
    if (nextBreak === undefined) return remaining

    const separatorLength = remaining.startsWith('\r\n\r\n', nextBreak) ? 4 : 2
    const frame = remaining.slice(0, nextBreak)
    remaining = remaining.slice(nextBreak + separatorLength)
    const data = frame
      .split(/\r?\n/)
      .filter(line => line.startsWith('data:'))
      .map(line => line.slice(5).trimStart())
      .join('\n')
    if (data) onData(data)
  }
}

// ── Context value ─────────────────────────────────────────────────────────────

export interface SessionContextValue {
  msgs: Msg[]
  online: boolean
  busy: boolean
  usage: UsageState
  model: string
  models: string[]
  setModel: (model: string) => Promise<void>
  createSession: (request: CreateSessionRequest) => Promise<void>
  send: (text: string, attachments?: Attachment[]) => Promise<void>
  appendAssistant: (text: string, role?: Role) => void
  resolve: (toolCallId: string, approved: boolean) => Promise<void>
  clearMsgs: () => void
}

const SessionContext = createContext<SessionContextValue>({
  msgs: [],
  online: false,
  busy: false,
  usage: EMPTY_USAGE,
  model: '',
  models: [],
  setModel: async () => {},
  createSession: async () => {},
  send: async () => {},
  appendAssistant: () => {},
  resolve: async () => {},
  clearMsgs: () => {},
})

export function SessionProvider({ children }: { children: React.ReactNode }) {
  const { base, token, session, setSession, workspaceRoot } = useDaemon()
  const [msgs, setMsgs] = useState<Msg[]>([])
  const [online, setOnline] = useState(false)
  const [busy, setBusy] = useState(false)
  const [usage, setUsage] = useState<UsageState>(EMPTY_USAGE)
  const [model, setModelState] = useState('')
  const [models, setModels] = useState<string[]>([])
  const seq = useRef(0)
  const pendingNonces = useRef<Set<string>>(new Set())

  const applyEvents = useCallback((evs: ProtocolEvent[]) => {
    for (const ev of evs) {
      if (ev.seq <= seq.current) continue
      seq.current = Math.max(seq.current, ev.seq)

      if (ev.$kind === 'assistantDelta') {
        if (ev.isReasoning) continue
        setMsgs(p => {
          const last = p[p.length - 1]
          if (last?.role === 'assistant' && last.streaming)
            return [...p.slice(0, -1), { ...last, text: last.text + ev.text }]
          return [...p, { id: `a${ev.seq}`, role: 'assistant', text: ev.text, streaming: true }]
        })
      } else if (ev.$kind === 'assistantMessageCompleted') {
        setMsgs(p => {
          const last = p[p.length - 1]
          if (last?.streaming) {
            const text = ev.message?.content || last.text
            return [...p.slice(0, -1), { ...last, text, streaming: false }]
          }
          const display = messageFromProtocolEvent(ev)
          return display ? [...p, { id: `a${ev.seq}`, ...display }] : p
        })
      } else if (ev.$kind === 'toolCallStarted') {
        const display = messageFromProtocolEvent(ev)
        if (display) setMsgs(p => [...p, { id: `t${ev.call.id}`, ...display }])
      } else if (ev.$kind === 'toolApprovalRequested') {
        const display = messageFromProtocolEvent(ev)
        if (display) setMsgs(p => [...p, { id: `ap${ev.call.id}`, ...display }])
      } else if (ev.$kind === 'toolApprovalResolved') {
        setMsgs(p => p.map(m => m.toolCallId === ev.toolCallId
          ? { ...m, role: 'tool' as Role, text: `${ev.approved ? '✓' : '✗'} ${m.text}` } : m))
      } else if (ev.$kind === 'runFailed') {
        const display = messageFromProtocolEvent(ev)
        if (display) setMsgs(p => [...p, { id: `e${ev.seq}`, ...display }])
      } else if (ev.$kind === 'runInterrupted') {
        const display = messageFromProtocolEvent(ev)
        if (display) setMsgs(p => [...p, { id: `i${ev.seq}`, ...display }])
      } else if (ev.$kind === 'userMessageSubmitted') {
        if (ev.nonce && pendingNonces.current.has(ev.nonce)) {
          pendingNonces.current.delete(ev.nonce)
        } else {
          setMsgs(p => [...p, { id: `u${ev.seq}`, role: 'user', text: ev.text }])
        }
      } else if (ev.$kind === 'usageUpdated') {
        const u = ev as UsageUpdated
        setUsage({
          promptTokens: u.promptTokens,
          completionTokens: u.completionTokens,
          cacheReadTokens: u.cacheReadTokens ?? 0,
          cacheCreationTokens: u.cacheCreationTokens ?? 0,
          costUsd: u.costUsd,
        })
      } else {
        const display = messageFromProtocolEvent(ev)
        if (display) setMsgs(p => [...p, { id: `${ev.$kind}-${ev.seq}`, ...display }])
      }
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    let cancelled = false

    async function connect() {
      let delay = 1000
      while (!cancelled) {
        try {
          const res = await fetch(eventStreamUrl(base, session, seq.current), {
            headers: daemonHeaders(token),
            signal: controller.signal,
          })
          if (!res.ok || !res.body) throw new Error(`stream ${res.status}`)
          setOnline(true)
          delay = 1000
          const reader = res.body.getReader()
          const decoder = new TextDecoder()
          let buffer = ''
          while (!cancelled) {
            const { value, done } = await reader.read()
            if (done) break
            buffer += decoder.decode(value, { stream: true })
            buffer = readSseMessages(buffer, (data) => {
              try { applyEvents([JSON.parse(data) as ProtocolEvent]) } catch { /* ignore */ }
            })
          }
        } catch (err) {
          if (cancelled || (err instanceof DOMException && err.name === 'AbortError')) return
          setOnline(false)
        }
        if (!cancelled) {
          await new Promise(r => setTimeout(r, delay))
          delay = Math.min(delay * 2, 30_000)
        }
      }
    }

    void connect()
    return () => {
      cancelled = true
      controller.abort()
      setOnline(false)
    }
  }, [base, token, session, applyEvents])

  useEffect(() => {
    let cancelled = false
    void fetch(`${base}/settings/orchestration`, {
      headers: daemonHeaders(token),
    })
      .then((res) => res.ok ? res.json() : undefined)
      .then((settings) => {
        if (cancelled || !settings) return
        const modelIds = Array.isArray(settings.models)
          ? settings.models
              .map((entry: { id?: unknown }) => entry.id)
              .filter((id: unknown): id is string => typeof id === 'string' && id.length > 0)
          : []
        setModels(modelIds)
        if (typeof settings.primaryOrchestratorModelId === 'string') {
          setModelState((current) => current || settings.primaryOrchestratorModelId)
        }
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [base, token])

  const send = useCallback(async (text: string, attachments?: Attachment[]) => {
    if (!hasOutgoingPayload(text, attachments?.length ?? 0) || busy) return
    setBusy(true)
    const nonce = crypto.randomUUID()
    pendingNonces.current.add(nonce)
    setMsgs(p => [...p, { id: `u${Date.now()}`, role: 'user', text, nonce, attachments }])
    try {
      await fetch(`${base}/sessions/${session}/messages`, {
        method: 'POST',
        headers: daemonHeaders(token),
        body: JSON.stringify(buildSessionMessageBody(text, nonce, attachments, workspaceRoot)),
      })
    } finally {
      setBusy(false)
    }
  }, [base, token, session, busy, workspaceRoot])

  const resolve = useCallback(async (toolCallId: string, approved: boolean) => {
    await fetch(`${base}/sessions/${session}/tool-approval`, {
      method: 'POST',
      headers: daemonHeaders(token),
      body: JSON.stringify({ toolCallId, approved }),
    })
  }, [base, token, session])

  const clearMsgs = useCallback(() => setMsgs([]), [])
  const appendAssistant = useCallback((text: string, role: Role = 'assistant') => {
    setMsgs(p => [...p, { id: `local-${Date.now()}`, role, text }])
  }, [])

  const setModel = useCallback(async (newModel: string) => {
    setModelState(newModel)
    await fetch(`${base}/sessions/${session}`, {
      method: 'PATCH',
      headers: daemonHeaders(token),
      body: JSON.stringify({ model: newModel }),
    })
  }, [base, token, session])

  const createSession = useCallback(async (request: CreateSessionRequest) => {
    const workspaceRoot = request.workspaceRoot.trim()
    if (!workspaceRoot) throw new Error('workspace required')
    const res = await fetch(`${base}/sessions`, {
      method: 'POST',
      headers: daemonHeaders(token),
      body: JSON.stringify({
        workspaceRoot,
        title: request.title?.trim() || workspaceRoot.split(/[\\/]/).filter(Boolean).at(-1) || 'remote session',
        model: request.model?.trim() || undefined,
        allowedTools: request.allowedTools?.length ? request.allowedTools : undefined,
        disallowedTools: request.disallowedTools?.length ? request.disallowedTools : undefined,
        executionBackend: request.executionBackend?.trim() || undefined,
        effort: request.effort,
        thinkingBudgetTokens: request.thinkingBudgetTokens,
        disableThinking: request.disableThinking,
      }),
    })
    if (!res.ok) throw new Error(`create session failed: ${res.status}`)
    const row = await res.json() as SessionRow
    seq.current = 0
    pendingNonces.current.clear()
    setMsgs([])
    setOnline(false)
    setBusy(false)
    setUsage(EMPTY_USAGE)
    setModelState(row.model || request.model || '')
    setSession(row.id)
  }, [base, token, setSession])

  return (
    <SessionContext.Provider value={{ msgs, online, busy, usage, model, models, setModel, createSession, send, appendAssistant, resolve, clearMsgs }}>
      {children}
    </SessionContext.Provider>
  )
}

export function useSession(): SessionContextValue {
  return useContext(SessionContext)
}
