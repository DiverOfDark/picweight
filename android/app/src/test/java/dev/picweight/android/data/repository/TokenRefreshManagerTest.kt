package dev.picweight.android.data.repository

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The renewal window. A queued upload may surface days after it was captured, so the
 * session has to be renewed well before it lapses rather than after a 401.
 */
class TokenRefreshManagerTest {

    private val hour = 60 * 60 * 1000L

    @Test
    fun `a fresh long-lived token is left alone`() {
        val now = 0L
        val ttl = 24 * 3600L
        assertFalse(TokenRefreshManager.shouldRefresh(now, now + 24 * hour, ttl))
    }

    @Test
    fun `refresh once half the lifetime is gone`() {
        val now = 13 * hour
        val issuedAt = 0L
        val ttl = 24 * 3600L
        assertTrue(TokenRefreshManager.shouldRefresh(now, issuedAt + 24 * hour, ttl))
    }

    @Test
    fun `a short-lived token still gets a one-hour floor`() {
        val now = 0L
        val ttl = 900L // 15 minutes: half of it would be a 7-minute window
        assertTrue(TokenRefreshManager.shouldRefresh(now, now + 10 * 60 * 1000L, ttl))
    }

    @Test
    fun `no token means nothing to refresh`() {
        assertFalse(TokenRefreshManager.shouldRefresh(System.currentTimeMillis(), 0L, 3600L))
    }
}
