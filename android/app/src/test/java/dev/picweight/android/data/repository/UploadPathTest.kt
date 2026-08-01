package dev.picweight.android.data.repository

import android.content.Context
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.json.JsonMapper
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule
import com.fasterxml.jackson.module.kotlin.KotlinModule
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.BaseUrlInterceptor
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.MealStatus
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.sync.SyncScheduler
import io.mockk.coEvery
import io.mockk.every
import io.mockk.mockk
import io.mockk.slot
import io.mockk.verify
import kotlinx.coroutines.test.runTest
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import retrofit2.Retrofit
import retrofit2.converter.jackson.JacksonConverterFactory
import java.io.File

/**
 * The capture → Room → WorkManager → POST path, exercised end to end on the JVM.
 *
 * The live symptom this is written against: a captured meal reaches
 * [LocalMealStatus.QUEUED] and stays there, while the server log shows the POST was
 * never attempted — not even as a 4xx. So either the uuid WorkManager was handed does
 * not name the row, or [MealRepository.uploadOne] returns before it reaches the wire.
 */
class UploadPathTest {

    private lateinit var store: LinkedHashMap<String, MealEntity>
    private lateinit var dao: MealDao
    private lateinit var scheduler: SyncScheduler

    /** Every request the OkHttp stack actually got as far as building. */
    private val issued = mutableListOf<Request>()

    /** Bodies, fully serialised — a multipart body only fails when it is written. */
    private val issuedBodies = mutableListOf<String>()

    private val mapper: ObjectMapper = JsonMapper.builder()
        .addModule(KotlinModule.Builder().build())
        .addModule(JavaTimeModule())
        .build()

    @Before
    fun setUp() {
        store = LinkedHashMap()
        dao = mockk(relaxed = true)
        scheduler = mockk(relaxed = true)

        coEvery { dao.upsert(any()) } answers {
            val meal = firstArg<MealEntity>()
            store[meal.clientUuid] = meal
        }
        coEvery { dao.bySitting(any()) } answers {
            store.values.filter { it.sittingId == firstArg<String>() }
        }
        coEvery { dao.heldInSitting(any()) } answers {
            store.values.lastOrNull {
                it.sittingId == firstArg<String>() && it.status == LocalMealStatus.HELD
            }
        }
        coEvery { dao.staleHeld(any()) } answers {
            store.values.filter { it.status == LocalMealStatus.HELD && it.createdAt < firstArg<Long>() }
        }
        coEvery { dao.awaitingUpload() } answers {
            store.values.filter {
                it.status == LocalMealStatus.HELD || it.status == LocalMealStatus.QUEUED
            }
        }
        coEvery { dao.awaitingAnalysis() } returns emptyList()
        coEvery { dao.byClientUuid(any()) } answers { store[firstArg<String>()] }
    }

    /**
     * Retrofit wired exactly as `AppModule` wires it — same base URL, same converter —
     * with the socket replaced by an interceptor that records what was built. Anything
     * that throws while assembling the multipart request throws here too.
     */
    private fun api(): PicweightApi {
        val client = OkHttpClient.Builder()
            .addInterceptor { chain ->
                val request = chain.request()
                issued += request
                issuedBodies += Buffer().also { request.body?.writeTo(it) }.readUtf8()
                Response.Builder()
                    .request(request)
                    .protocol(Protocol.HTTP_1_1)
                    .code(202)
                    .message("Accepted")
                    .body(
                        """{"deduplicated":false,"meal_id":"srv-1","revision":1,"status":"pending"}"""
                            .toResponseBody("application/json".toMediaType())
                    )
                    .build()
            }
            .build()
        return Retrofit.Builder()
            .baseUrl(BaseUrlInterceptor.PLACEHOLDER_BASE_URL)
            .client(client)
            .addConverterFactory(JacksonConverterFactory.create(mapper))
            .build()
            .create(PicweightApi::class.java)
    }

    private fun repository(api: PicweightApi = api()) = MealRepository(
        dao = dao,
        api = api,
        scheduler = scheduler,
        mapper = mapper,
        context = mockk<Context>(relaxed = true),
    )

    private fun jpeg(): File = File.createTempFile("capture-", ".jpg").apply {
        writeBytes(ByteArray(64) { 0x7F })
        deleteOnExit()
    }

    // ---- (a) does WorkManager get the uuid Room is keyed by? -----------------

    @Test
    fun `the uuid handed to WorkManager is the uuid Room is keyed by`() = runTest {
        val repository = repository()
        val enqueued = slot<String>()
        every { scheduler.enqueueUpload(capture(enqueued)) } returns Unit

        val sitting = repository.newSitting()
        val uuid = repository.addShot(
            sittingId = sitting,
            photo = jpeg(),
            dishName = "Шаурма",
            comment = null,
            nameSource = NameSource.VISION,
        )
        repository.closeSitting(sitting)

        assertEquals(LocalMealStatus.QUEUED, store.getValue(uuid).status)
        assertTrue("enqueueUpload was never called", enqueued.isCaptured)
        // Byte-for-byte: no trimming, no case change, no regenerated uuid.
        assertEquals(uuid, enqueued.captured)
        assertNotNull("the enqueued uuid must name a row", dao.byClientUuid(enqueued.captured))
    }

    @Test
    fun `resumeQueue re-enqueues the same uuid the row is keyed by`() = runTest {
        val repository = repository()
        val enqueued = mutableListOf<String>()
        every { scheduler.enqueueUpload(capture(enqueued)) } returns Unit

        val sitting = repository.newSitting()
        val uuid = repository.addShot(sitting, jpeg(), "Плов", null, NameSource.VISION)
        repository.closeSitting(sitting)
        enqueued.clear()

        repository.resumeQueue()

        assertEquals(listOf(uuid), enqueued)
    }

    // ---- (b) does uploadOne reach the wire? ---------------------------------

    @Test
    fun `a queued meal actually issues POST api v1 meals`() = runTest {
        val repository = repository()
        val sitting = repository.newSitting()
        val uuid = repository.addShot(sitting, jpeg(), "Шаурма", "с сыром", NameSource.RECENT_CHIP)
        repository.closeSitting(sitting)
        assertEquals(LocalMealStatus.QUEUED, store.getValue(uuid).status)

        val outcome = repository.uploadOne(uuid)

        assertEquals(
            "the upload never reached the HTTP stack; requests seen: $issued",
            1,
            issued.size,
        )
        assertEquals("POST", issued[0].method)
        assertEquals("/api/v1/meals", issued[0].url.encodedPath)
        assertTrue("client_uuid must be in the body", issuedBodies[0].contains(uuid))
        assertEquals(UploadOutcome.DONE, outcome)
    }

    @Test
    fun `a queued meal with no photo still issues the POST`() = runTest {
        val repository = repository()
        val sitting = repository.newSitting()
        val uuid = repository.addShot(sitting, null, "Кола", null, NameSource.MANUAL)
        repository.closeSitting(sitting)

        repository.uploadOne(uuid)

        assertEquals(1, issued.size)
    }

    /** A whole day's worth of offsets, since the offset is stamped per meal. */
    @Test
    fun `every real timezone offset builds a sendable request`() = runTest {
        val repository = repository()
        for (offsetMinutes in listOf(-720, -480, -210, 0, 120, 330, 345, 720, 780, 840)) {
            issued.clear()
            val uuid = "uuid-$offsetMinutes"
            store[uuid] = MealEntity(
                clientUuid = uuid,
                sittingId = "sitting-$offsetMinutes",
                nameSource = NameSource.VISION.value,
                eatenAt = 1_754_000_000_000L,
                timezoneOffset = offsetMinutes,
                status = LocalMealStatus.QUEUED,
                createdAt = 0,
                updatedAt = 0,
            )
            repository.uploadOne(uuid)
            assertEquals("offset $offsetMinutes produced no request", 1, issued.size)
        }
    }

    // ---- (c) does the status the server sends survive the trip into Room? ----

    @Test
    fun `a server status maps to the matching local status`() {
        assertEquals(LocalMealStatus.PENDING, LocalMealStatus.fromWire(MealStatus.PENDING.value))
        assertEquals(LocalMealStatus.ANALYZING, LocalMealStatus.fromWire(MealStatus.ANALYZING.value))
        assertEquals(LocalMealStatus.NEEDS_REVIEW, LocalMealStatus.fromWire(MealStatus.NEEDS_REVIEW.value))
        assertEquals(LocalMealStatus.CONFIRMED, LocalMealStatus.fromWire(MealStatus.CONFIRMED.value))
        assertEquals(LocalMealStatus.FAILED, LocalMealStatus.fromWire(MealStatus.FAILED.value))
    }
}
