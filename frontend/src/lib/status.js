/**
 * The day's verdict is the interface's accent colour.
 *
 * `DayStatus` comes from the rules engine, never from a model, so the colour is
 * a computed fact rather than a mood. `applyDayAccent` rewrites the `--day-accent`
 * custom property; everything that reads `text-day` / `bg-day` follows.
 */

/** Status → the token that describes it. Status colours are never reused as series colours. */
export const DAY_STATUS = {
  on_track: { label: 'On track', accent: 'var(--color-good)', tone: 'good' },
  tight: { label: 'Tight', accent: 'var(--color-warning)', tone: 'warning' },
  over: { label: 'Over', accent: 'var(--color-critical)', tone: 'critical' },
  protein_unreachable: {
    label: 'Protein out of reach',
    accent: 'var(--color-serious)',
    tone: 'serious',
  },
  no_targets: { label: 'No targets yet', accent: 'var(--color-ink-3)', tone: 'muted' },
}

/** Look up a status, falling back to the neutral one for anything unknown. */
export function dayStatus(status) {
  return DAY_STATUS[status] ?? DAY_STATUS.no_targets
}

/** Point `--day-accent` at the status colour. Idempotent; safe to call often. */
export function applyDayAccent(status) {
  const resolved = getComputedStyle(document.documentElement)
    .getPropertyValue(dayStatus(status).accent.slice(4, -1).trim())
    .trim()
  document.documentElement.style.setProperty('--day-accent', resolved || '#898781')
}

/** Meal lifecycle → the tone its pill wears. */
export const MEAL_TONE = {
  Pending: 'muted',
  Analyzing: 'warning',
  NeedsReview: 'serious',
  Confirmed: 'good',
  Failed: 'critical',
}

/** Tailwind classes for a tone. Kept here so a pill never invents its own colour. */
export const TONE_CLASS = {
  good: 'text-good border-good/40 bg-good/10',
  warning: 'text-warning border-warning/40 bg-warning/10',
  serious: 'text-serious border-serious/40 bg-serious/10',
  critical: 'text-critical border-critical/40 bg-critical/10',
  muted: 'text-ink-3 border-border bg-white/5',
}
