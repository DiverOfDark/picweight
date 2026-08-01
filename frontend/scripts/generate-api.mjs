#!/usr/bin/env node
/**
 * Generate the web client's contract layer from `android/openapi.json`.
 *
 * Why this exists: three separate production bugs in one day all came from
 * hand-typing values the API had already declared. The worst was
 * `Sex: ["Male","Female"]` — and the frontend was not even careless, it copied
 * what the OpenAPI document said. The document itself was wrong, because
 * `#[serde(rename)]` set the wire format while utoipa published the raw Rust
 * identifiers. Backend `enum_wire_values_match_the_openapi_schema` now makes
 * the document truthful; this script makes the web client inherit it instead
 * of re-typing it.
 *
 * Emits, into `src/lib/generated/` (never edit those files):
 *   - `enums.js`     runtime constants — the part TypeScript types cannot give
 *                    us, and the part every one of the bugs needed
 *   - `schema.d.ts`  full request/response types via openapi-typescript, for
 *                    editor completion and `checkJs`
 *
 * Run `npm run generate`. `npm run build` runs it first, and CI fails if the
 * committed output differs from what the current spec produces.
 */

import { execFileSync } from 'node:child_process'
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const SPEC = resolve(here, '../../android/openapi.json')
const OUT = resolve(here, '../src/lib/generated')

const BANNER = `/**
 * GENERATED FROM android/openapi.json — DO NOT EDIT.
 *
 * Regenerate with \`npm run generate\`. If a value here looks wrong, the API is
 * wrong: fix the Rust type and the spec regenerates. Hand-editing this file
 * recreates the class of bug it exists to prevent.
 */`

/** `MealStatus` -> `MEAL_STATUS`, `Sex` -> `SEX`. */
const constName = (schema) =>
  schema
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
    .replace(/[^A-Za-z0-9]+/g, '_')
    .toUpperCase()

/** `needs_review` -> `NEEDS_REVIEW`. */
const memberName = (value) => value.replace(/[^A-Za-z0-9]+/g, '_').toUpperCase()

const spec = JSON.parse(readFileSync(SPEC, 'utf8'))
const schemas = spec.components?.schemas ?? {}

const stringEnums = Object.entries(schemas)
  .filter(([, s]) => Array.isArray(s?.enum) && s.enum.every((v) => typeof v === 'string'))
  .sort(([a], [b]) => a.localeCompare(b))

if (!stringEnums.length) {
  console.error('No string enums found in the spec — refusing to emit an empty contract.')
  process.exit(1)
}

const parts = [BANNER, '']
for (const [schema, def] of stringEnums) {
  const name = constName(schema)
  if (def.description) {
    parts.push(`/** ${String(def.description).split('\n')[0]} */`)
  }
  parts.push(`export const ${name} = Object.freeze({`)
  for (const value of def.enum) {
    parts.push(`  ${memberName(value)}: ${JSON.stringify(value)},`)
  }
  parts.push('})')
  parts.push(`/** Every valid \`${schema}\` value, for exhaustiveness checks. */`)
  parts.push(`export const ${name}_VALUES = Object.freeze(${JSON.stringify(def.enum)})`)
  parts.push('')
}

mkdirSync(OUT, { recursive: true })
writeFileSync(resolve(OUT, 'enums.js'), parts.join('\n'))

execFileSync(
  'npx',
  ['openapi-typescript', SPEC, '-o', resolve(OUT, 'schema.d.ts')],
  { stdio: 'inherit' },
)

// Full request SDK: every path, method, query shape and response type.
//
// Invoked with plain flags on purpose. A config file using the (deprecated)
// `asClass` / `output.format` keys makes 0.99 emit ZERO files while still
// exiting 0 — a silent no-op that would leave `api.js` importing a directory
// that no longer exists. The assertion below turns that into a hard failure.
const SDK = resolve(OUT, 'sdk')
execFileSync('npx', ['@hey-api/openapi-ts', '-i', SPEC, '-o', SDK], { stdio: 'inherit' })

const emitted = readdirSync(SDK)
const required = ['sdk.gen.ts', 'types.gen.ts', 'client.gen.ts']
const missing = required.filter((f) => !emitted.includes(f))
if (missing.length) {
  console.error(`\nSDK generation produced no usable output (missing: ${missing.join(', ')}).`)
  console.error('This is the silent-no-op failure mode — check the generator flags.')
  process.exit(1)
}

const operations = readFileSync(resolve(SDK, 'sdk.gen.ts'), 'utf8').match(
  /^export const \w+/gm,
)?.length

console.log(
  `Generated ${stringEnums.length} enums (${stringEnums.map(([n]) => n).join(', ')}), ` +
    `schema types, and an SDK with ${operations} operations.`,
)
