package dev.picweight.android.update

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/**
 * The gate between a downloaded file and `PackageInstaller`.
 *
 * The digest half runs for real — [ApkVerifier] hashes an actual file on disk. The
 * certificate half runs against [FakeSignatures], because reading a signing
 * certificate needs a `PackageManager`; what is being tested here is the *decision*,
 * which is the part that can be wrong in a way that matters.
 */
class ApkVerifierTest {

    @get:Rule
    val temp = TemporaryFolder()

    /** `sha256("picweight")`, computed independently of the code under test. */
    private val payload = "picweight".toByteArray()
    private val payloadSha = "832cfa1b8404ca1aef032ed3bd9772f4c9a74ee8a77a7cd783bafe03163c1c51"

    private val ours = setOf("aa11", "bb22")

    private fun apk(bytes: ByteArray = payload): File =
        temp.newFile("picweight-43.apk").apply { writeBytes(bytes) }

    /** The digest of whatever we just wrote, so the two halves can be varied separately. */
    private fun digestOf(file: File): String =
        java.security.MessageDigest.getInstance("SHA-256").digest(file.readBytes()).toHex()

    private fun verifier(
        running: AppIdentity? = AppIdentity("dev.picweight.android", 42, ours),
        archive: AppIdentity? = AppIdentity("dev.picweight.android", 43, ours),
    ) = ApkVerifier(FakeSignatures(running, archive))

    // ---- (a) digest -------------------------------------------------------

    @Test
    fun `a file matching the advertised digest and signer passes`() {
        val file = apk()
        assertEquals(ApkVerdict.Ok, verifier().verify(file, digestOf(file)))
    }

    @Test
    fun `sha256 is computed over the real bytes`() {
        // Pins the hashing itself, not just its self-consistency: if this ever drifts,
        // every "verified" install would be verified against the wrong function.
        val file = apk()
        assertEquals(payloadSha, digestOf(file))
    }

    @Test
    fun `a tampered download fails the digest check`() {
        val file = apk()
        val advertised = digestOf(file)
        file.writeBytes(payload + "extra".toByteArray())

        val verdict = verifier().verify(file, advertised)
        val mismatch = verdict as? ApkVerdict.DigestMismatch
            ?: error("expected DigestMismatch, got $verdict")
        assertEquals(advertised, mismatch.expected)
        assertTrue(mismatch.actual != advertised)
    }

    @Test
    fun `digest comparison is case insensitive`() {
        val file = apk()
        assertEquals(ApkVerdict.Ok, verifier().verify(file, digestOf(file).uppercase()))
    }

    @Test
    fun `a missing file is a verdict, not an exception`() {
        val gone = File(temp.root, "never-downloaded.apk")
        assertTrue(verifier().verify(gone, payloadSha) is ApkVerdict.Unreadable)
    }

    // ---- order: (a) before (b) -------------------------------------------

    @Test
    fun `the digest is checked before the certificate is even read`() {
        // A file that would fail both must report the digest, and the signature reader
        // must not have been asked — nothing of unknown provenance is handed to the
        // platform parser.
        val signatures = FakeSignatures(
            running = AppIdentity("dev.picweight.android", 42, ours),
            archive = AppIdentity("dev.picweight.android", 43, setOf("ff99")),
        )
        val verdict = ApkVerifier(signatures).verify(apk(), "0".repeat(64))

        assertTrue(verdict is ApkVerdict.DigestMismatch)
        assertEquals(0, signatures.archiveReads)
        assertEquals(0, signatures.runningReads)
    }

    // ---- (b) signing certificate -----------------------------------------

    @Test
    fun `an APK signed by a different key is refused`() {
        val file = apk()
        val verdict = verifier(archive = AppIdentity("dev.picweight.android", 43, setOf("ff99")))
            .verify(file, digestOf(file))

        val mismatch = verdict as? ApkVerdict.SignatureMismatch
            ?: error("expected SignatureMismatch, got $verdict")
        assertEquals(ours, mismatch.running)
        assertEquals(setOf("ff99"), mismatch.downloaded)
    }

    @Test
    fun `a rotated key still matches while the old certificate is in the history`() {
        // The installed app's certificate history overlaps the new APK's signer; the
        // platform treats that as the same publisher and so does this.
        val file = apk()
        val verdict = verifier(
            running = AppIdentity("dev.picweight.android", 42, setOf("aa11", "cc33")),
            archive = AppIdentity("dev.picweight.android", 43, setOf("cc33")),
        ).verify(file, digestOf(file))
        assertEquals(ApkVerdict.Ok, verdict)
    }

    @Test
    fun `a correctly signed APK for a different package is refused`() {
        val file = apk()
        val verdict = verifier(archive = AppIdentity("com.example.other", 43, ours))
            .verify(file, digestOf(file))

        val wrong = verdict as? ApkVerdict.WrongPackage ?: error("expected WrongPackage, got $verdict")
        assertEquals("dev.picweight.android", wrong.expected)
        assertEquals("com.example.other", wrong.actual)
    }

    @Test
    fun `a download that is not a parseable APK is refused`() {
        val file = apk()
        assertTrue(
            verifier(archive = null).verify(file, digestOf(file)) is ApkVerdict.Unreadable
        )
    }

    @Test
    fun `an unreadable own certificate refuses rather than waves it through`() {
        val file = apk()
        assertTrue(
            verifier(running = null).verify(file, digestOf(file)) is ApkVerdict.Unreadable
        )
    }

    // ---- what the user is told -------------------------------------------

    @Test
    fun `only Ok has no refusal message`() {
        assertNull(ApkVerdict.Ok.refusalMessage())
        listOf(
            ApkVerdict.DigestMismatch("a", "b"),
            ApkVerdict.SignatureMismatch(setOf("a"), setOf("b")),
            ApkVerdict.WrongPackage("a", "b"),
            ApkVerdict.Unreadable("because"),
        ).forEach { assertNotNull("$it must explain itself", it.refusalMessage()) }
    }

    @Test
    fun `the signature refusal says which problem it is`() {
        val message = ApkVerdict.SignatureMismatch(setOf("a"), setOf("b")).refusalMessage()
        assertTrue(message!!.contains("signed by a different key"))
    }
}

/** [ApkSignatures] with no platform behind it, counting who asked. */
internal class FakeSignatures(
    private val running: AppIdentity?,
    private val archive: AppIdentity?,
) : ApkSignatures {
    var runningReads = 0
        private set
    var archiveReads = 0
        private set

    override fun runningApp(): AppIdentity? {
        runningReads++
        return running
    }

    override fun archive(file: File): AppIdentity? {
        archiveReads++
        return archive
    }
}
