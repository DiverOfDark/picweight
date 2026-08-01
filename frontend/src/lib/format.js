/**
 * Formatting helpers.
 *
 * Every number the user reads passes through here so rounding is decided once.
 * The backend deals in UTC instants plus a per-meal `timezone_offset` (minutes
 * east of UTC at capture), because "what did I eat today" is a question about
 * *your* day, not UTC's — so local rendering always goes through that offset
 * rather than the browser's clock.
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

/** The browser's current offset in minutes east of UTC, the API's convention. */
export function browserOffsetMinutes() {
  return -new Date().getTimezoneOffset()
}

/** Shift an instant into a fixed offset so UTC accessors read local wall time. */
function shifted(instant, offsetMinutes) {
  const date = instant instanceof Date ? instant : new Date(instant)
  return new Date(date.getTime() + (offsetMinutes ?? 0) * 60_000)
}

/** `YYYY-MM-DD` for an instant in the given offset. */
export function localDateKey(instant, offsetMinutes = 0) {
  return shifted(instant, offsetMinutes).toISOString().slice(0, 10)
}

/** Today's `YYYY-MM-DD` in the browser's own offset. */
export function todayKey() {
  return localDateKey(new Date(), browserOffsetMinutes())
}

/** Step a `YYYY-MM-DD` key by whole days. */
export function shiftDateKey(key, days) {
  const date = new Date(`${key}T00:00:00Z`)
  date.setUTCDate(date.getUTCDate() + days)
  return date.toISOString().slice(0, 10)
}

/** `14:05` — the wall-clock time at capture. */
export function localTime(instant, offsetMinutes = 0) {
  const d = shifted(instant, offsetMinutes)
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

/** `Fri 1 Aug` for a `YYYY-MM-DD` key. */
export function dayLabel(key) {
  return DAY_LABEL.format(new Date(`${key}T00:00:00Z`))
}

/** `Friday 1 August 2026` for a `YYYY-MM-DD` key. */
export function dayLabelFull(key) {
  return DAY_LABEL_FULL.format(new Date(`${key}T00:00:00Z`))
}

/** `Today` / `Yesterday` / `Fri 1 Aug`. */
export function relativeDayLabel(key) {
  const today = todayKey()
  if (key === today) return 'Today'
  if (key === shiftDateKey(today, -1)) return 'Yesterday'
  return dayLabel(key)
}

/** `1 Aug, 14:05` in the browser's offset — for revision and weight timestamps. */
export function stamp(instant) {
  const offset = browserOffsetMinutes()
  return `${dayLabel(localDateKey(instant, offset))}, ${localTime(instant, offset)}`
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
