package dev.picweight.android.data.repository

import android.content.Context
import com.fasterxml.jackson.databind.ObjectMapper
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.MealAcceptedResponse
import dev.picweight.android.data.remote.model.MealStatus
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.sync.SyncScheduler
import io.mockk.coEvery
import io.mockk.coVerify
import io.mockk.mockk
import io.mockk.verify
import kotlinx.coroutines.test.runTest
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import retrofit2.HttpException
import retrofit2.Response

/**
 * Retrying a failed analysis, from Room's point of view.
 *
 * The photo is not the problem and never was: the upload returned 202 and the server
 * still holds the 768px thumbnail it derived. Only the agent run died. So a retry sends
 * one meal id and nothing else, and its whole local effect is to stop the row saying why
 * the *last* attempt failed and put it back in the queue the event stream watches.
 */
class RetryAnalysisTest {

    private lateinit var store: LinkedHashMap<String, MealEntity>
    private lateinit var dao: MealDao
    private lateinit var api: PicweightApi
    private lateinit var scheduler: SyncScheduler
    private lateinit var repository: MealRepository

    private val uuid = "11111111-2222-3333-4444-555555555555"

    @Before
    fun setUp() {
        store = LinkedHashMap()
        dao = mockk(relaxed = true)
        api = mockk(relaxed = true)
        scheduler = mockk(relaxed = true)

        coEvery { dao.byClientUuid(any()) } answers { store[firstArg()] }
        coEvery { dao.upsert(any()) } answers {
            val meal = firstArg<MealEntity>()
            store[meal.clientUuid] = meal
        }

        repository = MealRepository(
            dao = dao,
            api = api,
            scheduler = scheduler,
            mapper = ObjectMapper(),
            context = mockk<Context>(relaxed = true),
        )

        store[uuid] = failed()
    }

    /** A meal the *server* failed: it has an id there, and a reason on the row. */
    private fun failed() = MealEntity(
        clientUuid = uuid,
        serverId = "srv-1",
        sittingId = "sitting",
        nameSource = NameSource.VISION.value,
        eatenAt = 1_700_000_000_000L,
        timezoneOffset = 120,
        thumbnailUrl = "thumbs/abc.jpg",
        status = LocalMealStatus.FAILED,
        revision = 1,
        error = "Estimation failed: the model provider reported the quota is exhausted",
        notified = true,
        createdAt = 1_700_000_000_000L,
        updatedAt = 1_700_000_000_000L,
    )

    private fun accepted(revision: Int = 1) = MealAcceptedResponse().apply {
        mealId = "srv-1"
        this.revision = revision
        status = MealStatus.PENDING
        deduplicated = false
    }

    @Test
    fun `a retry clears the reason and puts the meal back to pending at the same revision`() = runTest {
        coEvery { api.retryMeal("srv-1") } returns accepted(revision = 1)

        repository.retryAnalysis(uuid)

        val row = store.getValue(uuid)
        assertEquals(LocalMealStatus.PENDING, row.status)
        assertEquals("a retry is another attempt at the same estimate, not a new one", 1, row.revision)
        assertNull(
            "a meal that is analysing again must not still be captioned with why the last try died",
            row.error,
        )
        assertFalse("this attempt gets to announce its own result", row.notified)
    }

    /**
     * The status is read through the generated [MealStatus] constants, never a literal:
     * the wire values are lowercase snake_case and a hand-written `"Pending"` compiles
     * perfectly while matching nothing.
     */
    @Test
    fun `the accepted status comes off the wire rather than being assumed`() = runTest {
        coEvery { api.retryMeal("srv-1") } returns accepted().apply { status = MealStatus.ANALYZING }

        repository.retryAnalysis(uuid)

        assertEquals(LocalMealStatus.ANALYZING, store.getValue(uuid).status)
    }

    @Test
    fun `a retry re-uploads nothing — the server already has the photo`() = runTest {
        coEvery { api.retryMeal("srv-1") } returns accepted()

        repository.retryAnalysis(uuid)

        coVerify(exactly = 1) { api.retryMeal("srv-1") }
        coVerify(exactly = 0) { api.createMeal(any(), any()) }
        assertEquals(
            "the stored thumbnail is the whole reason a retry needs nothing from the phone",
            "thumbs/abc.jpg",
            store.getValue(uuid).thumbnailUrl,
        )
    }

    @Test
    fun `a retry re-opens the event stream so the result actually arrives`() = runTest {
        coEvery { api.retryMeal("srv-1") } returns accepted()

        repository.retryAnalysis(uuid)

        // PENDING with a server id is what MealEventWorker waits on; without this the
        // meal would finish server-side and the phone would never hear about it.
        verify(exactly = 1) { scheduler.enqueueEventStream() }
    }

    @Test
    fun `a refusal reaches the caller instead of being swallowed`() = runTest {
        coEvery { api.retryMeal("srv-1") } throws HttpException(
            Response.error<Unit>(
                409,
                """{"error":"conflict"}""".toResponseBody("application/json".toMediaType()),
            ),
        )

        val thrown = runCatching { repository.retryAnalysis(uuid) }.exceptionOrNull()

        assertTrue("the UI has to be able to say what refused it", thrown is HttpException)
        val row = store.getValue(uuid)
        assertEquals("a refused retry changes nothing locally", LocalMealStatus.FAILED, row.status)
        assertEquals(failed().error, row.error)
    }

    @Test
    fun `a meal that never reached the server cannot be retried`() = runTest {
        store[uuid] = failed().copy(serverId = null, thumbnailUrl = null)

        val thrown = runCatching { repository.retryAnalysis(uuid) }.exceptionOrNull()

        assertTrue(thrown is IllegalStateException)
        coVerify(exactly = 0) { api.retryMeal(any()) }
    }
}
