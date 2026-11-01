import { useMemo, useState, createElement } from 'react'
import { safeLinkHref } from './markdownSecurity.js'

function escapeHtml(text: string): string {
  const map: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#039;' }
  return text.replace(/[&<>"']/g, c => map[c])
}

interface BlockToken {
  type: 'code' | 'heading' | 'list' | 'blockquote' | 'paragraph' | 'table'
  content: string
  lang?: string
  level?: number
  ordered?: boolean
  table?: TableToken
}

type TableAlignment = 'left' | 'center' | 'right' | null

interface TableToken {
  headers: string[]
  alignments: TableAlignment[]
  rows: string[][]
}

function removeTrailingTablePipe(value: string): string {
  let backslashes = 0
  for (let index = value.length - 2; index >= 0 && value[index] === '\\'; index--) backslashes++
  if (value.endsWith('|') && backslashes % 2 === 0) return value.slice(0, -1)
  return value
}

function splitTableCells(line: string): string[] {
  let source = line.trim()
  if (source.startsWith('|')) source = source.slice(1)
  source = removeTrailingTablePipe(source)

  const cells: string[] = []
  let current = ''
  for (let index = 0; index < source.length; index++) {
    const char = source[index]
    if (char === '\\' && source[index + 1] === '|') {
      current += '|'
      index++
      continue
    }
    if (char === '|') {
      cells.push(current.trim())
      current = ''
      continue
    }
    current += char
  }
  cells.push(current.trim())
  return cells
}

function parseTableAlignments(cells: string[]): TableAlignment[] | null {
  if (cells.length < 2) return null
  const alignments: TableAlignment[] = []
  for (const cell of cells) {
    const marker = cell.replace(/\s+/g, '')
    if (!/^:?-{3,}:?$/.test(marker)) return null
    const left = marker.startsWith(':')
    const right = marker.endsWith(':')
    alignments.push(left && right ? 'center' : right ? 'right' : left ? 'left' : null)
  }
  return alignments
}

function normalizeTableRow(cells: string[], width: number): string[] {
  const next = cells.slice(0, width)
  while (next.length < width) next.push('')
  return next
}

function readLine(text: string, index: number): { line: string; next: number } {
  const lineEnd = text.indexOf('\n', index)
  if (lineEnd === -1) return { line: text.slice(index), next: text.length }
  return { line: text.slice(index, lineEnd), next: lineEnd + 1 }
}

function parseTableAt(text: string, index: number): { table: TableToken; next: number } | null {
  const header = readLine(text, index)
  const separator = readLine(text, header.next)
  const headerLine = header.line.trimEnd()
  const separatorLine = separator.line.trimEnd()
  if (!headerLine.includes('|') || !separatorLine.includes('|')) return null

  const headers = splitTableCells(headerLine)
  const alignments = parseTableAlignments(splitTableCells(separatorLine))
  if (!alignments || headers.length < 2 || alignments.length !== headers.length) return null

  const rows: string[][] = []
  let next = separator.next
  while (next < text.length) {
    const current = readLine(text, next)
    const line = current.line.trimEnd()
    if (!line.trim() || !line.includes('|')) break
    const cells = splitTableCells(line)
    if (cells.length < 2) break
    rows.push(normalizeTableRow(cells, headers.length))
    next = current.next
  }

  return {
    table: { headers: normalizeTableRow(headers, headers.length), alignments, rows },
    next,
  }
}

function alignmentClass(alignment: TableAlignment): string {
  if (alignment === 'center') return 'md-align-center'
  if (alignment === 'right') return 'md-align-right'
  return 'md-align-left'
}

function parseBlocks(text: string): BlockToken[] {
  const blocks: BlockToken[] = []
  let i = 0
  while (i < text.length) {
    while (i < text.length && text[i] === '\n') i++
    if (i >= text.length) break

    if (text[i] === '`' && text[i + 1] === '`' && text[i + 2] === '`') {
      let lang = ''
      let j = i + 3
      while (j < text.length && text[j] !== '\n') { lang += text[j]; j++ }
      j++
      let code = ''
      while (j < text.length) {
        if (text[j] === '`' && text[j + 1] === '`' && text[j + 2] === '`') {
          blocks.push({ type: 'code', content: code, lang: lang.trim() })
          i = j + 3
          break
        }
        code += text[j]; j++
      }
      if (j >= text.length) { blocks.push({ type: 'code', content: code, lang: lang.trim() }); i = j }
      continue
    }

    const headingMatch = text.slice(i).match(/^(#{1,6})\s+(.+?)(?:\n|$)/)
    if (headingMatch) {
      blocks.push({ type: 'heading', content: headingMatch[2], level: headingMatch[1].length })
      i += headingMatch[0].length
      continue
    }

    const table = parseTableAt(text, i)
    if (table) {
      blocks.push({ type: 'table', content: '', table: table.table })
      i = table.next
      continue
    }

    if (text[i] === '>') {
      let quote = ''
      while (i < text.length && text[i] === '>') {
        let lineEnd = text.indexOf('\n', i)
        if (lineEnd === -1) lineEnd = text.length
        quote += text.slice(i + 1, lineEnd).trim() + '\n'
        i = lineEnd + 1
      }
      blocks.push({ type: 'blockquote', content: quote.trim() })
      continue
    }

    if ((text[i] === '-' || text[i] === '*') && text[i + 1] === ' ') {
      let list = ''
      while (i < text.length && (text[i] === '-' || text[i] === '*') && text[i + 1] === ' ') {
        let lineEnd = text.indexOf('\n', i)
        if (lineEnd === -1) lineEnd = text.length
        list += text.slice(i + 2, lineEnd) + '\n'
        i = lineEnd + 1
      }
      blocks.push({ type: 'list', content: list.trim(), ordered: false })
      continue
    }

    const orderedListMatch = text.slice(i).match(/^(\d+)\.\s+(.+?)(?:\n|$)/)
    if (orderedListMatch) {
      let list = ''
      while (i < text.length) {
        const numMatch = text.slice(i).match(/^(\d+)\.\s+(.+?)(?:\n|$)/)
        if (!numMatch) break
        list += numMatch[2] + '\n'
        i += numMatch[0].length
      }
      blocks.push({ type: 'list', content: list.trim(), ordered: true })
      continue
    }

    let para = ''
    while (i < text.length && text[i] !== '\n') { para += text[i]; i++ }
    if (para.trim()) blocks.push({ type: 'paragraph', content: para.trim() })
    i++
  }
  return blocks
}

interface InlineToken { type: 'text' | 'bold' | 'italic' | 'code' | 'link'; content: string; href?: string }

function parseInline(text: string): InlineToken[] {
  const tokens: InlineToken[] = []
  let i = 0
  while (i < text.length) {
    if (text[i] === '[') {
      const closeIdx = text.indexOf(']', i)
      if (closeIdx !== -1 && text[closeIdx + 1] === '(') {
        const parenEnd = text.indexOf(')', closeIdx)
        if (parenEnd !== -1) {
          tokens.push({ type: 'link', content: text.slice(i + 1, closeIdx), href: text.slice(closeIdx + 2, parenEnd) })
          i = parenEnd + 1; continue
        }
      }
    }
    if (text[i] === '*' && text[i + 1] === '*') {
      const end = text.indexOf('**', i + 2)
      if (end !== -1) { tokens.push({ type: 'bold', content: text.slice(i + 2, end) }); i = end + 2; continue }
    }
    if ((text[i] === '*' || text[i] === '_') && text[i + 1] !== text[i]) {
      const quote = text[i]
      const end = text.indexOf(quote, i + 1)
      if (end !== -1 && text[end - 1] !== '\\') { tokens.push({ type: 'italic', content: text.slice(i + 1, end) }); i = end + 1; continue }
    }
    if (text[i] === '`' && text[i + 1] !== '`') {
      const end = text.indexOf('`', i + 1)
      if (end !== -1) { tokens.push({ type: 'code', content: text.slice(i + 1, end) }); i = end + 1; continue }
    }
    let plain = ''
    while (i < text.length && text[i] !== '*' && text[i] !== '_' && text[i] !== '[' && text[i] !== '`') { plain += text[i]; i++ }
    if (plain) tokens.push({ type: 'text', content: plain })
  }
  return tokens
}

function renderInline(tokens: InlineToken[]): React.ReactNode[] {
  return tokens.map((token, idx) => {
    switch (token.type) {
      case 'bold': return <strong key={idx}>{renderInline(parseInline(token.content))}</strong>
      case 'italic': return <em key={idx}>{renderInline(parseInline(token.content))}</em>
      case 'code': return <code key={idx} className="md-code-inline">{escapeHtml(token.content)}</code>
      case 'link': {
        const href = safeLinkHref(token.href)
        if (!href) return <span key={idx}>{escapeHtml(token.content)}</span>
        return <a key={idx} className="md-link" href={href} target="_blank" rel="noreferrer">{escapeHtml(token.content)}</a>
      }
      case 'text': return escapeHtml(token.content)
      default: return null
    }
  })
}

function CodeBlock({ code, lang }: { code: string; lang: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="md-code-block">
      <div className="md-code-header">
        <span>{lang || 'code'}</span>
        <button
          className="md-code-copy"
          onClick={() => { navigator.clipboard.writeText(code); setCopied(true); setTimeout(() => setCopied(false), 2000) }}
        >
          {copied ? 'Copied!' : 'Copy'}
        </button>
      </div>
      <pre className="md-code-body">
        <code>{escapeHtml(code)}</code>
      </pre>
    </div>
  )
}

export function Markdown({ text }: { text: string }) {
  const blocks = useMemo(() => parseBlocks(text), [text])
  return (
    <div className="md md-block">
      {blocks.map((block, idx) => {
        switch (block.type) {
          case 'code': return <CodeBlock key={idx} code={block.content} lang={block.lang || ''} />
          case 'heading': return createElement(`h${block.level}` as 'h1', { key: idx, className: `md md-h${block.level}` }, renderInline(parseInline(block.content)))
          case 'blockquote': return <blockquote key={idx} className="md">{renderInline(parseInline(block.content))}</blockquote>
          case 'list': {
            const items = block.content.split('\n').filter(l => l.trim())
            const ListTag = block.ordered ? 'ol' : 'ul'
            return <ListTag key={idx} className="md">{items.map((item, ii) => <li key={ii}>{renderInline(parseInline(item))}</li>)}</ListTag>
          }
          case 'table': {
            const table = block.table
            if (!table) return null
            return (
              <div key={idx} className="md-table-wrap">
                <table className="md-table">
                  <thead>
                    <tr>
                      {table.headers.map((header, headerIdx) => (
                        <th key={`${headerIdx}-${header}`} className={alignmentClass(table.alignments[headerIdx])}>
                          {renderInline(parseInline(header))}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {table.rows.map((row, rowIdx) => (
                      <tr key={`${rowIdx}-${row.join('|')}`}>
                        {row.map((cell, cellIdx) => (
                          <td key={`${rowIdx}-${cellIdx}-${cell}`} className={alignmentClass(table.alignments[cellIdx])}>
                            {renderInline(parseInline(cell))}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )
          }
          case 'paragraph': return <p key={idx} className="md">{renderInline(parseInline(block.content))}</p>
          default: return null
        }
      })}
    </div>
  )
}
