import { useState } from 'react'

interface DiffLine {
  type: 'add' | 'remove' | 'context' | 'header'
  content: string
  beforeNum?: number
  afterNum?: number
}

function parseDiff(patch: string): DiffLine[] {
  const lines: DiffLine[] = []
  let beforeLine = 0
  let afterLine = 0

  for (const raw of patch.split('\n')) {
    if (raw.startsWith('---') || raw.startsWith('+++')) {
      lines.push({ type: 'header', content: raw })
      continue
    }
    if (raw.startsWith('@@')) {
      const m = raw.match(/@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/)
      if (m) {
        beforeLine = parseInt(m[1], 10)
        afterLine = parseInt(m[2], 10)
      }
      lines.push({ type: 'header', content: raw })
      continue
    }
    if (raw.startsWith('+')) {
      lines.push({ type: 'add', content: raw.slice(1), afterNum: afterLine++ })
    } else if (raw.startsWith('-')) {
      lines.push({ type: 'remove', content: raw.slice(1), beforeNum: beforeLine++ })
    } else {
      lines.push({ type: 'context', content: raw.startsWith(' ') ? raw.slice(1) : raw, beforeNum: beforeLine++, afterNum: afterLine++ })
    }
  }
  return lines
}

const COLLAPSE_THRESHOLD = 50
const INITIAL_SHOW = 10

interface DiffViewProps {
  patch?: string
  before?: string
  after?: string
}

export function DiffView({ patch, before, after }: DiffViewProps) {
  const [expanded, setExpanded] = useState(false)
  const [copied, setCopied] = useState(false)

  const fullPatch = patch ?? (before !== undefined && after !== undefined
    ? `--- before\n+++ after\n@@ -1,${before.split('\n').length} +1,${after.split('\n').length} @@\n` +
      before.split('\n').map(l => `-${l}`).join('\n') + '\n' +
      after.split('\n').map(l => `+${l}`).join('\n')
    : '')

  const allLines = parseDiff(fullPatch)
  const shouldCollapse = allLines.length > COLLAPSE_THRESHOLD
  const visibleLines = shouldCollapse && !expanded ? allLines.slice(0, INITIAL_SHOW) : allLines
  const hiddenCount = allLines.length - INITIAL_SHOW

  function lineStyle(type: DiffLine['type']): React.CSSProperties {
    switch (type) {
      case 'add': return { backgroundColor: 'rgba(34,197,94,0.12)', color: '#86efac' }
      case 'remove': return { backgroundColor: 'rgba(239,68,68,0.12)', color: '#fca5a5' }
      case 'header': return { backgroundColor: 'rgba(99,102,241,0.12)', color: '#a5b4fc', fontStyle: 'italic' }
      default: return { color: 'var(--text)' }
    }
  }

  function linePrefix(type: DiffLine['type']): string {
    switch (type) {
      case 'add': return '+'
      case 'remove': return '-'
      case 'header': return ' '
      default: return ' '
    }
  }

  return (
    <div style={{ border: '1px solid var(--border)', borderRadius: '8px', overflow: 'hidden', marginTop: '8px', marginBottom: '8px' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', padding: '6px 12px', backgroundColor: 'rgba(0,0,0,0.2)', fontSize: '11px', color: 'var(--muted)' }}>
        <span style={{ textTransform: 'uppercase', letterSpacing: '0.5px', fontWeight: 600 }}>diff</span>
        <button
          onClick={() => { navigator.clipboard.writeText(fullPatch); setCopied(true); setTimeout(() => setCopied(false), 2000) }}
          style={{ padding: '3px 8px', border: '1px solid var(--border)', borderRadius: '4px', backgroundColor: 'transparent', color: 'var(--text)', fontSize: '11px', cursor: 'pointer' }}
        >
          {copied ? 'Copied!' : 'Copy'}
        </button>
      </div>
      <pre style={{ margin: 0, padding: '0', overflow: 'auto', fontFamily: 'var(--mono)', fontSize: '12px', lineHeight: '1.6', maxHeight: '400px' }}>
        {visibleLines.map((line, i) => (
          <div key={i} style={{ display: 'flex', padding: '0 12px', ...lineStyle(line.type) }}>
            <span style={{ width: '32px', flexShrink: 0, color: 'var(--muted)', userSelect: 'none', textAlign: 'right', paddingRight: '8px' }}>
              {line.type === 'add' ? (line.afterNum ?? '') : line.type === 'remove' ? (line.beforeNum ?? '') : line.type !== 'header' ? (line.beforeNum ?? '') : ''}
            </span>
            <span style={{ width: '16px', flexShrink: 0, userSelect: 'none' }}>{linePrefix(line.type)}</span>
            <span style={{ flex: 1, whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>{line.content}</span>
          </div>
        ))}
      </pre>
      {shouldCollapse && !expanded && (
        <button
          onClick={() => setExpanded(true)}
          style={{ width: '100%', padding: '8px', border: 'none', borderTop: '1px solid var(--border)', backgroundColor: 'rgba(0,0,0,0.1)', color: 'var(--muted)', fontSize: '12px', cursor: 'pointer' }}
        >
          Show {hiddenCount} more lines
        </button>
      )}
      {shouldCollapse && expanded && (
        <button
          onClick={() => setExpanded(false)}
          style={{ width: '100%', padding: '8px', border: 'none', borderTop: '1px solid var(--border)', backgroundColor: 'rgba(0,0,0,0.1)', color: 'var(--muted)', fontSize: '12px', cursor: 'pointer' }}
        >
          Collapse
        </button>
      )}
    </div>
  )
}
