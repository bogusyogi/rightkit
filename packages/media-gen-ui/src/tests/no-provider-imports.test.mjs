import test from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';

const scanRoots = ['src'];
const forbiddenPatterns = [
  ['tools', 'backends'].join('/'),
  'fal' + 'ai.js',
  'wavespeed' + '.js',
  'nanobanana' + 'api.js',
  'fal' + '-image.js',
  'wavespeed' + '-image.js',
  'evolink' + '-image.js',
  ['FAL', 'KEY'].join('_'),
  ['WAVESPEED', 'API', 'KEY'].join('_'),
  ['NANOBANANAAPI', 'API', 'KEY'].join('_'),
  ['EVOLINK', 'API', 'KEY'].join('_'),
  ['OPEN', 'AI_API_KEY'].join(''),
  ['api', 'openai', 'com'].join('.'),
  ['from ', "'open", "ai'"].join(''),
  ['from ', '"open', 'ai"'].join(''),
  '@' + 'openai/',
  'openai' + '-image'
];

function filesUnder(root) {
  const results = [];
  for (const entry of readdirSync(root)) {
    const fullPath = path.join(root, entry);
    const stats = statSync(fullPath);
    if (stats.isDirectory()) {
      results.push(...filesUnder(fullPath));
    } else {
      results.push(fullPath);
    }
  }
  return results;
}

test('console code does not import or reference provider backends', () => {
  const findings = [];
  for (const root of scanRoots) {
    for (const file of filesUnder(path.resolve(root))) {
      const text = readFileSync(file, 'utf8');
      for (const pattern of forbiddenPatterns) {
        if (text.includes(pattern)) {
          findings.push(`${file}: ${pattern}`);
        }
      }
    }
  }

  assert.deepEqual(findings, []);
});
