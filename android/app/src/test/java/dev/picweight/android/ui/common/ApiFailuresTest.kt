package dev.picweight.android.ui.common

import dev.picweight.android.data.remote.MeFixtures
import dev.picweight.android.data.remote.model.MeResponse
import dev.picweight.android.di.AppModule
import dev.picweight.android.ui.home.HomeErrorCopy
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import retrofit2.HttpException
import retrofit2.Response
import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import javax.net.ssl.SSLHandshakeException

/**
 * The exception-to-sentence mapping.
 *
 * Every case here was previously indistinguishable: one `runCatching {}.onFailure {}`
 * rendered "Offline — showing what this phone knows." for all of them, which is a lie in
 * every case but the first and cost hours of debugging on a bug that was neither an
 * outage nor an auth problem.
 */
class ApiFailuresTest {

    private val mapper = AppModule.provideObjectMapper()

    // ---- genuinely offline -------------------------------------------------

    @Test
    fun `no DNS is offline`() {
        val failure = ApiFailures.explain(UnknownHostException("picweight.example.com"))
        assertEquals(FailureKind.OFFLINE, failure.kind)
        assertTrue(failure.message.startsWith("Offline"))
    }

    @Test
    fun `a refused connection is offline`() {
        assertEquals(FailureKind.OFFLINE, ApiFailures.classify(ConnectException("ECONNREFUSED")))
    }

    @Test
    fun `only the offline case keeps the -showing what this phone knows- reassurance`() {
        assertEquals(
            "Offline — showing what this phone knows.",
            HomeErrorCopy.forFailure(ApiFailures.explain(UnknownHostException("nope"))),
        )
        // Room is not the best available answer for any of these, so the promise is not
        // made. This is the assertion the whole change exists for.
        listOf(
            httpException(500),
            httpException(401),
            contractFailure(),
            SocketTimeoutException("read timed out"),
        ).forEach { t ->
            val copy = HomeErrorCopy.forFailure(ApiFailures.explain(t))
            assertFalse(
                "must not claim Room has the answer for ${t.javaClass.simpleName}: $copy",
                copy.contains("showing what this phone knows"),
            )
        }
    }

    // ---- reachable, but not answering the way we expect ---------------------

    @Test
    fun `a timeout is not reported as offline`() {
        // The server may be up and simply wedged; "Offline" would send the user to check
        // their Wi-Fi instead of their pod.
        val failure = ApiFailures.explain(SocketTimeoutException("timeout"))
        assertEquals(FailureKind.TIMEOUT, failure.kind)
        assertFalse(failure.message.contains("Offline"))
    }

    @Test
    fun `a rejected certificate is not reported as offline`() {
        val failure = ApiFailures.explain(SSLHandshakeException("trust anchor not found"))
        assertEquals(FailureKind.INSECURE, failure.kind)
        assertTrue(failure.message.contains("certificate"))
    }

    // ---- HTTP --------------------------------------------------------------

    @Test
    fun `a server error carries its status code`() {
        val failure = ApiFailures.explain(httpException(500))
        assertEquals(FailureKind.HTTP, failure.kind)
        assertTrue("the code is the debugging handle: ${failure.message}", failure.message.contains("500"))
        assertFalse(failure.message.contains("Offline"))
    }

    @Test
    fun `429 and 404 say what they are`() {
        assertTrue(ApiFailures.explain(httpException(429)).message.contains("429"))
        assertTrue(ApiFailures.explain(httpException(404)).message.contains("404"))
    }

    @Test
    fun `a 401 is a session problem, not a network one`() {
        val failure = ApiFailures.explain(httpException(401))
        assertEquals(FailureKind.UNAUTHORIZED, failure.kind)
        assertTrue(failure.message.contains("401"))
        assertTrue(failure.message.contains("Sign in"))
    }

    // ---- the bug -----------------------------------------------------------

    @Test
    fun `the payload that broke the phone is a contract mismatch, not an outage`() {
        val failure = ApiFailures.explain(contractFailure())

        assertEquals(FailureKind.CONTRACT, failure.kind)
        assertFalse(
            "a 200 with an unreadable body is not an outage: ${failure.message}",
            failure.message.contains("Offline"),
        )
        assertTrue(failure.message.contains("contract mismatch"))
        // The field is the whole diagnosis; without it this is another shrug.
        assertTrue(
            "the message must name the field: ${failure.message}",
            failure.message.contains("user.created_at"),
        )
    }

    @Test
    fun `a parse failure is tested before IOException, or it reads as offline`() {
        val thrown = contractFailure()
        // The ordering hazard, asserted rather than assumed.
        assertTrue(thrown is IOException)
        assertEquals(FailureKind.CONTRACT, ApiFailures.classify(thrown))
    }

    @Test
    fun `a parse failure wrapped by a caller is still a contract mismatch`() {
        val wrapped = IllegalStateException("refresh failed", contractFailure())
        assertEquals(FailureKind.CONTRACT, ApiFailures.classify(wrapped))
    }

    // ---- everything else ---------------------------------------------------

    @Test
    fun `an unrecognised failure is reported verbatim rather than guessed at`() {
        val failure = ApiFailures.explain(IllegalStateException("no session"))
        assertEquals(FailureKind.UNEXPECTED, failure.kind)
        assertTrue(failure.message.contains("IllegalStateException"))
        assertTrue(failure.message.contains("no session"))
    }

    @Test
    fun `a self-referential cause chain terminates`() {
        // Defensive: a classifier that loops here would hang the refresh, not just
        // mislabel it.
        val looping = object : RuntimeException("loop") {
            override val cause: Throwable get() = this
        }
        assertEquals(FailureKind.UNEXPECTED, ApiFailures.classify(looping))
    }

    @Test
    fun `report returns the same failure it logs`() {
        // `android.util.Log` returns defaults under the JVM test runner
        // (`unitTests.isReturnDefaultValues`), so this asserts the call is made without
        // throwing and that logging does not alter the verdict.
        assertEquals(
            ApiFailures.explain(contractFailure()),
            ApiFailures.report("ApiFailuresTest", "Refreshing today", contractFailure()),
        )
    }

    // ---- helpers -----------------------------------------------------------

    /** A real Jackson failure, from the real payload, through the real generated model. */
    private fun contractFailure(): Throwable =
        runCatching {
            mapper.readValue(
                MeFixtures.me(createdAt = MeFixtures.OFFSETLESS),
                MeResponse::class.java,
            )
        }.exceptionOrNull() ?: throw AssertionError("the offset-less payload should not parse")

    private fun httpException(code: Int): HttpException = HttpException(
        Response.error<Unit>(code, """{"error":"boom"}""".toResponseBody("application/json".toMediaType())),
    )
}
