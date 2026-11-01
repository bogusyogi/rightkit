import { useCallback, useEffect, useRef, useState } from 'react'
import './App.css'
import { DaemonProvider, daemonHeaders, useDaemon } from './contexts/DaemonContext'
import { type SessionEffort, SessionProvider, useSession } from './contexts/SessionContext'
import { TranscriptPanel } from './components/TranscriptPanel'
import { InputBar } from './components/InputBar'
import { ModelSelector } from './components/ModelSelector'
import { UsagePanel } from './components/UsagePanel'
import { hostAppName } from "./appName";

// ── Search bar ────────────────────────────────────────────────────────────────

interface SearchBarProps {
  query: string
  onChange: (q: string) => void
  matchCount: number
  matchIndex: number
  onPrev: () => void
  onNext: () => void
}

function SearchBar({ query, onChange, matchCount, matchIndex, onPrev, onNext }: SearchBarProps) {
  const ref = useRef<HTMLInputElement>(null)

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.ctrlKey || e.metaKey) && e.key === 'f') { e.preventDefault(); ref.current?.focus() }
      if (e.key === 'Escape' && query) onChange('')
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [query, onChange])

  return (
    <div className="search-bar">
      <input
        ref={ref}
        type="text"
        placeholder="Search (⌘F)"
        value={query}
        onChange={e => { onChange(e.target.value) }}
      />
      {query && matchCount > 0 && (
        <>
          <button onClick={onPrev}>↑</button>
          <button onClick={onNext}>↓</button>
          <span className="count">{matchIndex + 1} / {matchCount}</span>
        </>
      )}
    </div>
  )
}

interface NewSessionDraft {
  workspaceRoot: string
  title: string
  model: string
  effort: '' | SessionEffort
  allowedTools: string
  disallowedTools: string
}

function NewSessionPanel({
  draft,
  models,
  pending,
  onDraft,
  onCreate,
}: {
  draft: NewSessionDraft
  models: string[]
  pending: boolean
  onDraft: (draft: NewSessionDraft) => void
  onCreate: () => void
}) {
  return (
    <form
      className="new-session-panel"
      onSubmit={(event) => {
        event.preventDefault()
        onCreate()
      }}
    >
      <input
        type="text"
        placeholder="workspace path"
        value={draft.workspaceRoot}
        onChange={(event) => onDraft({ ...draft, workspaceRoot: event.target.value })}
      />
      <input
        type="text"
        placeholder="title"
        value={draft.title}
        onChange={(event) => onDraft({ ...draft, title: event.target.value })}
      />
      <select
        value={draft.model}
        onChange={(event) => onDraft({ ...draft, model: event.target.value })}
      >
        <option value="">Default model</option>
        {models.map((model) => (
          <option key={model} value={model}>{model}</option>
        ))}
      </select>
      <select
        value={draft.effort}
        onChange={(event) => onDraft({ ...draft, effort: event.target.value as NewSessionDraft['effort'] })}
      >
        <option value="">Default effort</option>
        <option value="low">Low</option>
        <option value="medium">Medium</option>
        <option value="high">High</option>
      </select>
      <input
        type="text"
        placeholder="allow tools"
        value={draft.allowedTools}
        onChange={(event) => onDraft({ ...draft, allowedTools: event.target.value })}
      />
      <input
        type="text"
        placeholder="deny tools"
        value={draft.disallowedTools}
        onChange={(event) => onDraft({ ...draft, disallowedTools: event.target.value })}
      />
      <button type="submit" disabled={pending || !draft.workspaceRoot.trim()}>
        {pending ? 'Creating' : 'Create'}
      </button>
    </form>
  )
}

interface TelegramStatus {
  state?: string
  tokenConfigured?: boolean
  allowedUserCount?: number
  longPollRequested?: boolean
  bindingCount?: number
  runtime?: {
    lastPollAt?: string
    lastSuccessAt?: string
    lastError?: string
    updateOffset?: number
    activeChatCount?: number
  }
}

interface TelegramTest {
  verdict?: string
  checks?: Array<{ id: string; label: string; status: string; detail: string; fix?: string }>
  notes?: string[]
}

interface TelegramUsers {
  verdict?: string
  userCount?: number
  users?: Array<{
    userId: number
    username?: string | null
    firstName?: string | null
    lastName?: string | null
    chatId?: number | null
    chatType?: string | null
    lastUpdateId?: number
  }>
  detail?: string
  error?: string
  notes?: string[]
}

function RemotePanel({ onClose }: { onClose: () => void }) {
  const { base, token } = useDaemon()
  const [status, setStatus] = useState<TelegramStatus | null>(null)
  const [test, setTest] = useState<TelegramTest | null>(null)
  const [users, setUsers] = useState<TelegramUsers | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')

  const refresh = useCallback(async () => {
    setLoading(true)
    setError('')
    try {
      const next = await fetchTelegramRemote(base, token)
      setStatus(next.status)
      setTest(next.test)
      setUsers(next.users)
      setError(next.errors.join(' · '))
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [base, token])

  useEffect(() => {
    let cancelled = false
    void fetchTelegramRemote(base, token)
      .then((next) => {
        if (cancelled) return
        setStatus(next.status)
        setTest(next.test)
        setUsers(next.users)
        setError(next.errors.join(' · '))
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
    return () => {
      cancelled = true
    }
  }, [base, token])

  return (
    <section className="remote-panel">
      <div className="remote-panel-header">
        <div>
          <h2>Remote access</h2>
          <p>Telegram readiness is secret-redacted.</p>
        </div>
        <div className="remote-panel-actions">
          <button type="button" onClick={() => void refresh()} disabled={loading}>
            {loading ? 'Checking' : 'Refresh'}
          </button>
          <button type="button" onClick={onClose}>Close</button>
        </div>
      </div>
      {error ? <div className="remote-error">{error}</div> : null}
      <div className="remote-setup-flow" aria-label="Telegram setup sequence">
        {[
          ['1', 'BotFather token', 'Create a bot, then save the token in Settings > Remote or with coderight remote setup.'],
          ['2', 'Message bot', `Send one Telegram message from the account that should control ${hostAppName()}.`],
          ['3', 'Discover id', 'Refresh this panel or run coderight remote users, then copy the numeric user id.'],
          ['4', 'Smoke live', 'Allowlist the id, restart the daemon, then run coderight remote smoke while messaging the bot.'],
        ].map(([step, title, detail]) => (
          <div className="remote-setup-step" key={step}>
            <span>{step}</span>
            <div>
              <strong>{title}</strong>
              <p>{detail}</p>
            </div>
          </div>
        ))}
      </div>
      <div className="remote-grid">
        <RemoteMetric label="Bot" value={labelStatus(status?.state)} status={status?.tokenConfigured ? 'pass' : 'warn'} />
        <RemoteMetric label="Allowed users" value={String(status?.allowedUserCount ?? 0)} status={(status?.allowedUserCount ?? 0) > 0 ? 'pass' : 'warn'} />
        <RemoteMetric label="Long poll" value={status?.longPollRequested ? 'requested' : 'not requested'} status={status?.longPollRequested ? 'pass' : 'warn'} />
        <RemoteMetric label="Bindings" value={`${status?.bindingCount ?? 0} bound`} status={(status?.bindingCount ?? 0) > 0 ? 'pass' : 'warn'} />
        <RemoteMetric label="Chats" value={String(status?.runtime?.activeChatCount ?? 0)} status={(status?.runtime?.activeChatCount ?? 0) > 0 ? 'pass' : 'warn'} />
        <RemoteMetric label="Readiness" value={test?.verdict ?? 'unknown'} status={test?.verdict ?? 'warn'} />
      </div>
      {status?.runtime?.lastError ? (
        <div className="remote-error">{status.runtime.lastError}</div>
      ) : null}
      <div className="remote-checks">
        {(test?.checks ?? []).map((check) => (
          <div className="remote-check" key={check.id}>
            <span className={`remote-pill ${check.status}`}>{check.status}</span>
            <div>
              <strong>{check.label}</strong>
              <p>{check.detail}</p>
              {check.fix ? <p className="remote-fix">{check.fix}</p> : null}
            </div>
          </div>
        ))}
      </div>
      <div className="remote-users">
        <div className="remote-users-header">
          <strong>Recent Telegram users</strong>
          <span>{users?.verdict ?? 'unknown'} · {users?.userCount ?? 0}</span>
        </div>
        {users?.detail || users?.error ? (
          <p className="remote-fix">{users.detail ?? users.error}</p>
        ) : null}
        {users?.users?.length ? (
          <div className="remote-user-list">
            {users.users.map((user) => (
              <div className="remote-user-row" key={`${user.userId}-${user.lastUpdateId ?? 0}`}>
                <code>{user.userId}</code>
                <span>
                  {[user.username ? `@${user.username}` : null, user.firstName, user.lastName]
                    .filter(Boolean)
                    .join(' ') || 'Telegram user'}
                </span>
                <small>{user.chatType ?? 'chat'} {user.chatId ?? ''}</small>
              </div>
            ))}
          </div>
        ) : (
          <p>Message the bot once, then refresh to discover the numeric user id.</p>
        )}
      </div>
    </section>
  )
}

async function fetchTelegramRemote(base: string, token: string) {
  const [status, test, users] = await Promise.all([
    fetchTelegramEndpoint<TelegramStatus>(`${base}/remote/telegram/status`, token, 'telegram status'),
    fetchTelegramEndpoint<TelegramTest>(`${base}/remote/telegram/test`, token, 'telegram test'),
    fetchTelegramEndpoint<TelegramUsers>(`${base}/remote/telegram/users?limit=20`, token, 'telegram users'),
  ])
  return {
    status: status.value,
    test: test.value,
    users: users.value,
    errors: [status.error, test.error, users.error].filter(Boolean) as string[],
  }
}

async function fetchTelegramEndpoint<T>(url: string, token: string, label: string): Promise<{ value: T | null; error?: string }> {
  try {
    const res = await fetch(url, { headers: daemonHeaders(token) })
    if (!res.ok) return { value: null, error: `${label} failed: ${res.status}` }
    return { value: await res.json() as T }
  } catch (err) {
    return { value: null, error: `${label} failed: ${err instanceof Error ? err.message : String(err)}` }
  }
}

function RemoteMetric({ label, value, status }: { label: string; value: string; status?: string }) {
  return (
    <div className="remote-metric">
      <span>{label}</span>
      <strong className={status === 'pass' ? 'ok' : status === 'block' ? 'bad' : 'warn'}>{value}</strong>
    </div>
  )
}

function labelStatus(value?: string) {
  if (!value) return 'unknown'
  return value.replace(/_/g, ' ')
}

// ── Shell (inner — has access to session context) ─────────────────────────────

function Shell() {
  const { msgs, online, clearMsgs, usage, model, models, setModel, createSession } = useSession()
  const [searchQuery, setSearchQuery] = useState('')
  const [matchIndex, setMatchIndex] = useState(0)
  const [showNewSession, setShowNewSession] = useState(false)
  const [showRemote, setShowRemote] = useState(false)
  const [newSessionPending, setNewSessionPending] = useState(false)
  const [newSessionDraft, setNewSessionDraft] = useState<NewSessionDraft>(() => {
    const params = new URLSearchParams(window.location.search)
    return {
      workspaceRoot: params.get('workspace') ?? '',
      title: '',
      model: '',
      effort: '',
      allowedTools: '',
      disallowedTools: '',
    }
  })

  const matchingIds = searchQuery
    ? msgs.filter(m => m.text.toLowerCase().includes(searchQuery.toLowerCase())).map(m => m.id)
    : []

  function handleSearchChange(q: string) {
    setSearchQuery(q)
    setMatchIndex(0)
  }

  async function handleCreateSession() {
    setNewSessionPending(true)
    try {
      await createSession({
        workspaceRoot: newSessionDraft.workspaceRoot,
        title: newSessionDraft.title,
        model: newSessionDraft.model || undefined,
        allowedTools: parseToolList(newSessionDraft.allowedTools),
        disallowedTools: parseToolList(newSessionDraft.disallowedTools),
        effort: newSessionDraft.effort || undefined,
      })
      setShowNewSession(false)
    } finally {
      setNewSessionPending(false)
    }
  }

  return (
    <div className="shell">
      <div className="topbar topbar-spread">
        <div className="topbar-group">
          <span className="brand">{hostAppName()}</span>
          <span className={`dot ${online ? 'on' : 'off'}`} title={online ? 'connected' : 'disconnected'} />
        </div>
        <div className="topbar-group topbar-group-right">
          <UsagePanel usage={usage} />
          <ModelSelector currentModel={model} options={models} onModelChange={setModel} />
          <button
            className="topbar-button"
            type="button"
            onClick={() => setShowNewSession((value) => !value)}
          >
            New session
          </button>
          <button
            className="topbar-button"
            type="button"
            onClick={() => setShowRemote((value) => !value)}
          >
            Remote
          </button>
          <SearchBar
            query={searchQuery}
            onChange={handleSearchChange}
            matchCount={matchingIds.length}
            matchIndex={matchIndex}
            onPrev={() => setMatchIndex(i => (i - 1 + matchingIds.length) % matchingIds.length)}
            onNext={() => setMatchIndex(i => (i + 1) % matchingIds.length)}
          />
        </div>
      </div>
      {showNewSession ? (
        <NewSessionPanel
          draft={newSessionDraft}
          models={models}
          pending={newSessionPending}
          onDraft={setNewSessionDraft}
          onCreate={() => void handleCreateSession()}
        />
      ) : null}
      {showRemote ? <RemotePanel onClose={() => setShowRemote(false)} /> : null}
      <TranscriptPanel
        searchQuery={searchQuery}
        currentMatchIndex={matchIndex}
        matchingIds={matchingIds}
      />
      <InputBar onClear={clearMsgs} />
    </div>
  )
}

function parseToolList(value: string) {
  return value
    .split(/[\s,]+/)
    .map((tool) => tool.trim())
    .filter(Boolean)
}

// ── App root ──────────────────────────────────────────────────────────────────

export default function App() {
  return (
    <DaemonProvider>
      <SessionProvider>
        <Shell />
      </SessionProvider>
    </DaemonProvider>
  )
}
