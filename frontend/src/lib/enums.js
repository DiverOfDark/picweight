/**
 * Canonical API enum wire values.
 *
 * The backend serialises every enum through the `text_enum!` macro in
 * `backend/src/models.rs`, which emits **lowercase snake_case** strings —
 * `needs_review`, `recent_chip`, `male` — not the PascalCase Rust variant
 * names. Getting that backwards is not a hypothetical: the first version of
 * this frontend keyed its label and tone maps by `NeedsReview`/`Male`/`Lose`
 * and compared statuses against `'Confirmed'`. Every lookup silently fell
 * through to a default and every comparison silently inverted, so failed
 * meals were counted in trends and confirmed meals kept offering a Confirm
 * button. Only `PUT /me/profile` failed loudly, because `sex` is one of the
 * few enums sent back to the server:
 *
 *   unknown variant `Male`, expected `male` or `female`
 *
 * So: these strings are declared once, here, and imported everywhere. Any
 * component comparing a status against a bare string literal is a bug.
 *
 * Authority is `android/openapi.json`, generated from the utoipa annotations.
 */

/** `meals.status` — see PRD §8. */
export const MEAL_STATUS = {
  PENDING: 'pending',
  ANALYZING: 'analyzing',
  NEEDS_REVIEW: 'needs_review',
  CONFIRMED: 'confirmed',
  FAILED: 'failed',
}

/** `meals.name_source` — which input path supplied the dish name (§8). */
export const NAME_SOURCE = {
  VISION: 'vision',
  RECENT_CHIP: 'recent_chip',
  SHARE_INTENT: 'share_intent',
  COMMENT: 'comment',
  MANUAL: 'manual',
}

/** `user_profiles.sex` — required by Mifflin-St Jeor (§6). */
export const SEX = {
  MALE: 'male',
  FEMALE: 'female',
}

/** `user_profiles.goal_type`. */
export const GOAL_TYPE = {
  LOSE: 'lose',
  MAINTAIN: 'maintain',
  GAIN: 'gain',
}

/** `weight_logs.source`. */
export const WEIGHT_SOURCE = {
  MANUAL: 'manual',
  ONBOARDING: 'onboarding',
  IMPORT: 'import',
  SCALE: 'scale',
}

/** Options for the profile form's sex selector. */
export const SEX_OPTIONS = [
  { value: SEX.MALE, label: 'Male' },
  { value: SEX.FEMALE, label: 'Female' },
]

/** Options for the profile form's goal selector. */
export const GOAL_OPTIONS = [
  { value: GOAL_TYPE.LOSE, label: 'Lose weight' },
  { value: GOAL_TYPE.MAINTAIN, label: 'Maintain' },
  { value: GOAL_TYPE.GAIN, label: 'Gain weight' },
]

/** A meal whose analysis has finished, successfully or not. */
export function isTerminal(status) {
  return status === MEAL_STATUS.CONFIRMED || status === MEAL_STATUS.FAILED
}

/** A meal still moving through the queue — the UI shows progress for these. */
export function isInFlight(status) {
  return status === MEAL_STATUS.PENDING || status === MEAL_STATUS.ANALYZING
}
