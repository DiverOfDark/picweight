/**
 * API enum values, re-exported from the generated contract, plus the small
 * derived helpers a spec cannot express.
 *
 * **No enum value is typed by hand in this file, or anywhere else in the web
 * client.** They come from `generated/enums.js`, which comes from
 * `android/openapi.json`, which comes from the Rust types. That chain exists
 * because breaking it caused three production bugs in one day — culminating in
 * a spec that advertised `Sex: ["Male","Female"]` against a server accepting
 * only `male`/`female`, which the frontend then faithfully copied.
 *
 * Two guards keep the chain honest:
 *   - backend `enum_wire_values_match_the_openapi_schema` fails if the document
 *     stops describing what the server actually speaks;
 *   - `npm run check:generated` (CI) fails if the committed output here drifts
 *     from what the current spec produces.
 *
 * Comparing a status against a bare string literal is therefore a bug, and a
 * greppable one.
 */

export {
  DAY_STATUS,
  DAY_STATUS_VALUES,
  EXPORT_FORMAT,
  GOAL_TYPE,
  GOAL_TYPE_VALUES,
  GRAMS_SOURCE,
  MACRO_SOURCE,
  MEAL_EVENT_KIND,
  MEAL_STATUS,
  MEAL_STATUS_VALUES,
  NAME_SOURCE,
  SEX,
  SEX_VALUES,
  WEIGHT_SOURCE,
} from '@/lib/generated/enums'

import { GOAL_TYPE, MEAL_STATUS, SEX } from '@/lib/generated/enums'

// --- Derived: presentation and predicates -----------------------------------
//
// These are *choices*, not contract. The spec says which values exist; it does
// not say which of them count as "in flight", or what to call them in a form.

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
