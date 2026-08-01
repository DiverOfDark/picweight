package dev.picweight.android.ui.common

import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.model.NameSource
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * What a queued meal says and shows.
 *
 * Both halves of this were actively misleading during the "forever QUEUED" bug. The
 * label said "Waiting for a connection" on a phone that was online, pointing every
 * diagnosis at the network, and the row showed a grey camera placeholder even though the
 * photo was sitting on disk — so the screen suggested nothing had been captured either.
 */
class MealPresentationTest {

    private fun meal(
        status: LocalMealStatus,
        error: String? = null,
        photoPath: String? = null,
        thumbnailUrl: String? = null,
    ) = MealEntity(
        clientUuid = "uuid",
        sittingId = "sitting",
        nameSource = NameSource.VISION.value,
        eatenAt = 0L,
        timezoneOffset = 0,
        photoPath = photoPath,
        thumbnailUrl = thumbnailUrl,
        status = status,
        error = error,
        createdAt = 0L,
        updatedAt = 0L,
    )

    // ---- copy --------------------------------------------------------------

    @Test
    fun `only a genuinely offline phone is told it is waiting for a connection`() {
        assertEquals(
            "Waiting for a connection",
            MealStatusCopy.title(LocalMealStatus.QUEUED, online = false),
        )
        assertEquals(
            "an online phone with a queued meal is uploading, not waiting",
            "Uploading…",
            MealStatusCopy.title(LocalMealStatus.QUEUED, online = true),
        )
    }

    @Test
    fun `the detail screen badge follows the same rule`() {
        // This screen had its own hand-rolled copy of the label, and so its own copy of
        // the bug: it said "waiting for a connection" regardless of connectivity.
        assertEquals(
            "waiting for a connection",
            MealStatusCopy.badge(LocalMealStatus.QUEUED, online = false),
        )
        assertEquals("uploading", MealStatusCopy.badge(LocalMealStatus.QUEUED, online = true))
    }

    @Test
    fun `no other status can produce the offline sentence, in either register`() {
        LocalMealStatus.entries
            .filter { it != LocalMealStatus.QUEUED }
            .forEach { status ->
                listOf(true, false).forEach { online ->
                    assertFalse(
                        "$status must not claim to be waiting for a connection",
                        MealStatusCopy.title(status, online).contains("connection"),
                    )
                    assertFalse(
                        "$status badge must not claim to be waiting for a connection",
                        MealStatusCopy.badge(status, online).contains("connection"),
                    )
                }
            }
    }

    @Test
    fun `a queued meal whose upload keeps failing reports the reason instead`() {
        val note = "Upload failed 5 times: SocketTimeoutException: Read timed out"
        val row = meal(LocalMealStatus.QUEUED, error = note)

        // Even offline: once we have a concrete reason, it beats the generic sentence.
        assertEquals(note, MealStatusCopy.detail(row, online = false))
        assertEquals(note, MealStatusCopy.detail(row, online = true))
        assertTrue("it should be coloured as the problem it is", MealStatusCopy.isProblem(row))
    }

    @Test
    fun `a healthy queued meal reports its plain status`() {
        val row = meal(LocalMealStatus.QUEUED)
        assertEquals("Uploading…", MealStatusCopy.detail(row, online = true))
        assertFalse(MealStatusCopy.isProblem(row))
    }

    @Test
    fun `a failed meal still leads with its error`() {
        val row = meal(LocalMealStatus.FAILED, error = "Photo file disappeared before upload")
        assertEquals("Photo file disappeared before upload", MealStatusCopy.detail(row, online = true))
        assertTrue(MealStatusCopy.isProblem(row))
    }

    @Test
    fun `a failed meal with no reason still says something`() {
        val row = meal(LocalMealStatus.FAILED)
        assertEquals("Analysis failed", MealStatusCopy.detail(row, online = true))
    }

    // ---- image -------------------------------------------------------------

    @Test
    fun `a meal that has not uploaded yet renders its local photo`() {
        val photo = File.createTempFile("shot", ".jpg").apply { writeBytes(ByteArray(8)) }
        try {
            val row = meal(LocalMealStatus.QUEUED, photoPath = photo.absolutePath)

            val model = MealImage.model(row) { error("must not need the server") }

            assertEquals(
                "the queue showed a grey placeholder while this file sat on disk",
                photo,
                model,
            )
        } finally {
            photo.delete()
        }
    }

    @Test
    fun `the local photo wins while both exist, so no network is needed`() {
        val photo = File.createTempFile("shot", ".jpg").apply { writeBytes(ByteArray(8)) }
        try {
            val row = meal(
                LocalMealStatus.PENDING,
                photoPath = photo.absolutePath,
                thumbnailUrl = "thumbs/abc.jpg",
            )
            assertEquals(photo, MealImage.model(row) { "https://example.test/$it" })
        } finally {
            photo.delete()
        }
    }

    @Test
    fun `once the local file is gone the server thumbnail takes over`() {
        val row = meal(LocalMealStatus.CONFIRMED, thumbnailUrl = "thumbs/abc.jpg")

        val model = MealImage.model(row) { path -> "https://example.test/$path" }

        assertEquals("https://example.test/thumbs/abc.jpg", model)
    }

    @Test
    fun `a stale photo path does not become a broken image`() {
        val row = meal(
            LocalMealStatus.QUEUED,
            photoPath = "/does/not/exist/shot.jpg",
            thumbnailUrl = null,
        )
        // Falls through to the server leg rather than handing Coil a dead path.
        assertNull(MealImage.model(row) { null })
    }

    @Test
    fun `a meal with neither image renders the placeholder`() {
        assertNull(MealImage.model(meal(LocalMealStatus.HELD)) { null })
    }
}
