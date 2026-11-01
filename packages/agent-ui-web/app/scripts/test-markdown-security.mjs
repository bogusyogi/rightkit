import { pathToFileURL } from 'node:url'
import { readFile, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import ts from '@typescript/typescript6'

const source = await readFile(new URL('../../src/markdownSecurity.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText

const tempModule = join(process.cwd(), `.tmp-markdown-security-${process.pid}.mjs`)

try {
  await writeFile(tempModule, compiled)
  const { safeLinkHref } = await import(pathToFileURL(tempModule).href)

  const allowed = [
    ['https://example.com/path?q=1', 'https://example.com/path?q=1'],
    ['http://localhost:5173', 'http://localhost:5173'],
    ['mailto:security@example.com', 'mailto:security@example.com'],
    ['  HTTPS://EXAMPLE.COM  ', 'HTTPS://EXAMPLE.COM'],
  ]

  for (const [input, expected] of allowed) {
    const actual = safeLinkHref(input)
    if (actual !== expected) {
      throw new Error(`Expected ${input} to sanitize to ${expected}, got ${actual}`)
    }
  }

  const rejected = [
    'javascript:alert(1)',
    'data:text/html,<script>alert(1)</script>',
    'file:///C:/Users/me/.ssh/id_rsa',
    '/relative/path',
    'https:javascript:alert(1)',
    'https://example.com/\njavascript:alert(1)',
    '',
  ]

  for (const input of rejected) {
    const actual = safeLinkHref(input)
    if (actual !== undefined) {
      throw new Error(`Expected ${input} to be rejected, got ${actual}`)
    }
  }

  console.log('markdown link sanitization checks passed')
} finally {
  await rm(tempModule, { force: true })
}
