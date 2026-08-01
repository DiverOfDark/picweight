package dev.picweight.android.ui.common

import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.model.NameSource
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import retrofit2.HttpException
import retrofit2.Response
import java.net.UnknownHostException

/**
 * The retry affordance's state machine.
 *
 * A meal that failed for a transient reason — a spent quota, a rate limit, a provider
 * hiccup — used to be unrecoverable: the only way out was deleting it and photographing
 * food that had already been eaten. The button that fixes that is the one control in the
 * day list that *does* something, and it is pressed by a user who is already annoyed, so
 * the two things worth pinning are that a second tap costs nothing and that a refusal
 * says which refusal it was.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MealRetryTest {

    private fun meal(
        status: LocalMealStatus,
        serverId: String? = "srv-1",
        error: String? = null,
    ) = MealEntity(
        clientUuid = "m1",
        serverId = serverId,
        sittingId = "sitting",
        nameSource = NameSource.VISION.value,
        eatenAt = 0L,
        timezoneOffset = 0,
        status = status,
        error = error,
        createdAt = 0L,
        updatedAt = 0L,
    )

    // ---- idle → retrying → success -----------------------------------------

    @Test
    fun `a tap goes idle to retrying and back to idle once the server accepts`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val asked = mutableListOf<String>()
        val retries = MealRetries(this, TAG) { key ->
            asked += key
            gate.await()
        }

        assertFalse("nothing is retrying before the first tap", retries.state.value.isRetrying("m1"))

        assertTrue("the tap must be accepted", retries.start("m1"))
        assertTrue(
            "the button has to be disabled before the request is even dispatched",
            retries.state.value.isRetrying("m1"),
        )

        runCurrent()
        assertEquals("exactly one analysis was asked for", listOf("m1"), asked)
        assertTrue("still in flight while the server is thinking", retries.state.value.isRetrying("m1"))

        gate.complete(Unit)
        runCurrent()

        assertFalse("the button comes back once it lands", retries.state.value.isRetrying("m1"))
        assertNull("a retry the server took is not an error", retries.state.value.error)
    }

    // ---- idle → retrying → failure -----------------------------------------

    @Test
    fun `a refused retry goes back to idle and names what refused it`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val retries = MealRetries(this, TAG) {
            gate.await()
            throw httpException(500)
        }

        retries.start("m1")
        runCurrent()
        assertTrue(retries.state.value.isRetrying("m1"))

        gate.complete(Unit)
        runCurrent()

        assertFalse(
            "a failure must re-enable the button, or the meal is stuck all over again",
            retries.state.value.isRetrying("m1"),
        )
        val error = retries.state.value.error
        assertNotNull("a silent failure is the bug this feature exists to undo", error)
        assertTrue("the status code is the handle: $error", error!!.contains("500"))
    }

    // ---- the double tap ----------------------------------------------------

    @Test
    fun `a second tap while a retry is in flight is a no-op`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var calls = 0
        val retries = MealRetries(this, TAG) {
            calls++
            gate.await()
        }

        assertTrue(retries.start("m1"))
        // Same frame, before the dispatcher has run anything: the claim is synchronous
        // precisely so this case cannot slip through.
        assertFalse("a frustrated double tap must not ask twice", retries.start("m1"))

        runCurrent()
        assertFalse("and not after dispatch either", retries.start("m1"))

        gate.complete(Unit)
        runCurrent()

        assertEquals("one tap, one analysis", 1, calls)
        // Once it has landed the meal is retryable again — the guard is per attempt, not
        // a one-shot latch.
        assertTrue(retries.start("m1"))
        gate.complete(Unit)
        runCurrent()
        assertEquals(2, calls)
    }

    @Test
    fun `retrying one meal leaves the other failed rows tappable`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val retries = MealRetries(this, TAG) { gate.await() }

        retries.start("m1")
        assertTrue("a day can hold more than one failed meal", retries.start("m2"))

        assertTrue(retries.state.value.isRetrying("m1"))
        assertTrue(retries.state.value.isRetrying("m2"))

        gate.complete(Unit)
        runCurrent()
        assertEquals(emptySet<String>(), retries.state.value.inFlight)
    }

    // ---- the copy ----------------------------------------------------------

    @Test
    fun `a 409 reads as a stale row rather than a malfunction`() = runTest {
        // The endpoint answers 409 for "this meal isn't failed any more" and for "an
        // analysis is already queued or running" — both mean the row that was tapped is
        // out of date, which is not a fault the user should be alarmed by.
        val retries = MealRetries(this, TAG) { throw httpException(409) }

        retries.start("m1")
        runCurrent()

        val error = retries.state.value.error
        assertNotNull(error)
        assertTrue("should explain the conflict: $error", error!!.contains("isn't failed any more"))
        assertFalse("a server refusal is not an outage: $error", error.contains("Offline"))
        assertFalse(
            "and Room has nothing better to show, so it must not promise that: $error",
            error.contains("showing what this phone knows"),
        )
    }

    @Test
    fun `a genuinely offline retry says it was not sent, and promises nothing else`() = runTest {
        val retries = MealRetries(this, TAG) { throw UnknownHostException("picweight.example.com") }

        retries.start("m1")
        runCurrent()

        val error = retries.state.value.error
        assertNotNull(error)
        assertTrue("was: $error", error!!.contains("wasn't sent"))
        assertFalse(
            "nothing is being shown from Room here — the retry simply did not happen: $error",
            error.contains("showing what this phone knows"),
        )
    }

    @Test
    fun `every server-side refusal keeps its own words instead of degrading to offline`() {
        listOf(500, 502, 429, 404, 401).forEach { code ->
            val copy = RetryErrorCopy.forFailure(ApiFailures.explain(httpException(code)))
            assertTrue("HTTP $code should carry its code: $copy", copy.contains(code.toString()))
            assertFalse("HTTP $code is not an outage: $copy", copy.contains("Offline"))
        }
    }

    @Test
    fun `a dismissed refusal stays dismissed`() = runTest {
        val retries = MealRetries(this, TAG) { throw httpException(500) }
        retries.start("m1")
        runCurrent()
        assertNotNull(retries.state.value.error)

        retries.dismissError()

        assertNull(retries.state.value.error)
    }

    @Test
    fun `starting a fresh retry clears the previous refusal`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var fail = true
        val retries = MealRetries(this, TAG) {
            if (fail) throw httpException(500)
            gate.await()
        }

        retries.start("m1")
        runCurrent()
        assertNotNull(retries.state.value.error)

        fail = false
        retries.start("m1")
        assertNull("the stale banner must not outlive the tap that replaced it", retries.state.value.error)

        gate.complete(Unit)
        runCurrent()
    }

    // ---- lifecycle ---------------------------------------------------------

    @Test
    fun `a cancelled retry is not reported as a failed one`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val retries = MealRetries(backgroundScope, TAG) { gate.await() }

        retries.start("m1")
        runCurrent()
        assertTrue(retries.state.value.isRetrying("m1"))

        // The screen goes away mid-request: backgroundScope is cancelled by runTest.
        backgroundScope.coroutineContext[Job]!!.cancelChildren()
        runCurrent()

        assertFalse(
            "a returning screen must not find a permanently disabled button",
            retries.state.value.isRetrying("m1"),
        )
        assertNull("a cancelled scope has not refused anything", retries.state.value.error)
    }

    // ---- which meals get the button ----------------------------------------

    @Test
    fun `only a meal whose server-side analysis failed offers a retry`() {
        assertTrue(MealRetry.isRetryable(meal(LocalMealStatus.FAILED, error = "your quota is done")))

        // The other kind of FAILED: the upload gave up, so the server has never heard of
        // this meal and there is no analysis to re-run. A button here could only 404.
        assertFalse(
            "an upload that never landed has nothing to retry server-side",
            MealRetry.isRetryable(meal(LocalMealStatus.FAILED, serverId = null)),
        )

        LocalMealStatus.entries
            .filter { it != LocalMealStatus.FAILED }
            .forEach {
                assertFalse(
                    "$it is not a failure and the server would answer 409",
                    MealRetry.isRetryable(meal(it)),
                )
            }
    }

    private fun httpException(code: Int): HttpException = HttpException(
        Response.error<Unit>(
            code,
            """{"error":"boom"}""".toResponseBody("application/json".toMediaType()),
        ),
    )

    private companion object {
        const val TAG = "MealRetryTest"
    }
}
