import { readFile, readdir } from 'node:fs/promises'
import { join } from 'node:path'

const srcRoot = join(process.cwd(), 'src')
const files = []

async function collect(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      await collect(path)
    } else if (/\.(ts|tsx)$/.test(entry.name)) {
      files.push(path)
    }
  }
}

await collect(srcRoot)

const forbidden = [
  'access_token',
  'p.get(\'token\')',
  'p.get("token")',
  'searchParams.set(\'token\'',
  'searchParams.set("token"',
  'new EventSource',
]

for (const file of files) {
  const text = await readFile(file, 'utf8')
  for (const needle of forbidden) {
    if (text.includes(needle)) {
      throw new Error(`${file} contains forbidden URL-token pattern: ${needle}`)
    }
  }
}

console.log('web auth URL safety checks passed')
