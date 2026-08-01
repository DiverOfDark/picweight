/**
 * The single door to the backend — a thin session-policy layer over the
 * **generated** SDK.
 *
 * No URL, HTTP method, query-parameter name or body shape is written by hand
 * in the web client. They all come from `generated/sdk`, generated from
 * `android/openapi.json`, generated from the Rust types. Hand-maintaining that
 * contract caused three production bugs in one day, and for the worst of them
 * the OpenAPI document itself was wrong — so the fix was to make the document
 * truthful (backend `enum_wire_values_match_the_openapi_schema`) and then to
 * stop re-typing it here.
 *
 * What is deliberately still hand-written is everything the spec cannot say:
 *   - authentication is a `picweight_session` HttpOnly cookie, so requests need
 *     `credentials`, not an Authorization header the SDK would attach;
 *   - a 401 must reach whoever owns the router, exactly once, from anywhere;
 *   - the API's `ErrorBody` becomes an `ApiError` with a message worth showing.
 *
 * The exported `api` surface is unchanged from the hand-rolled version, so
 * views did not have to move when this was swapped underneath them.
 */

import { client } from '@/lib/generated/sdk/client.gen'
import * as sdk from '@/lib/generated/sdk/sdk.gen'

const API = '/api/v1'

/** A non-2xx response, carrying the API's stable machine-readable code. */
export class ApiError extends Error {
  constructor(status, code, message) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
  }
}

let onUnauthorized = () => {}

/**
 * Register what happens when the session has gone. Called once by the router
 * so every view gets consistent behaviour without importing the router.
 */
export function setUnauthorizedHandler(handler) {
  onUnauthorized = handler
}

// The session is a cookie; nothing else needs configuring. `throwOnError` stays
// off so `unwrap` below owns error translation for every call in one place.
client.setConfig({
  credentials: 'same-origin',
  headers: { Accept: 'application/json' },
})

/**
 * Turn an SDK result into the value a view wants, or throw an `ApiError`.
 *
 * The SDK returns `{ data, error, response }` rather than throwing, which is
 * exactly the seam needed to apply one session policy to every endpoint.
 */
async function unwrap(promise) {
  let result
  try {
    result = await promise
  } catch (cause) {
    if (cause?.name === 'AbortError') throw cause
    throw new ApiError(0, 'offline', 'Cannot reach the server. Check the connection and try again.')
  }

  const { data, error, response } = result

  if (response?.status === 401) {
    onUnauthorized()
    throw new ApiError(401, 'unauthenticated', 'The session has expired. Sign in again.')
  }

  if (error !== undefined || (response && !response.ok)) {
    const status = response?.status ?? 0
    if (error && typeof error.message === 'string') {
      throw new ApiError(status, error.error ?? 'error', error.message)
    }
    throw new ApiError(status, 'error', `The server returned ${status}.`)
  }

  // 204 and non-JSON bodies surface as undefined; views expect null.
  return data ?? null
}

/** Build `?a=1&b=2`, dropping anything unset. Used only for plain-navigation URLs. */
function query(params) {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(params ?? {})) {
    if (value !== undefined && value !== null && value !== '') search.append(key, String(value))
  }
  const rendered = search.toString()
  return rendered ? `?${rendered}` : ''
}

/** Drop unset query values so the SDK does not serialise `undefined`. */
function clean(params) {
  return Object.fromEntries(
    Object.entries(params ?? {}).filter(([, v]) => v !== undefined && v !== null && v !== ''),
  )
}

export const api = {
  /** Claims of the current session, or a 401 when there is none. */
  session: () => unwrap(sdk.sessionInfo()),

  /** Slide the session cookie so an active user never hits the absolute expiry. */
  refreshSession: () => unwrap(sdk.refresh()),

  /** Issuer and client ids, for the "signing in against …" line on /login. */
  authConfig: () => unwrap(sdk.authConfig()),

  /** Identity, profile and today's state in one request. */
  me: () => unwrap(sdk.getMe()),

  /** Write body data; the backend recomputes targets and may return warnings. */
  updateProfile: (profile) => unwrap(sdk.updateProfile({ body: profile })),

  /** One local day: totals, targets, verdict, collapsed sittings, loose meals. */
  day: (date, tzOffset) =>
    unwrap(sdk.getDay({ path: { date }, query: clean({ tz_offset: tzOffset }) })),

  /** History, newest first. `from`/`to` are inclusive local dates. */
  meals: (params) => unwrap(sdk.listMeals({ query: clean(params) })),

  /** One meal with its items at the current revision. */
  meal: (id) => unwrap(sdk.getMeal({ path: { id } })),

  /** Confirm, rename, rescale or replace the item list. */
  patchMeal: (id, patch) => unwrap(sdk.patchMeal({ path: { id }, body: patch })),

  /** Remove a meal — the two-tap fix for a side dish counted twice. */
  deleteMeal: (id) => unwrap(sdk.deleteMeal({ path: { id } })),

  /** Resume the persisted agent session with the user's own words. */
  reanalyze: (id, feedback) =>
    unwrap(sdk.reanalyzeMeal({ path: { id }, body: { feedback } })),

  /**
   * Re-run the agent on a meal whose analysis failed — same revision, same
   * stored thumbnail, nothing asked of the phone. 409 when the meal did not
   * fail or an attempt is already in flight; `unwrap` turns that into an
   * `ApiError` carrying the server's own sentence.
   */
  retryMeal: (id) => unwrap(sdk.retryMeal({ path: { id } })),

  /** Revision history, newest first, with the feedback that caused each. */
  revisions: (id) => unwrap(sdk.mealRevisions({ path: { id } })),

  /** A sitting: its member meals and their combined totals. */
  group: (groupId) => unwrap(sdk.getGroup({ path: { group_id: groupId } })),

  /** Weight readings, newest first. */
  weights: (params) => unwrap(sdk.listWeights({ query: clean(params) })),

  /** Log a weight; the backend recomputes targets from it. */
  logWeight: (payload) => unwrap(sdk.logWeight({ body: payload })),

  /** Last N confirmed dishes. */
  recentDishes: (limit) => unwrap(sdk.recentDishes({ query: clean({ limit }) })),

  /** Resolve a scanned barcode to a food record. */
  barcode: (ean) => unwrap(sdk.resolveBarcode({ path: { ean } })),

  // --- Plain URLs -----------------------------------------------------------
  //
  // These are navigated to or handed to EventSource/<img>, not fetched, so they
  // stay strings. The paths still match the generated SDK's operations.

  /** URL of the full dump; the session cookie authorises the plain navigation. */
  exportUrl: (format) => `${API}/export${query({ format })}`,

  /** Relative URL of a meal's 768px thumbnail. */
  thumbnailUrl: (id) => `${API}/meals/${encodeURIComponent(id)}/thumbnail`,

  /** The completion stream. The caller owns closing it. */
  eventsUrl: () => `${API}/meals/events`,
}
