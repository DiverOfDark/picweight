#!/usr/bin/env node
/**
 * Fail if `src/lib/generated/` has drifted from what the current spec produces.
 *
 * Deliberately does NOT shell out to `git diff`: generated files can be
 * untracked (a fresh one, or a clean checkout that has not committed them yet)
 * and `git diff` silently ignores untracked paths — a guard that passes when
 * the file is missing entirely is worse than no guard.
 *
 * Instead: snapshot the committed bytes, regenerate into a scratch directory,
 * compare, and always restore. Never leaves the tree modified.
 */

import { execFileSync } from 'node:child_process'
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const GENERATED = resolve(here, '../src/lib/generated')

/** Every file under `root`, as sorted paths relative to it. */
function walk(root, prefix = '') {
  const out = []
  for (const entry of readdirSync(join(root, prefix), { withFileTypes: true })) {
    const rel = prefix ? join(prefix, entry.name) : entry.name
    if (entry.isDirectory()) out.push(...walk(root, rel))
    else out.push(rel)
  }
  return out.sort()
}

const snapshot = mkdtempSync(join(tmpdir(), 'picweight-generated-'))
const hadOutput = existsSync(GENERATED)
if (hadOutput) cpSync(GENERATED, snapshot, { recursive: true })

let failure = null
try {
  execFileSync('node', [resolve(here, 'generate-api.mjs')], { stdio: 'inherit' })

  // The SDK nests (sdk/client, sdk/core), so walk rather than readdir once —
  // a flat listing would call readFileSync on a directory and crash.
  const before = hadOutput ? walk(snapshot) : []
  const after = walk(GENERATED)

  if (before.join('\n') !== after.join('\n')) {
    const added = after.filter((f) => !before.includes(f))
    const removed = before.filter((f) => !after.includes(f))
    failure =
      'the set of generated files differs.' +
      (added.length ? `\n  missing from the commit: ${added.join(', ')}` : '') +
      (removed.length ? `\n  no longer generated: ${removed.join(', ')}` : '')
  } else {
    for (const name of after) {
      if (readFileSync(join(snapshot, name), 'utf8') !== readFileSync(join(GENERATED, name), 'utf8')) {
        failure = `${name} differs from what android/openapi.json produces.`
        break
      }
    }
  }
} finally {
  // Restore whatever was committed, pass or fail — a check must not mutate.
  if (hadOutput) {
    rmSync(GENERATED, { recursive: true, force: true })
    cpSync(snapshot, GENERATED, { recursive: true })
  }
  rmSync(snapshot, { recursive: true, force: true })
}

if (failure) {
  console.error(`\n✗ src/lib/generated is stale: ${failure}`)
  console.error('  Run `npm run generate` and commit the result.')
  console.error('  If the spec itself looks wrong, fix the Rust type — the spec is generated too.\n')
  process.exit(1)
}

console.log('\n✓ src/lib/generated matches android/openapi.json')
