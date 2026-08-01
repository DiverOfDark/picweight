#!/usr/bin/env node
/**
 * Guard the one class of bug that costs correctness without ever raising:
 * misreading an API instant.
 *
 * There is no test framework in this project and this is not the excuse to add
 * one — it is a plain node script in the same shape as `check-generated.mjs`,
 * using only `node:assert`. Run it with `npm run check:time`.
 *
 * What it pins down:
 *
 *   - The backend once served `format: date-time` fields as offset-less strings
 *     (`2026-08-01T13:32:33.441427539`). Android threw and reported itself
 *     offline; the browser did not throw at all — `new Date` reads an
 *     offset-less date-time as *local*, so every timestamp the web UI drew was
 *     silently wrong by the viewer's offset. These assertions fail loudly if
 *     `parseInstant` ever goes back to doing that.
 *   - Local-day bucketing (PRD §8) — a meal's day is the day where it was
 *     eaten, a weight reading's day is the viewer's day, and neither is UTC's.
 *   - DST: the offset that matters is the one in force *at that instant*.
 *
 * The whole file runs under a fixed, non-UTC, DST-observing zone. Under `TZ=UTC`
 * the original bug is invisible, which is precisely why it survived to
 * production — so pinning the zone is load-bearing, not incidental.
 */

process.env.TZ = 'Europe/Berlin' // +01:00 winter, +02:00 summer

import assert from 'node:assert/strict'

const {
  browserOffsetMinutesAt,
  dayLabel,
  dayOffsetMinutes,
  instantMs,
  localDateKey,
  localTime,
  parseInstant,
  shiftDateKey,
  stamp,
  todayKey,
} = await import('../src/lib/format.js')

let failures = 0

function check(name, fn) {
  try {
    fn()
    console.log(`  ✓ ${name}`)
  } catch (error) {
    failures += 1
    console.error(`  ✗ ${name}\n    ${error.message.split('\n').join('\n    ')}`)
  }
}

// The literal body the deployed pod returned, plus the corrected form.
const NAIVE = '2026-08-01T13:32:33.441427539'
const RFC3339 = `${NAIVE}Z`
const EXPECTED_MS = Date.UTC(2026, 7, 1, 13, 32, 33, 441)

console.log('parseInstant')

check('reads the RFC 3339 instant the backend now sends', () => {
  assert.equal(parseInstant(RFC3339).getTime(), EXPECTED_MS)
})

check('truncates chrono nanoseconds instead of trusting the engine to', () => {
  assert.equal(parseInstant(RFC3339).getUTCMilliseconds(), 441)
})

check('reads an offset-less instant as UTC, never as local time', () => {
  // The regression itself. Berlin is +02:00 in August, so the old behaviour
  // landed exactly two hours early.
  assert.equal(parseInstant(NAIVE).getTime(), EXPECTED_MS)
  assert.notEqual(new Date(NAIVE).getTime(), EXPECTED_MS) // ...as `new Date` proves
})

check('honours a non-Z offset', () => {
  assert.equal(parseInstant('2026-08-01T16:32:33.441+03:00').getTime(), EXPECTED_MS)
})

check('turns junk into null rather than an Invalid Date', () => {
  for (const bad of [undefined, null, '', 'not a date', {}, Number.NaN, new Date('x')]) {
    assert.equal(parseInstant(bad), null, `expected null for ${JSON.stringify(bad)}`)
  }
})

check('accepts a Date and epoch milliseconds', () => {
  assert.equal(parseInstant(new Date(EXPECTED_MS)).getTime(), EXPECTED_MS)
  assert.equal(parseInstant(EXPECTED_MS).getTime(), EXPECTED_MS)
})

console.log('local-day bucketing')

check("a meal's day is the day where it was eaten, not UTC's", () => {
  // 22:30 UTC is half past one the next morning in Moscow (+180).
  assert.equal(localDateKey('2026-08-01T22:30:00Z', 180), '2026-08-02')
  assert.equal(localDateKey('2026-08-01T22:30:00Z', 0), '2026-08-01')
  // ...and a negative offset pulls the other way.
  assert.equal(localDateKey('2026-08-01T02:30:00Z', -300), '2026-07-31')
})

check('an offset-free instant buckets into the viewer day, not the UTC day', () => {
  // Weight readings and `created_at` carry no offset. Berlin is +02:00 here, so
  // 22:30 UTC is already tomorrow locally. `localDateKey(x, 0)` would say
  // otherwise — that was the live defect in the weight trend's captions.
  assert.equal(localDateKey('2026-08-01T22:30:00Z'), '2026-08-02')
})

check('the wall-clock time reads in the offset it is given', () => {
  assert.equal(localTime('2026-08-01T13:32:33Z', 180), '16:32')
  assert.equal(localTime('2026-08-01T13:32:33Z', 0), '13:32')
  assert.equal(localTime('2026-08-01T13:32:33Z'), '15:32') // Berlin, +02:00
})

check('a day key round-trips through its label without sliding a day', () => {
  // `dayLabel` reads a key back as midnight UTC precisely so it cannot re-apply
  // the offset the key already carries. Asserted on the day number rather than
  // a literal string, because the label is rendered in the host locale.
  const key = localDateKey('2026-08-01T22:30:00Z', 180)
  assert.equal(key, '2026-08-02')
  assert.match(dayLabel(key), /\b0?2\b/, `label for ${key} should name the 2nd`)
  assert.notEqual(dayLabel(key), dayLabel('2026-08-01'))
})

console.log('daylight saving')

check('the offset is the one in force at that instant, not the one in force now', () => {
  assert.equal(browserOffsetMinutesAt('2026-01-15T12:00:00Z'), 60)
  assert.equal(browserOffsetMinutesAt('2026-07-15T12:00:00Z'), 120)
})

check('a winter stamp does not read an hour off when viewed in summer', () => {
  assert.equal(stamp('2026-01-15T23:30:00Z'), `${dayLabel('2026-01-16')}, 00:30`)
  assert.equal(stamp('2026-07-15T23:30:00Z'), `${dayLabel('2026-07-16')}, 01:30`)
})

check('tz_offset is the offset on the day being viewed', () => {
  assert.equal(dayOffsetMinutes('2026-01-15'), 60)
  assert.equal(dayOffsetMinutes('2026-07-15'), 120)
  // The 2026 European changes are 29 March and 25 October.
  assert.equal(dayOffsetMinutes('2026-03-28'), 60)
  assert.equal(dayOffsetMinutes('2026-03-30'), 120)
  // A non-key still yields something sendable rather than NaN.
  assert.equal(dayOffsetMinutes('garbage'), -new Date().getTimezoneOffset())
})

console.log('defensiveness')

check('nothing throws on a malformed instant', () => {
  for (const bad of [undefined, null, 'garbage']) {
    assert.equal(localTime(bad, 0), '—')
    assert.equal(localDateKey(bad, 0), '')
    assert.equal(stamp(bad), '—')
  }
  assert.equal(dayLabel('garbage'), '—')
  assert.equal(dayLabel(undefined), '—')
})

check('a malformed instant sinks itself instead of scrambling the sort', () => {
  assert.equal(instantMs('garbage'), 0)
  assert.equal(instantMs(RFC3339), EXPECTED_MS)
  const order = ['2026-08-03T10:00:00Z', 'garbage', '2026-08-01T10:00:00Z']
    .sort((a, b) => instantMs(b) - instantMs(a))
  assert.deepEqual(order, ['2026-08-03T10:00:00Z', '2026-08-01T10:00:00Z', 'garbage'])
})

console.log('date keys')

check('keys step by whole days across a month boundary', () => {
  assert.equal(shiftDateKey('2026-08-01', -1), '2026-07-31')
  assert.equal(shiftDateKey('2026-02-28', 1), '2026-03-01')
  assert.equal(shiftDateKey('2026-03-29', -7), '2026-03-22') // over the DST change
})

check('a non-key comes back untouched instead of throwing', () => {
  assert.equal(shiftDateKey('garbage', -1), 'garbage')
  assert.equal(shiftDateKey(undefined, -1), undefined)
})

check('todayKey is the viewer\'s day', () => {
  assert.match(todayKey(), /^\d{4}-\d{2}-\d{2}$/)
  assert.equal(todayKey(), localDateKey(new Date(), browserOffsetMinutesAt(new Date())))
})

if (failures) {
  console.error(`\n✗ ${failures} timestamp assertion${failures === 1 ? '' : 's'} failed.`)
  process.exit(1)
}

console.log('\n✓ API instants parse, bucket and render correctly')
