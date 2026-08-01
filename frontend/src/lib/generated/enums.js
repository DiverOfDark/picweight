/**
 * GENERATED FROM android/openapi.json — DO NOT EDIT.
 *
 * Regenerate with `npm run generate`. If a value here looks wrong, the API is
 * wrong: fix the Rust type and the spec regenerates. Hand-editing this file
 * recreates the class of bug it exists to prevent.
 */

/** How a day is going. Computed from the numbers, never asked of a model. */
export const DAY_STATUS = Object.freeze({
  ON_TRACK: "on_track",
  TIGHT: "tight",
  OVER: "over",
  PROTEIN_UNREACHABLE: "protein_unreachable",
  NO_TARGETS: "no_targets",
})
/** Every valid `DayStatus` value, for exhaustiveness checks. */
export const DAY_STATUS_VALUES = Object.freeze(["on_track","tight","over","protein_unreachable","no_targets"])

/** Output format. */
export const EXPORT_FORMAT = Object.freeze({
  JSON: "json",
  CSV: "csv",
})
/** Every valid `ExportFormat` value, for exhaustiveness checks. */
export const EXPORT_FORMAT_VALUES = Object.freeze(["json","csv"])

/** Direction of the calorie goal. */
export const GOAL_TYPE = Object.freeze({
  LOSE: "lose",
  MAINTAIN: "maintain",
  GAIN: "gain",
})
/** Every valid `GoalType` value, for exhaustiveness checks. */
export const GOAL_TYPE_VALUES = Object.freeze(["lose","maintain","gain"])

/** Where an item's gram figure came from. */
export const GRAMS_SOURCE = Object.freeze({
  AGENT: "agent",
  USER: "user",
  BARCODE: "barcode",
  RECALL: "recall",
})
/** Every valid `GramsSource` value, for exhaustiveness checks. */
export const GRAMS_SOURCE_VALUES = Object.freeze(["agent","user","barcode","recall"])

/** Where an item's macros came from. */
export const MACRO_SOURCE = Object.freeze({
  RECALL: "recall",
  MODEL: "model",
  BARCODE: "barcode",
  WEB: "web",
  USER: "user",
})
/** Every valid `MacroSource` value, for exhaustiveness checks. */
export const MACRO_SOURCE_VALUES = Object.freeze(["recall","model","barcode","web","user"])

/** What happened. */
export const MEAL_EVENT_KIND = Object.freeze({
  QUEUED: "queued",
  ANALYZING: "analyzing",
  COMPLETED: "completed",
  REANALYZED: "reanalyzed",
  FAILED: "failed",
  GROUP_SETTLED: "group_settled",
  UPDATED: "updated",
})
/** Every valid `MealEventKind` value, for exhaustiveness checks. */
export const MEAL_EVENT_KIND_VALUES = Object.freeze(["queued","analyzing","completed","reanalyzed","failed","group_settled","updated"])

/** Lifecycle of a meal. `pending`/`analyzing` are in-flight, `needs_review` */
export const MEAL_STATUS = Object.freeze({
  PENDING: "pending",
  ANALYZING: "analyzing",
  NEEDS_REVIEW: "needs_review",
  CONFIRMED: "confirmed",
  FAILED: "failed",
})
/** Every valid `MealStatus` value, for exhaustiveness checks. */
export const MEAL_STATUS_VALUES = Object.freeze(["pending","analyzing","needs_review","confirmed","failed"])

/** Which input path supplied the dish name. Instrumented in M4 to answer */
export const NAME_SOURCE = Object.freeze({
  VISION: "vision",
  RECENT_CHIP: "recent_chip",
  SHARE_INTENT: "share_intent",
  COMMENT: "comment",
  MANUAL: "manual",
})
/** Every valid `NameSource` value, for exhaustiveness checks. */
export const NAME_SOURCE_VALUES = Object.freeze(["vision","recent_chip","share_intent","comment","manual"])

/** Biological sex, as required by the Mifflin-St Jeor BMR formula (§6). */
export const SEX = Object.freeze({
  MALE: "male",
  FEMALE: "female",
})
/** Every valid `Sex` value, for exhaustiveness checks. */
export const SEX_VALUES = Object.freeze(["male","female"])

/** How a weight reading was captured. */
export const WEIGHT_SOURCE = Object.freeze({
  MANUAL: "manual",
  ONBOARDING: "onboarding",
  IMPORT: "import",
  SCALE: "scale",
})
/** Every valid `WeightSource` value, for exhaustiveness checks. */
export const WEIGHT_SOURCE_VALUES = Object.freeze(["manual","onboarding","import","scale"])
