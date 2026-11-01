import { useEffect, useRef, useState } from 'react'
import { daemonHeaders, useDaemon } from '../contexts/DaemonContext'
import { useSession, type Attachment } from '../contexts/SessionContext'
import { hostAppName } from "../appName";

// ── Slash commands ────────────────────────────────────────────────────────────

interface CommandInfo { name: string; description: string; usage: string }
interface CommandResult {
  success: boolean
  output: string
  action:
    | 'none'
    | 'clear_chat'
    | 'compact_context'
    | 'list_sessions'
    | 'submit_message'
    | 'conduct_goal'
    | { none: null }
    | { clear_chat: null }
    | { compact_context: null }
    | { switch_session: string }
    | { list_sessions: null }
    | { submit_message: string }
    | { conduct_goal: string }
}

function commandActionKind(action: CommandResult['action']) {
  return typeof action === 'string' ? action : Object.keys(action)[0] ?? 'none'
}

// ── Image helpers ─────────────────────────────────────────────────────────────

const ACCEPTED_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp']

async function fileToAttachment(file: File): Promise<Attachment | null> {
  if (!ACCEPTED_TYPES.includes(file.type)) return null
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const dataUrl = reader.result as string
      const base64 = dataUrl.split(',')[1]
      resolve({ type: 'image', data: base64, mimeType: file.type })
    }
    reader.onerror = reject
    reader.readAsDataURL(file)
  })
}

// ── InputBar ──────────────────────────────────────────────────────────────────

interface InputBarProps {
  onClear: () => void
}

export function InputBar({ onClear }: InputBarProps) {
  const { base, token, session } = useDaemon()
  const { busy, send, appendAssistant } = useSession()
  const [input, setInput] = useState('')
  const [commandPalette, setCommandPalette] = useState<CommandInfo[]>([])
  const [commands, setCommands] = useState<CommandInfo[]>([])
  const [attachments, setAttachments] = useState<Attachment[]>([])
  const fileInputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    let cancelled = false
    fetch(`${base}/commands`, { headers: daemonHeaders(token) })
      .then(res => (res.ok ? res.json() : []))
      .then((rows: CommandInfo[]) => {
        if (!cancelled) setCommands(rows)
      })
      .catch(() => {
        if (!cancelled) setCommands([])
      })
    return () => { cancelled = true }
  }, [base, token])

  async function addFiles(files: FileList | File[]) {
    const results = await Promise.all(Array.from(files).map(fileToAttachment))
    const valid = results.filter((a): a is Attachment => a !== null)
    setAttachments(prev => [...prev, ...valid])
  }

  function removeAttachment(idx: number) {
    setAttachments(prev => prev.filter((_, i) => i !== idx))
  }

  function handleInputChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    const val = e.target.value
    setInput(val)
    if (val.startsWith('/') && val.length > 0 && !val.slice(1).includes(' ')) {
      const partial = val.slice(1).toLowerCase()
      setCommandPalette(commands.filter(c => c.name.toLowerCase().startsWith(partial)).slice(0, 8))
    } else {
      setCommandPalette([])
    }
  }

  async function executeSlashCommand(text: string) {
    const res = await fetch(`${base}/commands/execute`, {
      method: 'POST',
      headers: daemonHeaders(token),
      body: JSON.stringify({ input: text, sessionId: session }),
    })
    if (!res.ok) throw new Error(`command failed: ${res.status}`)
    const result = await res.json() as CommandResult
    const kind = commandActionKind(result.action)
    if (kind === 'clear_chat') onClear()
    if (kind === 'compact_context') {
      await fetch(`${base}/sessions/${session}/compact`, {
        method: 'POST',
        headers: daemonHeaders(token),
      })
    }
    if (kind === 'submit_message' && typeof result.action === 'object' && 'submit_message' in result.action) {
      await send(result.action.submit_message)
    }
    if (kind === 'conduct_goal' && typeof result.action === 'object' && 'conduct_goal' in result.action) {
      await fetch(`${base}/sessions/${session}/orchestrate`, {
        method: 'POST',
        headers: daemonHeaders(token),
        body: JSON.stringify({ goal: result.action.conduct_goal, dryRun: false }),
      })
    }
    if (result.output.trim() && kind !== 'clear_chat' && kind !== 'submit_message') {
      appendAssistant(result.output, result.success ? 'assistant' : 'error')
    }
  }

  async function handlePaste(e: React.ClipboardEvent) {
    const items = Array.from(e.clipboardData.items)
    const imageItems = items.filter(it => ACCEPTED_TYPES.includes(it.type))
    if (imageItems.length === 0) return
    e.preventDefault()
    const files = imageItems.map(it => it.getAsFile()).filter((f): f is File => f !== null)
    await addFiles(files)
  }

  async function submit() {
    const text = input.trim()
    if ((!text && attachments.length === 0) || busy) return
    setInput('')
    setCommandPalette([])
    const atts = [...attachments]
    setAttachments([])

    if (text.startsWith('/') && atts.length === 0) {
      await executeSlashCommand(text)
      return
    }

    await send(text, atts.length ? atts : undefined)
  }

  function onKey(e: React.KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submit() }
  }

  return (
    <div className="compose">
      <div style={{ position: 'relative', flex: 1, display: 'flex', flexDirection: 'column', gap: '8px' }}>
        {attachments.length > 0 && (
          <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
            {attachments.map((att, i) => (
              <div key={i} style={{ position: 'relative' }}>
                <img
                  src={`data:${att.mimeType};base64,${att.data}`}
                  alt="attachment preview"
                  style={{ width: '64px', height: '64px', objectFit: 'cover', borderRadius: '6px', border: '1px solid var(--border)' }}
                />
                <button
                  onClick={() => removeAttachment(i)}
                  style={{ position: 'absolute', top: '-6px', right: '-6px', width: '18px', height: '18px', borderRadius: '50%', border: 'none', backgroundColor: '#ef4444', color: '#fff', fontSize: '11px', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', lineHeight: 1 }}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
        )}
        <div style={{ position: 'relative' }}>
          <textarea
            value={input}
            onChange={handleInputChange}
            onKeyDown={onKey}
            onPaste={handlePaste}
            placeholder={`Message ${hostAppName()}… (Enter to send)`}
            rows={3}
            disabled={busy}
          />
          {commandPalette.length > 0 && (
            <div style={{ position: 'absolute', bottom: '100%', left: 0, right: 0, backgroundColor: 'var(--color-panel-2, #1a1916)', border: '1px solid var(--border)', borderRadius: '6px', boxShadow: '0 4px 12px rgba(0,0,0,0.3)', zIndex: 10, maxHeight: '200px', overflowY: 'auto' }}>
              {commandPalette.map(cmd => (
                <button
                  key={cmd.name}
                  style={{ width: '100%', textAlign: 'left', padding: '8px 12px', border: 'none', backgroundColor: 'transparent', cursor: 'pointer', fontSize: '14px' }}
                  onMouseEnter={e => (e.currentTarget.style.backgroundColor = 'var(--surface)')}
                  onMouseLeave={e => (e.currentTarget.style.backgroundColor = 'transparent')}
                  onFocus={e => (e.currentTarget.style.backgroundColor = 'var(--surface)')}
                  onBlur={e => (e.currentTarget.style.backgroundColor = 'transparent')}
                  onClick={() => { setInput(`/${cmd.name} `); setCommandPalette([]) }}
                >
                  <span style={{ color: 'var(--muted)', fontFamily: 'var(--mono)' }}>/{cmd.name}</span>
                  <span style={{ color: '#999', marginLeft: '8px', fontSize: '12px' }}>{cmd.description}</span>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', alignSelf: 'flex-end' }}>
        <button
          type="button"
          title="Attach image"
          onClick={() => fileInputRef.current?.click()}
          style={{ height: '38px', width: '38px', padding: 0, background: 'transparent', border: '1px solid var(--border)', borderRadius: '8px', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--muted)', flexShrink: 0 }}
        >
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M13.5 10.5V13.5H2.5V10.5" />
            <path d="M8 2v8" />
            <path d="M5.5 4.5L8 2l2.5 2.5" />
          </svg>
        </button>
        <button onClick={submit} disabled={busy || (!input.trim() && attachments.length === 0)}>
          {busy ? '…' : 'Send'}
        </button>
      </div>
      <input
        ref={fileInputRef}
        type="file"
        accept={ACCEPTED_TYPES.join(',')}
        multiple
        style={{ display: 'none' }}
        onChange={async e => {
          if (e.target.files) await addFiles(e.target.files)
          e.target.value = ''
        }}
      />
    </div>
  )
}
