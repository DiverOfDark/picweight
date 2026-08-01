package dev.picweight.android.data.repository

import android.content.Context
import com.fasterxml.jackson.annotation.JsonInclude
import com.fasterxml.jackson.databind.DeserializationFeature
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.SerializationFeature
import com.fasterxml.jackson.databind.json.JsonMapper
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule
import com.fasterxml.jackson.module.kotlin.KotlinModule
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.AuthInterceptor
import dev.picweight.android.data.remote.BaseUrlInterceptor
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.TokenRefreshManager
import dev.picweight.android.sync.SyncScheduler
import io.mockk.coEvery
import io.mockk.every
import io.mockk.mockk
import kotlinx.coroutines.test.runTest
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.RequestBody.Companion.asRequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.logging.HttpLoggingInterceptor
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import retrofit2.Retrofit
import retrofit2.converter.jackson.JacksonConverterFactory
import java.io.File
import java.net.ServerSocket
import java.net.Socket
import java.nio.charset.StandardCharsets
import java.util.UUID
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * THROWAWAY reproduction harness. Drives MealRepository.uploadOne against a real
 * OkHttp/Retrofit stack (same interceptors and converter the app's Hilt module builds)
 * pointed at a real listening socket, with device-shaped inputs.
 *
 * Assertion is deliberately blunt: did a POST /api/v1/meals actually leave the client?
 */
class UploadReproTest {

    /** Bare recording HTTP server — no MockWebServer in this build's test classpath. */
    private class RecordingServer(private val responder: (String) -> String) {
        private val socket = ServerSocket(0)
        private val running = AtomicBoolean(true)
        val requests = LinkedBlockingQueue<Recorded>()
        val port: Int get() = socket.localPort

        data class Recorded(val requestLine: String, val headers: Map<String, String>, val bodySize: Int, val body: String)

        private val thread = Thread {
            while (running.get()) {
                val client = runCatching { socket.accept() }.getOrNull() ?: break
                runCatching { serve(client) }
                runCatching { client.close() }
            }
        }.apply { isDaemon = true; start() }

        private fun serve(client: Socket) {
            val input = client.getInputStream()
            val head = StringBuilder()
            // Read the request head byte by byte so the body stays on the stream.
            while (!head.endsWith("\r\n\r\n")) {
                val b = input.read()
                if (b == -1) return
                head.append(b.toChar())
            }
            val lines = head.toString().trim().split("\r\n")
            val requestLine = lines.first()
            val headers = lines.drop(1).mapNotNull {
                val i = it.indexOf(':')
                if (i < 0) null else it.substring(0, i).lowercase() to it.substring(i + 1).trim()
            }.toMap()
            val length = headers["content-length"]?.toIntOrNull() ?: 0
            val body = ByteArray(length)
            var read = 0
            while (read < length) {
                val n = input.read(body, read, length - read)
                if (n <= 0) break
                read += n
            }
            requests.add(
                Recorded(
                    requestLine,
                    headers,
                    read,
                    String(body, 0, read.coerceAtLeast(0), StandardCharsets.ISO_8859_1),
                )
            )
            client.getOutputStream().write(responder(requestLine).toByteArray(StandardCharsets.UTF_8))
            client.getOutputStream().flush()
        }

        fun url(): String = "http://127.0.0.1:$port"

        fun shutdown() {
            running.set(false)
            runCatching { socket.close() }
            thread.interrupt()
        }
    }

    private lateinit var server: RecordingServer
    private lateinit var store: LinkedHashMap<String, MealEntity>
    private lateinit var dao: MealDao
    private lateinit var repository: MealRepository
    private lateinit var photo: File
    private lateinit var rawApi: PicweightApi
    private val failures = mutableListOf<Pair<String, String>>()

    private val acceptedBody = """{"deduplicated":false,"meal_id":"srv-1","revision":1,"status":"pending"}"""

    @Before
    fun setUp() {
        server = RecordingServer {
            "HTTP/1.1 202 Accepted\r\n" +
                "Content-Type: application/json\r\n" +
                "Content-Length: ${acceptedBody.toByteArray().size}\r\n" +
                "Connection: close\r\n\r\n" +
                acceptedBody
        }

        val authRepository = mockk<AuthRepository>(relaxed = true)
        every { authRepository.getServerUrl() } returns server.url()
        every { authRepository.getToken() } returns "device-jwt"

        val tokenRefreshManager = mockk<TokenRefreshManager>(relaxed = true)
        every { tokenRefreshManager.ensureFreshToken() } returns "device-jwt"

        val baseUrlInterceptor = BaseUrlInterceptor(dagger.Lazy { authRepository })
        val authInterceptor = AuthInterceptor(
            dagger.Lazy { authRepository },
            dagger.Lazy { tokenRefreshManager },
        )

        val mapper: ObjectMapper = JsonMapper.builder()
            .addModule(KotlinModule.Builder().build())
            .addModule(JavaTimeModule())
            .disable(SerializationFeature.WRITE_DATES_AS_TIMESTAMPS)
            .disable(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES)
            .defaultPropertyInclusion(
                JsonInclude.Value.construct(JsonInclude.Include.NON_NULL, JsonInclude.Include.ALWAYS)
            )
            .build()

        val client = OkHttpClient.Builder()
            .addInterceptor(baseUrlInterceptor)
            .addInterceptor(authInterceptor)
            .addInterceptor(HttpLoggingInterceptor().apply { level = HttpLoggingInterceptor.Level.BASIC })
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(20, TimeUnit.SECONDS)
            .writeTimeout(20, TimeUnit.SECONDS)
            .build()

        val api = Retrofit.Builder()
            .baseUrl(BaseUrlInterceptor.PLACEHOLDER_BASE_URL)
            .client(client)
            .addConverterFactory(JacksonConverterFactory.create(mapper))
            .build()
            .create(PicweightApi::class.java)
        rawApi = api

        store = LinkedHashMap()
        dao = mockk(relaxed = true)
        coEvery { dao.upsert(any()) } answers {
            val meal = firstArg<MealEntity>()
            store[meal.clientUuid] = meal
        }
        coEvery { dao.byClientUuid(any()) } answers { store[firstArg<String>()] }
        coEvery { dao.markFailed(any(), any(), any()) } answers {
            failures += firstArg<String>() to secondArg<String>()
            val uuid = firstArg<String>()
            store[uuid]?.let { store[uuid] = it.copy(status = LocalMealStatus.FAILED, error = secondArg()) }
        }

        photo = File.createTempFile("picweight-repro", ".jpg").apply {
            // Not a valid JPEG, but the HTTP layer never looks: it only streams bytes.
            writeBytes(ByteArray(180_000) { (it % 251).toByte() })
            deleteOnExit()
        }

        repository = MealRepository(
            dao = dao,
            api = api,
            scheduler = mockk<SyncScheduler>(relaxed = true),
            mapper = mapper,
            context = mockk<Context>(relaxed = true),
        )
    }

    @After
    fun tearDown() {
        server.shutdown()
        photo.delete()
    }

    private fun queuedRow(uuid: String) = MealEntity(
        clientUuid = uuid,
        sittingId = uuid,
        groupId = null,
        groupSize = null,
        dishName = "Шаурма",
        comment = null,
        nameSource = NameSource.VISION.value,
        mealType = null,
        eatenAt = System.currentTimeMillis(),
        timezoneOffset = 120,
        photoPath = photo.absolutePath,
        status = LocalMealStatus.QUEUED,
        createdAt = System.currentTimeMillis(),
        updatedAt = System.currentTimeMillis(),
    )

    /** Diagnostic: what does the Retrofit call itself throw, if anything? */
    @Test
    fun `raw createMeal call surfaces its own exception`() = runTest {
        val uuid = UUID.randomUUID().toString()
        val parts = mapOf(
            "client_uuid" to uuid.toRequestBody("text/plain; charset=utf-8".toMediaType()),
            "timezone_offset" to "120".toRequestBody("text/plain; charset=utf-8".toMediaType()),
        )
        val part = okhttp3.MultipartBody.Part.createFormData(
            "photo", photo.name, photo.asRequestBody("image/jpeg".toMediaType())
        )
        val result = runCatching { rawApi.createMeal(parts, part) }
        val recorded = server.requests.poll(10, TimeUnit.SECONDS)
        println("RAW-DIAG request=${recorded?.requestLine} bodySize=${recorded?.bodySize}")
        result.exceptionOrNull()?.let { throw AssertionError("createMeal threw ${it::class.java.name}: ${it.message}", it) }
        println("RAW-DIAG response=${result.getOrNull()?.code()} body=${result.getOrNull()?.body()?.mealId}")
    }

    @Test
    fun `a queued meal with a photo actually POSTs to the server`() = runTest {
        val uuid = UUID.randomUUID().toString()
        store[uuid] = queuedRow(uuid)

        val outcome = repository.uploadOne(uuid)

        val recorded = server.requests.poll(10, TimeUnit.SECONDS)
        assertNotNull(
            "NO REQUEST REACHED THE SERVER. outcome=$outcome failures=$failures " +
                "row=${store[uuid]?.status}",
            recorded,
        )
        assertEquals("POST /api/v1/meals HTTP/1.1", recorded!!.requestLine)
        assertTrue("bearer token missing", recorded.headers["authorization"] == "Bearer device-jwt")
        assertTrue(
            "multipart body looks empty: ${recorded.bodySize}",
            recorded.bodySize > 180_000,
        )
        assertTrue("client_uuid part missing", recorded.body.contains(uuid))
        assertTrue("eaten_at offset wrong: ${recorded.body.take(2000)}", recorded.body.contains("+02:00"))
        assertEquals(UploadOutcome.DONE, outcome)
        assertEquals(LocalMealStatus.PENDING, store[uuid]?.status)
    }

    @Test
    fun `a queued meal with no photo at all still POSTs`() = runTest {
        val uuid = UUID.randomUUID().toString()
        store[uuid] = queuedRow(uuid).copy(photoPath = null)

        val outcome = repository.uploadOne(uuid)
        val recorded = server.requests.poll(10, TimeUnit.SECONDS)
        assertNotNull("NO REQUEST (photo-less path). outcome=$outcome failures=$failures", recorded)
        assertEquals(UploadOutcome.DONE, outcome)
    }

    @Test
    fun `a grouped final shot POSTs group_id and group_size`() = runTest {
        val uuid = UUID.randomUUID().toString()
        val sitting = UUID.randomUUID().toString()
        store[uuid] = queuedRow(uuid).copy(sittingId = sitting, groupId = sitting, groupSize = 3)

        val outcome = repository.uploadOne(uuid)
        val recorded = server.requests.poll(10, TimeUnit.SECONDS)
        assertNotNull("NO REQUEST (grouped path). outcome=$outcome failures=$failures", recorded)
        assertTrue(recorded!!.body.contains("group_size"))
        assertEquals(UploadOutcome.DONE, outcome)
    }
}
