import assert from 'node:assert/strict'
import fs from 'node:fs'
import vm from 'node:vm'
import ts from '@typescript/typescript6'

const sourceUrl = new URL('../../src/contexts/messagePayload.ts', import.meta.url)
const source = fs.readFileSync(sourceUrl, 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText

const module = { exports: {} }
vm.runInNewContext(compiled, { module, exports: module.exports })

const { buildSessionMessageBody } = module.exports

assert.equal(typeof buildSessionMessageBody, 'function')

const attachment = { type: 'image', data: 'ZGF0YQ==', mimeType: 'image/png' }
assert.deepEqual(
  JSON.parse(JSON.stringify(buildSessionMessageBody('hello', 'nonce-1', [], ' D:/Claude/coderight '))),
  {
    text: 'hello',
    nonce: 'nonce-1',
    workspaceRoot: 'D:/Claude/coderight',
  },
)
assert.deepEqual(
  JSON.parse(JSON.stringify(buildSessionMessageBody('', 'nonce-2', [attachment], ''))),
  {
    text: '',
    nonce: 'nonce-2',
    attachments: [attachment],
  },
)
