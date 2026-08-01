/**
 * The single door to the backend.
 *
 * Authentication is a `picweight_session` HttpOnly cookie set by
 * `GET /api/auth/callback`, so there is no token for the SPA to attach — the
 * job of this wrapper is the other half: recognise a 401, hand it to whoever
 * owns the router, and turn the API's `ErrorBody` into a message worth showing.
 */

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

async function readError(response) {
  const fallback = `The server returned ${response.status}.`
  try {
    const body = await response.json()
    if (body && typeof body.message === 'string') {
      return new ApiError(response.status, body.error ?? 'error', body.message)
    }
  } catch {
    // Not JSON — the static fallback or a proxy error page.
  }
  return new ApiError(response.status, 'error', fallback)
}

async function request(path, options = {}) {
  const { method = 'GET', body, signal, headers = {} } = options

  const init = {
    method,
    signal,
    credentials: 'same-origin',
    headers: { Accept: 'application/json', ...headers },
  }

  if (body !== undefined) {
    if (body instanceof FormData) {
      init.body = body
    } else {
      init.headers['Content-Type'] = 'application/json'
      init.body = JSON.stringify(body)
    }
  }

  let response
  try {
    response = await fetch(path, init)
  } catch (cause) {
    if (cause?.name === 'AbortError') throw cause
    throw new ApiError(0, 'offline', 'Cannot reach the server. Check the connection and try again.')
  }

  if (response.status === 401) {
    onUnauthorized()
    throw new ApiError(401, 'unauthenticated', 'The session has expired. Sign in again.')
  }
  if (!response.ok) throw await readError(response)
  if (response.status === 204) return null

  const type = response.headers.get('content-type') || ''
  if (!type.includes('application/json')) return null
  return response.json()
}

/** Build `?a=1&b=2`, dropping anything unset. */
function query(params) {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(params ?? {})) {
    if (value !== undefined && value !== null && value !== '') search.append(key, String(value))
  }
  const rendered = search.toString()
  return rendered ? `?${rendered}` : ''
}

export const api = {
  /** Claims of the current session, or a 401 when there is none. */
  session: () => request('/api/auth/me'),

  /** Slide the session cookie so an active user never hits the absolute expiry. */
  refreshSession: () => request('/api/auth/refresh', { method: 'POST' }),

  /** Issuer and client ids, for the "signing in against …" line on /login. */
  authConfig: () => request('/api/auth/config'),

  /** Identity, profile and today's state in one request. */
  me: () => request(`${API}/me`),

  /** Write body data; the backend recomputes targets and may return warnings. */
  updateProfile: (profile) => request(`${API}/me/profile`, { method: 'PUT', body: profile }),

  /** One local day: totals, targets, verdict, collapsed sittings, loose meals. */
  day: (date, tzOffset) => request(`${API}/days/${date}${query({ tz_offset: tzOffset })}`),

  /** History, newest first. `from`/`to` are inclusive local dates. */
  meals: (params) => request(`${API}/meals${query(params)}`),

  /** One meal with its items at the current revision. */
  meal: (id) => request(`${API}/meals/${encodeURIComponent(id)}`),

  /** Confirm, rename, rescale or replace the item list. */
  patchMeal: (id, patch) =>
    request(`${API}/meals/${encodeURIComponent(id)}`, { method: 'PATCH', body: patch }),

  /** Remove a meal — the two-tap fix for a side dish counted twice. */
  deleteMeal: (id) => request(`${API}/meals/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Resume the persisted agent session with the user's own words. */
  reanalyze: (id, feedback) =>
    request(`${API}/meals/${encodeURIComponent(id)}/reanalyze`, {
      method: 'POST',
      body: { feedback },
    }),

  /** Revision history, newest first, with the feedback that caused each. */
  revisions: (id) => request(`${API}/meals/${encodeURIComponent(id)}/revisions`),

  /** A sitting: its member meals and their combined totals. */
  group: (groupId) => request(`${API}/groups/${encodeURIComponent(groupId)}`),

  /** Weight readings, newest first. */
  weights: (params) => request(`${API}/weights${query(params)}`),

  /** Log a weight; the backend recomputes targets from it. */
  logWeight: (payload) => request(`${API}/weights`, { method: 'POST', body: payload }),

  /** Last N confirmed dishes. */
  recentDishes: (limit) => request(`${API}/dishes/recent${query({ limit })}`),

  /** URL of the full dump; the session cookie authorises the plain navigation. */
  exportUrl: (format) => `${API}/export${query({ format })}`,

  /** Relative URL of a meal's 768px thumbnail. */
  thumbnailUrl: (id) => `${API}/meals/${encodeURIComponent(id)}/thumbnail`,

  /** The completion stream. The caller owns closing it. */
  eventsUrl: () => `${API}/meals/events`,
}
