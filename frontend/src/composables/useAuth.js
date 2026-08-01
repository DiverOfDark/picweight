import { computed, ref } from 'vue'
import { api, ApiError } from '@/lib/api'

/**
 * Session state, shared across the app.
 *
 * picweight always runs behind OIDC — the backend performs discovery at startup
 * and refuses to serve without an issuer — so, unlike phos, there is no
 * "auth disabled" mode to fall back into. A network failure leaves the user
 * unauthenticated with a reason to show, never silently signed in.
 */
const claims = ref(null)
const checked = ref(false)
const error = ref('')

export function useAuth() {
  const isAuthenticated = computed(() => !!claims.value)
  const displayName = computed(
    () => claims.value?.name || claims.value?.email || claims.value?.sub || '',
  )

  /** Read the session cookie's claims. Resolves whether or not one exists. */
  async function fetchSession() {
    try {
      claims.value = await api.session()
      error.value = ''
      // Slide the cookie so an active user never hits the absolute expiry.
      api.refreshSession().catch(() => {})
    } catch (e) {
      claims.value = null
      error.value = e instanceof ApiError && e.status === 401 ? '' : e.message
    }
    checked.value = true
  }

  /** Hand the browser to the IdP. The backend owns PKCE, nonce and CSRF. */
  function login() {
    window.location.href = '/api/auth/login'
  }

  /** Clear the cookie server-side, then land back on /login. */
  function logout() {
    claims.value = null
    checked.value = false
    window.location.href = '/api/auth/logout'
  }

  /** Drop cached claims so the next guarded navigation re-checks. */
  function forget() {
    claims.value = null
    checked.value = false
  }

  return { claims, checked, error, isAuthenticated, displayName, fetchSession, login, logout, forget }
}
