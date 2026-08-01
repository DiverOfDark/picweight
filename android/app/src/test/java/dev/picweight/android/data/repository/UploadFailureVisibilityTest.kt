package dev.picweight.android.data.repository

import android.content.Context
import com.fasterxml.jackson.databind.ObjectMapper
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.MacroTotals
import dev.picweight.android.data.remote.model.MealAcceptedResponse
import dev.picweight.android.data.remote.model.MealResponse
import dev.picweight.android.data.remote.model.MealStatus
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.sync.SyncScheduler
import io.mockk.coEvery
import io.mockk.mockk
import kotlinx.coroutines.test.runTest
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import retrofit2.Response
import java.io.File
import java.net.SocketTimeoutException
import java.time.OffsetDateTime

/**
 * An upload that cannot succeed must never look like one that is merely waiting.
 *
 * This is the lesson from the "forever QUEUED" bug rather than a restatement of it: the
 * worker there was never constructed at all, but the *reason it hid for so long* is that
 * every failure mode in this layer degraded into the same silent, indefinitely-retrying
 * QUEUED row, rendered as "Waiting for a connection". `uploadOne` caught `IOException`,
 * logged it at [android.util.Log.i], and returned RETRY — and anything that was not an
 * `IOException` escaped the method entirely, failing the work permanently while the row
 * still claimed to be waiting for a signal.
 *
 * So the rules pinned here are:
 *  - repeated I/O failures are written onto the row, with a count and a reason;
 *  - a single blip is not (a lost request in a lift is not an incident);
 *  - a non-`IOException` is caught, recorded, and gives up rather than escaping;
 *  - and none of it ever drops the meal (G5), which is why the row stays recoverable.
 */
class UploadFailureVisibilityTest {

    private lateinit var store: LinkedHashMap<String, MealEntity>
    private lateinit var dao: MealDao
    private lateinit var api: PicweightApi
    private lateinit var repository: MealRepository

    private val uuid = "11111111-2222-3333-4444-555555555555"

    @Before
    fun setUp() {
        store = LinkedHashMap()
        dao = mockk(relaxed = true)
        api = mockk(relaxed = true)

        coEvery { dao.byClientUuid(any()) } answers { store[firstArg()] }
        coEvery { dao.upsert(any()) } answers {
            val meal = firstArg<MealEntity>()
            store[meal.clientUuid] = meal
        }
        // applyServerMeal writes the meal and its items together.
        coEvery { dao.upsertWithItems(any(), any()) } answers {
            val meal = firstArg<MealEntity>()
            store[meal.clientUuid] = meal
        }
        coEvery { dao.markFailed(any(), any(), any()) } answers {
            val id = firstArg<String>()
            store[id]?.let {
                store[id] = it.copy(status = LocalMealStatus.FAILED, error = secondArg())
            }
            Unit
        }
        coEvery { dao.noteUploadError(any(), any(), any()) } answers {
            val id = firstArg<String>()
            store[id]?.let { store[id] = it.copy(error = secondArg()) }
            Unit
        }

        repository = MealRepository(
            dao = dao,
            api = api,
            scheduler = mockk(relaxed = true),
            mapper = ObjectMapper(),
            context = mockk<Context>(relaxed = true),
        )

        store[uuid] = queued()
    }

    private fun queued() = MealEntity(
        clientUuid = uuid,
        sittingId = "sitting",
        nameSource = NameSource.VISION.value,
        eatenAt = 1_700_000_000_000L,
        timezoneOffset = 120,
        photoPath = null,
        status = LocalMealStatus.QUEUED,
        createdAt = 1_700_000_000_000L,
        updatedAt = 1_700_000_000_000L,
    )

    @Test
    fun `the first bounced attempt stays quiet`() = runTest {
        coEvery { api.createMeal(any(), any()) } throws SocketTimeoutException("timeout")

        val outcome = repository.uploadOne(uuid, attempt = 0)

        assertEquals(UploadOutcome.RETRY, outcome)
        assertEquals(LocalMealStatus.QUEUED, store.getValue(uuid).status)
        assertNull(
            "one lost request is normal and must not shout at the user",
            store.getValue(uuid).error,
        )
    }

    @Test
    fun `a repeatedly failing upload says so, with a count and a reason`() = runTest {
        coEvery { api.createMeal(any(), any()) } throws SocketTimeoutException("Read timed out")

        val outcome = repository.uploadOne(uuid, attempt = 4)

        assertEquals(UploadOutcome.RETRY, outcome)
        val row = store.getValue(uuid)
        // Still queued: WorkManager keeps trying and the meal is never dropped (G5)...
        assertEquals(LocalMealStatus.QUEUED, row.status)
        // ...but it no longer claims to be waiting for a connection.
        val error = row.error
        assertNotNull("attempt 5 must be recorded on the row", error)
        assertTrue("should count the attempts, was: $error", error!!.contains("failed 5 times"))
        assertTrue("should name the reason, was: $error", error.contains("Read timed out"))
    }

    @Test
    fun `a retryable server status is recorded rather than swallowed`() = runTest {
        coEvery { api.createMeal(any(), any()) } returns
            Response.error(503, "".asJsonBody())

        val outcome = repository.uploadOne(uuid, attempt = 3)

        assertEquals(UploadOutcome.RETRY, outcome)
        val error = store.getValue(uuid).error
        assertNotNull("a repeatedly 503-ing upload is not 'waiting for a connection'", error)
        assertTrue("was: $error", error!!.contains("503"))
    }

    /**
     * The important one. Before this, anything that was not an `IOException` or an
     * `HttpException` propagated out of `uploadOne`, out of `doWork`, and was turned by
     * WorkManager into a terminal FAILED work item — while the row sat at QUEUED saying
     * "Waiting for a connection". A crash in a worker must never present as waiting.
     */
    @Test
    fun `a non-IOException is caught, recorded on the row, and gives up`() = runTest {
        coEvery { api.createMeal(any(), any()) } throws
            IllegalStateException("No server URL configured")

        val outcome = repository.uploadOne(uuid, attempt = 0)

        assertEquals(
            "a bug must not be retried forever as though it were a flaky network",
            UploadOutcome.GIVE_UP,
            outcome,
        )
        val row = store.getValue(uuid)
        assertEquals(LocalMealStatus.FAILED, row.status)
        val error = row.error
        assertNotNull("the crash must be readable on the row", error)
        assertTrue("should name the exception, was: $error", error!!.contains("IllegalStateException"))
        assertTrue("should carry the message, was: $error", error.contains("No server URL configured"))
    }

    @Test
    fun `a cancelled upload is not mistaken for a failed one`() = runTest {
        coEvery { api.createMeal(any(), any()) } throws
            kotlinx.coroutines.CancellationException("worker stopped")

        val thrown = runCatching { repository.uploadOne(uuid, attempt = 0) }.exceptionOrNull()

        assertTrue(
            "cancellation must propagate, or structured concurrency breaks",
            thrown is kotlinx.coroutines.CancellationException,
        )
        assertEquals(LocalMealStatus.QUEUED, store.getValue(uuid).status)
        assertNull("a stopped worker has not failed", store.getValue(uuid).error)
    }

    /**
     * The local photo is what the queue renders while the server has nothing to show, so
     * a successful POST must not take it away — `202 Accepted` carries no thumbnail URL.
     */
    @Test
    fun `a successful upload keeps the local photo until the server has a thumbnail`() = runTest {
        val photo = File.createTempFile("shot", ".jpg").apply { writeBytes(ByteArray(16)) }
        store[uuid] = queued().copy(photoPath = photo.absolutePath)

        coEvery { api.createMeal(any(), any()) } returns Response.success(
            MealAcceptedResponse().apply {
                mealId = "srv-1"
                revision = 1
                status = MealStatus.PENDING
                deduplicated = false
            }
        )

        val outcome = repository.uploadOne(uuid, attempt = 0)

        assertEquals(UploadOutcome.DONE, outcome)
        val row = store.getValue(uuid)
        assertEquals(LocalMealStatus.PENDING, row.status)
        assertEquals(
            "the queue would render a grey placeholder for the whole analysis otherwise",
            photo.absolutePath,
            row.photoPath,
        )
        assertTrue("the file itself must survive too", photo.isFile)

        photo.delete()
    }

    /** Sanity: the wire status actually round-trips now (it used to fall through). */
    @Test
    fun `the accepted status is mapped from the wire rather than defaulted`() {
        assertEquals(
            LocalMealStatus.NEEDS_REVIEW,
            LocalMealStatus.fromWire(MealStatus.NEEDS_REVIEW.value),
        )
    }

    /** The other half of the hand-off: the local file goes only once a server one lands. */
    @Test
    fun `the local photo is released when the server thumbnail arrives`() = runTest {
        val photo = File.createTempFile("shot", ".jpg").apply { writeBytes(ByteArray(16)) }
        store[uuid] = queued().copy(photoPath = photo.absolutePath, status = LocalMealStatus.PENDING)

        repository.applyServerMeal(serverMeal(thumbnail = "thumbs/abc.jpg"))

        val row = store.getValue(uuid)
        assertNull("the server can render it now", row.photoPath)
        assertEquals("thumbs/abc.jpg", row.thumbnailUrl)
        assertTrue("and the file must not be left behind", !photo.isFile)
    }

    @Test
    fun `the local photo is kept while the server still has no thumbnail`() = runTest {
        val photo = File.createTempFile("shot", ".jpg").apply { writeBytes(ByteArray(16)) }
        store[uuid] = queued().copy(photoPath = photo.absolutePath, status = LocalMealStatus.PENDING)

        repository.applyServerMeal(serverMeal(thumbnail = null))

        assertEquals(photo.absolutePath, store.getValue(uuid).photoPath)
        assertTrue(photo.isFile)

        photo.delete()
    }

    private fun serverMeal(thumbnail: String?): MealResponse = MealResponse().apply {
        clientUuid = this@UploadFailureVisibilityTest.uuid
        id = "srv-1"
        nameSource = NameSource.VISION
        status = MealStatus.ANALYZING
        revision = 1
        portionScale = 1.0
        timezoneOffset = 120
        eatenAt = OffsetDateTime.parse("2026-08-01T12:00:00Z")
        createdAt = OffsetDateTime.parse("2026-08-01T12:00:00Z")
        updatedAt = OffsetDateTime.parse("2026-08-01T12:00:00Z")
        thumbnailUrl = thumbnail
        items = emptyList()
        totals = MacroTotals().apply {
            kcal = 0.0
            proteinG = 0.0
            fatG = 0.0
            carbsG = 0.0
        }
    }

    private fun String.asJsonBody(): ResponseBody = toResponseBody("application/json".toMediaType())
}
