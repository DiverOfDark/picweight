/**
 * Formatting helpers.
 *
 * Every number the user reads passes through here so rounding is decided once.
 * The backend deals in UTC instants plus a per-meal `timezone_offset` (minutes
 * east of UTC at capture), because "what did I eat today" is a question about
 * *your* day, not UTC's — so local rendering always goes through that offset
 * rather than the browser's clock.
 *
 * ## The wire format of an instant
 *
 * Every API timestamp field is declared `{ "type": "string", "format":
 * "date-time" }`, which is RFC 3339 — where the UTC offset is **mandatory**.
 * The backend now honours that and sends `2026-08-01T13:32:33.441427539Z`.
 *
 * It briefly did not: `chrono::NaiveDateTime` rendered `2026-08-01T13:32:33.441427539`,
 * with no `Z`. The Android client — Jackson `OffsetDateTime` — threw and the
 * app reported itself offline, which at least made the bug visible. The web
 * client did something worse: `new Date("2026-08-01T13:32:33.441427539")` is
 * defined to parse an offset-less date-*time* as **local** time, so every
 * timestamp this app drew was silently wrong by the viewer's own UTC offset and
 * nothing ever threw. For a tracker whose premise is correct local-day
 * bucketing (PRD §8) that is a correctness bug, not a cosmetic one.
 *
 * Hence: nothing in the app calls `new Date(apiValue)` directly. Everything
 * goes through `parseInstant`, which is deliberately strict about what it
 * accepts and deliberately forgiving about what it does with the rest.
 */

const KCAL = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 })

/** Whole kilocalories with a thousands separator: `1,450`. */
export function kcal(value) {
  return KCAL.format(Math.round(value ?? 0))
}

/** Signed kilocalories, for a remaining budget that can go negative. */
export function kcalSigned(value) {
  const rounded = Math.round(value ?? 0)
  return rounded > 0 ? `+${KCAL.format(rounded)}` : KCAL.format(rounded)
}

/** Grams, one decimal below 10 and whole above — the precision a scale gives. */
export function grams(value) {
  const n = value ?? 0
  return n < 10 && n > 0 ? n.toFixed(1) : String(Math.round(n))
}

/** Kilograms to one decimal. */
export function kg(value) {
  return (value ?? 0).toFixed(1)
}

/** `0.82` → `82%`. Returns an em dash when the model gave no confidence. */
export function percent(value) {
  if (value === null || value === undefined) return '—'
  return `${Math.round(value * 100)}%`
}

/** What every date-shaped formatter renders when it was handed nothing usable. */
const NO_VALUE = '—'

/** A bare `YYYY-MM-DD`, the `format: date` fields (`birth_date`, `DayState.date`). */
const DATE_KEY = /^\d{4}-\d{2}-\d{2}$/

/**
 * An ISO-8601 date-time carrying **no** offset — the shape RFC 3339 forbids and
 * the one that caused the silent local-time misreading. Also tolerates a space
 * separator, which is what a SQL-ish serialiser emits.
 */
const NAIVE_INSTANT = /^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?$/

/** Sub-millisecond digits, which the backend emits (chrono renders nanoseconds). */
const SUB_MILLIS = /(T\d{2}:\d{2}:\d{2}\.\d{3})\d+/

let warnedAboutNaiveInstant = false

/**
 * Parse an API instant into a `Date`, or `null` if it is not one.
 *
 * **Expected input:** an RFC 3339 date-time *with* an offset, exactly as
 * `format: date-time` promises — `2026-08-01T13:32:33.441427539Z` or
 * `2026-08-01T16:32:33.441+03:00`. A `Date` or an epoch-milliseconds number is
 * accepted too, so callers can pass `new Date()` for "now".
 *
 * Three things it deliberately does beyond `new Date`:
 *
 * 1. **Sub-millisecond digits are truncated.** chrono renders nanoseconds and
 *    the ECMAScript Date Time String Format specifies exactly three fractional
 *    digits; every engine happens to accept more and truncate, but that is
 *    implementation-defined behaviour, so do the truncation here rather than
 *    depend on it.
 * 2. **A missing offset is read as UTC**, not as local time. This is the whole
 *    point of the helper. The values *are* UTC — the schema descriptions say so
 *    — so re-attaching the `Z` is lossless, and it means a backend regression
 *    degrades to "correct" instead of to "quietly off by the viewer's offset".
 *    It also warns once, so the regression is findable rather than invisible.
 * 3. **Junk becomes `null`**, never an `Invalid Date`. An invalid `Date` throws
 *    out of both `toISOString()` and `Intl.DateTimeFormat.format()`, so letting
 *    one through would turn a single malformed field into a blank page.
 *
 * @param {Date | string | number | null | undefined} value
 * @returns {Date | null}
 */
export function parseInstant(value) {
  if (value === null || value === undefined || value === '') return null

  if (value instanceof Date) return Number.isNaN(value.getTime()) ? null : value

  if (typeof value === 'number') {
    return Number.isFinite(value) ? new Date(value) : null
  }

  if (typeof value !== 'string') return null

  let text = value.replace(SUB_MILLIS, '$1')

  if (NAIVE_INSTANT.test(text)) {
    if (!warnedAboutNaiveInstant) {
      warnedAboutNaiveInstant = true
      console.warn(
        `[picweight] API sent an offset-less timestamp (${value}). ` +
          'The spec declares these `format: date-time`, which requires an offset. ' +
          'Reading it as UTC — fix the backend DTO before the numbers drift.',
      )
    }
    text = `${text.replace(' ', 'T')}Z`
  }

  const date = new Date(text)
  return Number.isNaN(date.getTime()) ? null : date
}

/**
 * Epoch milliseconds for an API instant, for sorting and comparison.
 *
 * Unparseable values collapse to `0` rather than `NaN`: a `NaN` in a comparator
 * makes the sort order implementation-defined for the *whole* list, so one bad
 * row would scramble the rest instead of just sinking itself.
 */
export function instantMs(value) {
  return parseInstant(value)?.getTime() ?? 0
}

/**
 * Parse a `YYYY-MM-DD` key as midnight UTC, or `null` if it is not one.
 *
 * A key is a *local* calendar date — whoever produced it already applied the
 * offset — so it is read back in UTC. Reading it in the browser's zone would
 * re-apply that offset and slide the date by a day.
 */
function parseDateKey(key) {
  if (typeof key !== 'string' || !DATE_KEY.test(key)) return null
  const date = new Date(`${key}T00:00:00Z`)
  return Number.isNaN(date.getTime()) ? null : date
}

/**
 * The browser's offset in minutes east of UTC **at a given instant**.
 *
 * At that instant, not right now: `getTimezoneOffset` is DST-aware, so asking
 * the January timestamp gives +01:00 in Berlin even while you read it in July.
 * Falls back to the current offset when the instant is unusable.
 */
export function browserOffsetMinutesAt(instant) {
  return -(parseInstant(instant) ?? new Date()).getTimezoneOffset()
}

/** The browser's current offset in minutes east of UTC, the API's convention. */
export function browserOffsetMinutes() {
  return -new Date().getTimezoneOffset()
}

/**
 * The viewer's offset during a given `YYYY-MM-DD` local day — what to send as
 * `tz_offset`, which the API documents as "minutes east of UTC to bucket by".
 *
 * Sampled at midday, so it is the offset that covers the bulk of that local
 * day whatever side of a DST change it falls on. Sending the offset as it
 * stands *now* would bucket a January day by the summer offset once the clocks
 * moved, quietly moving every 00:00–01:00 meal onto the wrong day.
 */
export function dayOffsetMinutes(key) {
  const date = parseDateKey(key)
  if (!date) return browserOffsetMinutes()
  date.setUTCHours(12)
  return -date.getTimezoneOffset()
}

/**
 * The offset to render an instant in.
 *
 * An explicit offset wins — a meal carries `timezone_offset`, the offset where
 * it was actually eaten, which is the only correct answer for that row. When
 * there is none (weight readings, revision stamps, `created_at`) fall back to
 * the viewer's own offset *at that instant*. Note the default is the viewer's
 * zone and not UTC: a caller that forgets the argument should get their own
 * day, because PRD §8 says the day is theirs.
 */
function resolveOffset(instant, offsetMinutes) {
  return Number.isFinite(offsetMinutes) ? offsetMinutes : browserOffsetMinutesAt(instant)
}

/**
 * Shift an instant into a fixed offset so UTC accessors read local wall time.
 * `null` when the instant did not parse — every caller has to handle that.
 */
function shifted(instant, offsetMinutes) {
  const date = parseInstant(instant)
  if (!date) return null
  return new Date(date.getTime() + offsetMinutes * 60_000)
}

/**
 * `YYYY-MM-DD` for an instant, in `offsetMinutes` east of UTC.
 *
 * Omit the offset to bucket into the viewer's own local day. Returns `''` for
 * an unparseable instant, so a bad row lands in its own visibly-empty bucket
 * rather than silently joining 1970-01-01.
 */
export function localDateKey(instant, offsetMinutes) {
  const date = shifted(instant, resolveOffset(instant, offsetMinutes))
  return date ? date.toISOString().slice(0, 10) : ''
}

/** Today's `YYYY-MM-DD` in the browser's own offset. */
export function todayKey() {
  return localDateKey(new Date())
}

/** Step a `YYYY-MM-DD` key by whole days. A non-key is returned untouched. */
export function shiftDateKey(key, days) {
  const date = parseDateKey(key)
  if (!date) return key
  date.setUTCDate(date.getUTCDate() + days)
  return date.toISOString().slice(0, 10)
}

/**
 * `14:05` — the wall-clock time at capture, in `offsetMinutes` east of UTC.
 * Omit the offset to render in the viewer's own zone.
 */
export function localTime(instant, offsetMinutes) {
  const d = shifted(instant, resolveOffset(instant, offsetMinutes))
  if (!d) return NO_VALUE
  return `${String(d.getUTCHours()).padStart(2, '0')}:${String(d.getUTCMinutes()).padStart(2, '0')}`
}

const DAY_LABEL = new Intl.DateTimeFormat(undefined, {
  weekday: 'short',
  day: 'numeric',
  month: 'short',
  timeZone: 'UTC',
})

const DAY_LABEL_FULL = new Intl.DateTimeFormat(undefined, {
  weekday: 'long',
  day: 'numeric',
  month: 'long',
  year: 'numeric',
  timeZone: 'UTC',
})

/**
 * `Fri 1 Aug` for a `YYYY-MM-DD` key.
 *
 * The key is already local — whoever produced it applied the offset — so it is
 * read back as midnight **UTC** and formatted in UTC. Handing the browser's
 * zone a key would re-apply the offset and slide the label by a day.
 */
export function dayLabel(key) {
  const date = parseDateKey(key)
  return date ? DAY_LABEL.format(date) : NO_VALUE
}

/** `Friday 1 August 2026` for a `YYYY-MM-DD` key. */
export function dayLabelFull(key) {
  const date = parseDateKey(key)
  return date ? DAY_LABEL_FULL.format(date) : NO_VALUE
}

/** `Today` / `Yesterday` / `Fri 1 Aug`. */
export function relativeDayLabel(key) {
  const today = todayKey()
  if (key === today) return 'Today'
  if (key === shiftDateKey(today, -1)) return 'Yesterday'
  return dayLabel(key)
}

/**
 * `1 Aug, 14:05` in the viewer's own zone — for revision, account and weight
 * timestamps, none of which carry an offset of their own.
 *
 * The offset is taken at that instant rather than now, so a stamp from before a
 * DST change does not read an hour off for the rest of the year.
 */
export function stamp(instant) {
  const offset = browserOffsetMinutesAt(instant)
  const day = localDateKey(instant, offset)
  if (!day) return NO_VALUE
  return `${dayLabel(day)}, ${localTime(instant, offset)}`
}

/**
 * Human-readable enum labels, keyed by the **wire** value.
 *
 * The API speaks lowercase snake_case (`needs_review`), never the PascalCase
 * Rust variant name. See `lib/enums.js` for why that is worth a comment.
 */
const LABELS = {
  // meal status
  pending: 'Queued',
  analyzing: 'Analysing',
  needs_review: 'Needs review',
  confirmed: 'Confirmed',
  failed: 'Failed',
  // name_source
  vision: 'Photo',
  recent_chip: 'Recent chip',
  share_intent: 'Share sheet',
  comment: 'Comment',
  manual: 'Manual',
  // grams_source / macro_source
  agent: 'Estimated',
  user: 'You',
  barcode: 'Barcode',
  recall: 'From history',
  model: 'Estimated',
  web: 'Web',
  // weight source
  onboarding: 'Onboarding',
  import: 'Import',
  scale: 'Scale',
  // profile
  male: 'Male',
  female: 'Female',
  lose: 'Lose weight',
  maintain: 'Maintain',
  gain: 'Gain weight',
}

/**
 * Turn an API enum value into something readable.
 *
 * Unknown values are prettified rather than echoed raw, so a variant added
 * server-side shows as "Needs review" rather than leaking `needs_review` into
 * the UI. That also means a future mismatch degrades visibly-but-gracefully
 * instead of silently, which is how the PascalCase bug hid for so long.
 */
export function label(variant) {
  if (!variant) return ''
  const known = LABELS[variant]
  if (known) return known
  const pretty = String(variant).replace(/_/g, ' ')
  return pretty.charAt(0).toUpperCase() + pretty.slice(1)
}

/** Sum the macros of a list of items into a `MacroTotals`-shaped object. */
export function sumTotals(items) {
  return (items ?? []).reduce(
    (acc, item) => ({
      kcal: acc.kcal + (item.kcal ?? 0),
      protein_g: acc.protein_g + (item.protein_g ?? 0),
      fat_g: acc.fat_g + (item.fat_g ?? 0),
      carbs_g: acc.carbs_g + (item.carbs_g ?? 0),
    }),
    { kcal: 0, protein_g: 0, fat_g: 0, carbs_g: 0 },
  )
}
