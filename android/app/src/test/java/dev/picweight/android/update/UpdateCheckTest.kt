package dev.picweight.android.update

import dev.picweight.android.ui.common.FailureKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The rule that decides whether the app offers to install code fetched over the
 * network.
 *
 * Every one of these was a real failure mode before this feature existed: the
 * Dockerfile stamped every master build `versionCode 1`, so the only comparison the
 * app could ever make was 1 against 1 — permanently "up to date", silently, with no
 * way to tell that from a server that genuinely had nothing newer.
 */
class UpdateCheckTest {

    private val running = RunningVersion(versionCode = 42, versionName = "master+b5fcc95")

    private fun build(
        versionCode: Int,
        versionName: String = "master+deadbee",
        sha256: String = VALID_SHA,
        sizeBytes: Long = 25_801_464L,
        downloadPath: String = "/picweight.apk",
    ) = ServerBuild(versionName, versionCode, sha256, sizeBytes, downloadPath)

    // ---- the ordering rule ------------------------------------------------

    @Test
    fun `a strictly greater version code is offered`() {
        val state = UpdateCheck.decide(running, build(versionCode = 43))
        val available = state as? UpdateState.Available
            ?: error("expected Available, got $state")
        assertEquals(43, available.versionCode)
        assertEquals("master+deadbee", available.versionName)
        assertEquals(25_801_464L, available.sizeBytes)
    }

    @Test
    fun `an equal version code never prompts`() {
        assertEquals(UpdateState.UpToDate, UpdateCheck.decide(running, build(versionCode = 42)))
    }

    @Test
    fun `a lower version code is never offered as a downgrade`() {
        // A server rolled back to an older image would otherwise walk every client
        // backwards — and Android would refuse the install after the download anyway.
        assertEquals(UpdateState.UpToDate, UpdateCheck.decide(running, build(versionCode = 41)))
        assertEquals(UpdateState.UpToDate, UpdateCheck.decide(running, build(versionCode = 1)))
    }

    @Test
    fun `a server with no bundled APK is up to date, not an error`() {
        assertEquals(UpdateState.UpToDate, UpdateCheck.decide(running, null))
    }

    @Test
    fun `the local dev build is out-ranked by every real server build`() {
        // The Gradle fallback for a build with no -PversionCode.
        val dev = RunningVersion(versionCode = 1, versionName = "0.0.0-dev")
        assertTrue(UpdateCheck.decide(dev, build(versionCode = 2)) is UpdateState.Available)
    }

    // ---- an advertisement that cannot be verified is refused, not offered --

    @Test
    fun `a newer build with no usable digest is refused as a contract failure`() {
        val state = UpdateCheck.decide(running, build(versionCode = 43, sha256 = ""))
        val failed = state as? UpdateState.Failed ?: error("expected Failed, got $state")
        assertEquals(FailureKind.CONTRACT, failed.failure.kind)
        assertTrue(failed.failure.message.contains("not a digest"))
    }

    @Test
    fun `a digest of the wrong length is not a digest`() {
        val short = VALID_SHA.dropLast(1)
        assertTrue(UpdateCheck.decide(running, build(43, sha256 = short)) is UpdateState.Failed)
    }

    @Test
    fun `a digest with non-hex characters is not a digest`() {
        val notHex = "z".repeat(64)
        assertTrue(UpdateCheck.decide(running, build(43, sha256 = notHex)) is UpdateState.Failed)
    }

    @Test
    fun `an uppercase digest is still a digest`() {
        assertTrue(
            UpdateCheck.decide(running, build(43, sha256 = VALID_SHA.uppercase()))
                is UpdateState.Available
        )
    }

    @Test
    fun `a newer build with no size is refused`() {
        assertTrue(UpdateCheck.decide(running, build(43, sizeBytes = 0L)) is UpdateState.Failed)
    }

    @Test
    fun `a newer build with no download path is refused`() {
        assertTrue(UpdateCheck.decide(running, build(43, downloadPath = "  ")) is UpdateState.Failed)
    }

    @Test
    fun `an unverifiable advertisement that is not newer is still just up to date`() {
        // The `available: false` payload has an empty sha256 and size 0. Nothing about
        // it is newer, so it must not surface as a failure the user has to read.
        assertEquals(
            UpdateState.UpToDate,
            UpdateCheck.decide(running, build(versionCode = 0, sha256 = "", sizeBytes = 0L)),
        )
    }

    private companion object {
        /** Real 64-hex digest, from the sidecar the Docker build emits. */
        const val VALID_SHA = "d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349"
    }
}
