import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { Markdown } from '../Markdown'
import { DiffView } from './DiffView'
import { looksLikeDiff } from './diffUtils'
import { useSession, type Msg, type Role } from '../contexts/SessionContext'
import { hostAppName } from "../appName";

// ── Virtual list ──────────────────────────────────────────────────────────────

const ESTIMATED_HEIGHT = 80
const BUFFER_PX = 200

interface VirtualState {
  startIdx: number
  endIdx: number
  offsetTop: number
  totalHeight: number
}

function computeWindow(
  scrollTop: number,
  viewportHeight: number,
  heights: number[],
): VirtualState {
  const total = heights.length
  const totalHeight = heights.reduce((a, b) => a + b, 0)
  let offsetTop = 0
  let startIdx = 0
  let cumulative = 0

  for (let i = 0; i < total; i++) {
    if (cumulative + heights[i] >= scrollTop - BUFFER_PX) {
      startIdx = i
      offsetTop = cumulative
      break
    }
    cumulative += heights[i]
    if (i === total - 1) {
      startIdx = total
      offsetTop = cumulative
    }
  }

  let endIdx = startIdx
  let visible = offsetTop
  while (endIdx < total && visible < scrollTop + viewportHeight + BUFFER_PX) {
    visible += heights[endIdx]
    endIdx++
  }

  return { startIdx, endIdx: Math.min(endIdx, total), offsetTop, totalHeight }
}

// ── Message bubble ────────────────────────────────────────────────────────────

function lbl(r: Role) {
  return { user: 'You', assistant: hostAppName(), tool: '⚙', approval: '⚠', error: '!' }[r]
}

interface BubbleProps {
  msg: Msg
  highlighted: boolean
  currentMatch: boolean
  onResolve: (id: string, approved: boolean) => void
}

function Bubble({ msg, highlighted, currentMatch, onResolve }: BubbleProps) {
  const outline = currentMatch
    ? '2px solid var(--accent)'
    : highlighted
    ? '2px solid var(--accent)'
    : 'none'
  const opacity = highlighted && !currentMatch ? 0.6 : 1

  return (
    <div
      data-msg-id={msg.id}
      className={`bubble ${msg.role}${msg.streaming ? ' streaming' : ''}`}
      style={{ outline, outlineOffset: '-1px', opacity }}
    >
      <span className="lbl">{lbl(msg.role)}</span>
      <span className="body">
        {msg.role === 'assistant' && !msg.streaming ? (
          looksLikeDiff(msg.text) ? <DiffView patch={msg.text} /> : <Markdown text={msg.text} />
        ) : msg.role === 'tool' && looksLikeDiff(msg.text) ? (
          <DiffView patch={msg.text} />
        ) : (
          msg.text
        )}
        {msg.attachments?.map((att, i) =>
          att.type === 'image' ? (
            <img
              key={i}
              src={`data:${att.mimeType};base64,${att.data}`}
              alt="attachment"
              style={{ maxWidth: '240px', maxHeight: '180px', borderRadius: '6px', marginTop: '8px', display: 'block' }}
            />
          ) : null
        )}
      </span>
      {msg.role === 'approval' && msg.toolCallId && (
        <span className="btns">
          <button onClick={() => onResolve(msg.toolCallId!, true)}>Allow</button>
          <button onClick={() => onResolve(msg.toolCallId!, false)}>Deny</button>
        </span>
      )}
    </div>
  )
}

// ── Panel ─────────────────────────────────────────────────────────────────────

interface TranscriptPanelProps {
  searchQuery?: string
  currentMatchIndex?: number
  matchingIds?: string[]
}

export function TranscriptPanel({ searchQuery = '', currentMatchIndex = 0, matchingIds = [] }: TranscriptPanelProps) {
  const { msgs, resolve } = useSession()
  const containerRef = useRef<HTMLDivElement>(null)
  const heightMapRef = useRef<number[]>([])
  const rowRefs = useRef<Map<string, HTMLDivElement>>(new Map())
  const atBottomRef = useRef(true)
  const [virtual, setVirtual] = useState<VirtualState>({ startIdx: 0, endIdx: msgs.length, offsetTop: 0, totalHeight: 0 })

  function recompute() {
    const el = containerRef.current
    if (!el) return
    setVirtual(computeWindow(el.scrollTop, el.clientHeight, [...heightMapRef.current]))
  }

  // Keep heightMap sized to msgs
  useLayoutEffect(() => {
    while (heightMapRef.current.length < msgs.length) {
      heightMapRef.current.push(ESTIMATED_HEIGHT)
    }
    heightMapRef.current.length = msgs.length
  }, [msgs.length])

  // Measure rendered rows and update height estimates
  useLayoutEffect(() => {
    let changed = false
    for (const [id, el] of rowRefs.current) {
      const idx = msgs.findIndex(m => m.id === id)
      if (idx === -1) continue
      const h = el.getBoundingClientRect().height
      if (h > 0 && heightMapRef.current[idx] !== h) {
        heightMapRef.current[idx] = h
        changed = true
      }
    }
    if (changed) recompute()
  })

  // Recompute on scroll
  function onScroll() {
    const el = containerRef.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40
    atBottomRef.current = atBottom
    recompute()
  }

  // Snap to bottom on new messages if we were at the bottom
  useEffect(() => {
    const el = containerRef.current
    if (!el || !atBottomRef.current) return
    el.scrollTop = el.scrollHeight
  }, [msgs.length])

  // Scroll to current search match
  useEffect(() => {
    if (!searchQuery || !matchingIds.length) return
    const id = matchingIds[currentMatchIndex]
    const el = document.querySelector(`[data-msg-id="${id}"]`)
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'center' })
  }, [currentMatchIndex, searchQuery, matchingIds])

  const { startIdx, endIdx, offsetTop, totalHeight } = virtual
  const visibleMsgs = msgs.slice(startIdx, endIdx)

  return (
    <div
      ref={containerRef}
      className="feed"
      onScroll={onScroll}
      style={{ position: 'relative' }}
    >
      {msgs.length === 0 && <div className="empty">Send a message to start.</div>}
      <div style={{ height: totalHeight, position: 'relative', width: '100%' }}>
        <div style={{ position: 'absolute', top: offsetTop, left: 0, right: 0 }}>
          {visibleMsgs.map((m) => {
            const isMatch = !!(searchQuery && m.text.toLowerCase().includes(searchQuery.toLowerCase()))
            const isCurrent = isMatch && matchingIds[currentMatchIndex] === m.id
            return (
              <div
                key={m.id}
                ref={el => {
                  if (el) rowRefs.current.set(m.id, el)
                  else rowRefs.current.delete(m.id)
                }}
              >
                <Bubble
                  msg={m}
                  highlighted={isMatch}
                  currentMatch={isCurrent}
                  onResolve={resolve}
                />
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
