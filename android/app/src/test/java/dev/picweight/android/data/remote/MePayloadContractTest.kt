package dev.picweight.android.data.remote

import com.fasterxml.jackson.core.JsonProcessingException
import com.fasterxml.jackson.databind.JsonMappingException
import dev.picweight.android.data.remote.model.DayStatus
import dev.picweight.android.data.remote.model.MeResponse
import dev.picweight.android.data.remote.model.Sex
import dev.picweight.android.data.remote.model.UserResponse
import dev.picweight.android.di.AppModule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.time.LocalDate
import java.time.OffsetDateTime
import java.time.ZoneOffset

/**
 * `/api/v1/me` payloads, shared with [dev.picweight.android.ui.common.ApiFailuresTest]
 * so both sides of the bug are pinned against the same bytes.
 */
internal object MeFixtures {

    /** What the deployed pod emits now: RFC 3339, offset present. */
    const val RFC3339_UTC = "2026-08-01T13:32:33.441427539Z"

    /**
     * What it emitted before the backend was fixed — the literal string read off the
     * live cluster. `chrono::NaiveDateTime` serialises without any offset, while the
     * OpenAPI document declared the field `format: date-time`, which is RFC 3339, where
     * the offset is mandatory. Jackson's `OffsetDateTime` deserializer therefore threw,
     * `/api/v1/me` died mid-parse, and the two calls queued behind it were never issued.
     */
    const val OFFSETLESS = "2026-08-01T13:32:33.441427539"

    /** A realistic body, with every timestamp under test parameterised. */
    fun me(
        createdAt: String = RFC3339_UTC,
        targetsComputedAt: String = "2026-08-01T09:14:02.117Z",
    ): String = """
        {
          "user": {
            "id": "01J8Z2Q0000000000000000000",
            "email": "diverofdark@gmail.com",
            "display_name": "Vladimir",
            "created_at": "$createdAt"
          },
          "profile": {
            "sex": "male",
            "birth_date": "1986-04-17",
            "height_cm": 183.0,
            "activity_factor": 1.375,
            "goal_type": "lose",
            "target_weight_kg": 82.0,
            "current_weight_kg": 91.4,
            "rate_kg_per_week": 0.5,
            "target_kcal": 2050.0,
            "target_protein_g": 165.0,
            "target_fat_g": 66.0,
            "target_carbs_g": 190.0,
            "calibration_factor": 1.0,
            "targets_computed_at": "$targetsComputedAt",
            "timezone": "Europe/Paris"
          },
          "today": {
            "date": "2026-08-01",
            "consumed_kcal": 1450.0,
            "target_kcal": 2050.0,
            "remaining_kcal": 600.0,
            "consumed_protein_g": 82.0,
            "target_protein_g": 165.0,
            "remaining_protein_g": 83.0,
            "consumed_fat_g": 50.0,
            "target_fat_g": 66.0,
            "consumed_carbs_g": 120.0,
            "target_carbs_g": 190.0,
            "meals_logged": 3,
            "status": "tight"
          },
          "version": "master"
        }
    """.trimIndent()
}

/**
 * The contract test that would have caught the bug.
 *
 * It runs a realistic `/api/v1/me` body through the **generated** Jackson models — the
 * ones under `app/build/generated/openapi`, produced by the `openApiGenerate` task — and
 * through the app's own [AppModule.provideObjectMapper] configuration, so it exercises
 * exactly the deserialisation path Retrofit uses at runtime rather than a lookalike.
 *
 * The failure it guards against is not a network failure. The server answered 200; the
 * phone simply could not read the answer, which is a client/server contract mismatch and
 * is invisible in server logs.
 */
class MePayloadContractTest {

    private val mapper = AppModule.provideObjectMapper()

    @Test
    fun `the payload the server emits today deserialises through the generated models`() {
        val me = mapper.readValue(MeFixtures.me(), MeResponse::class.java)

        assertEquals("01J8Z2Q0000000000000000000", me.user.id)
        assertEquals(
            OffsetDateTime.parse("2026-08-01T13:32:33.441427539Z"),
            me.user.createdAt,
        )
        // The instant is UTC and nothing was rounded away on the way in.
        assertEquals(ZoneOffset.UTC, me.user.createdAt.offset)
        assertEquals(441_427_539, me.user.createdAt.nano)

        // The nullable date-time on the profile is the same contract.
        val profile = me.profile ?: throw AssertionError("the fixture supplies a profile")
        val computedAt = profile.targetsComputedAt
            ?: throw AssertionError("the fixture supplies targets_computed_at")
        assertEquals(ZoneOffset.UTC, computedAt.offset)

        assertEquals(Sex.MALE, profile.sex)
        assertEquals(DayStatus.TIGHT, me.today.status)
        assertEquals("master", me.version)
    }

    @Test
    fun `a bare calendar day is not a timestamp and must stay offset-free`() {
        val me = mapper.readValue(MeFixtures.me(), MeResponse::class.java)

        // `birth_date` and `today.date` are `format: date`, not `date-time`. If a future
        // "fix" ever gives every timestamp-shaped field an offset, these two must not
        // acquire one — they are calendar days, and a day has no instant.
        val profile = me.profile ?: throw AssertionError("the fixture supplies a profile")
        assertEquals(LocalDate.of(1986, 4, 17), profile.birthDate)
        assertEquals(LocalDate.of(2026, 8, 1), me.today.date)
    }

    /**
     * The regression pin, and a warning to the next person here.
     *
     * **Do not make this parse.** Not by registering a lenient deserializer, not with an
     * `ObjectMapper` feature, not by widening the generated field. An instant with no
     * offset is genuinely ambiguous, and the only way to accept it is to guess a zone —
     * at which point every meal silently shifts by the phone's UTC offset and days bucket
     * into the wrong local day (PRD §8, `timezone_offset`). The fix belongs on the server,
     * which now emits `Z`; this test exists so the client never "helpfully" papers over a
     * server that regresses.
     */
    @Test
    fun `an offset-less instant is rejected - this is the exact payload that broke the phone`() {
        val thrown = failureFor(MeFixtures.me(createdAt = MeFixtures.OFFSETLESS))

        assertTrue(
            "expected a Jackson parse failure, got ${thrown.javaClass.name}",
            thrown is JsonProcessingException,
        )
        assertEquals("user.created_at", pathOf(thrown))
        assertTrue(
            "the offending value belongs in the message: ${thrown.message}",
            thrown.message.orEmpty().contains(MeFixtures.OFFSETLESS),
        )
    }

    @Test
    fun `the same rejection applies to every date-time field, not just the one that broke`() {
        val thrown = failureFor(MeFixtures.me(targetsComputedAt = MeFixtures.OFFSETLESS))

        assertTrue(thrown is JsonProcessingException)
        assertEquals("profile.targets_computed_at", pathOf(thrown))
    }

    @Test
    fun `RFC 3339 requires an offset, not specifically Z`() {
        // A server in a non-UTC deployment is still compliant; the app must accept it and
        // normalise correctly, so this asserts the instant rather than the text.
        val me = mapper.readValue(
            MeFixtures.me(createdAt = "2026-08-01T15:32:33.441427539+02:00"),
            MeResponse::class.java,
        )
        assertEquals(
            OffsetDateTime.parse("2026-08-01T13:32:33.441427539Z").toInstant(),
            me.user.createdAt.toInstant(),
        )
    }

    @Test
    fun `the generated field is an OffsetDateTime, which is why the offset is mandatory`() {
        // Pins the generator configuration (`dateLibrary=java8`, `serializationLibrary=
        // jackson`). If this ever becomes LocalDateTime the offset-less payload would
        // start parsing again — silently, and wrongly.
        assertEquals(
            OffsetDateTime::class.java,
            UserResponse::class.java.getDeclaredField("createdAt").type,
        )
    }

    @Test
    fun `a parse failure is an IOException, which is why it used to read as offline`() {
        // Not incidental: `JsonProcessingException extends IOException`, so any handler
        // that tests `is IOException` first classifies a 200-with-an-unreadable-body as a
        // lost connection. `ApiFailures` tests the parse case first for this reason.
        assertTrue(failureFor(MeFixtures.me(createdAt = MeFixtures.OFFSETLESS)) is IOException)
    }

    private fun failureFor(json: String): Throwable =
        runCatching { mapper.readValue(json, MeResponse::class.java) }.exceptionOrNull()
            ?: throw AssertionError("expected the payload to be rejected, but it parsed")

    /** The dotted JSON path Jackson accumulated, e.g. `user.created_at`. */
    private fun pathOf(t: Throwable): String? = (t as? JsonMappingException)
        ?.path
        ?.joinToString(".") { it.fieldName ?: "[${it.index}]" }
}
