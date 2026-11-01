import assert from 'node:assert/strict'
import fs from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'

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

const { hasOutgoingPayload } = module.exports

assert.equal(typeof hasOutgoingPayload, 'function')
assert.equal(hasOutgoingPayload('', 0), false)
assert.equal(hasOutgoingPayload('   ', 0), false)
assert.equal(hasOutgoingPayload('hello', 0), true)
assert.equal(hasOutgoingPayload('   ', 1), true)
assert.equal(hasOutgoingPayload('', 2), true)
