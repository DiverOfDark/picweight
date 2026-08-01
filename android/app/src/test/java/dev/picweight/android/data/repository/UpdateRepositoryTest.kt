package dev.picweight.android.data.repository

import android.content.Intent
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.ClientVersionResponse
import dev.picweight.android.ui.common.FailureKind
import dev.picweight.android.update.ApkDownloader
import dev.picweight.android.update.ApkInstaller
import dev.picweight.android.update.ApkVerifier
import dev.picweight.android.update.AppIdentity
import dev.picweight.android.update.FakeSignatures
import dev.picweight.android.update.InstallOutcome
import dev.picweight.android.update.InstallOutcomes
import dev.picweight.android.update.InstallState
import dev.picweight.android.update.RunningVersion
import dev.picweight.android.update.UpdateState
import io.mockk.coEvery
import io.mockk.coVerify
import io.mockk.every
import io.mockk.mockk
import kotlinx.coroutines.test.runTest
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import retrofit2.HttpException
import retrofit2.Response
import java.io.File
import java.io.IOException
import java.net.UnknownHostException
import java.security.MessageDigest

/**
 * The whole update path, minus the two pieces that need a device.
 *
 * The assertions that matter most are the negative ones: a download whose digest is
 * wrong, or whose signer is wrong, must reach [ApkInstaller] **zero** times. Android
 * would reject a differently-signed update on its own, but "we relied on the platform"
 * is not a property this test can check and not a property a reviewer can see.
 */
class UpdateRepositoryTest {

    @get:Rule
    val temp = TemporaryFolder()

    private val api = mockk<PicweightApi>()
    private val auth = mockk<AuthRepository>()
    private val running = RunningVersion(versionCode = 42, versionName = "master+b5fcc95")
    private val ourSigner = setOf("aa11")

    private val apkBytes = "a plausible apk".toByteArray()
    private val apkSha = MessageDigest.getInstance("SHA-256").digest(apkBytes)
        .joinToString("") { "%02x".format(it) }

    init {
        every { auth.getServerUrl() } returns "https://picweight.example.com"
    }

    private fun response(
        available: Boolean = true,
        versionCode: Int = 43,
        versionName: String = "master+deadbee",
        sha256: String = apkSha,
        sizeBytes: Long = apkBytes.size.toLong(),
        downloadPath: String = "/picweight.apk",
    ): ClientVersionResponse = ClientVersionResponse()
        .available(available)
        .versionCode(versionCode)
        .versionName(versionName)
        .sha256(sha256)
        .sizeBytes(sizeBytes)
        .downloadPath(downloadPath)

    private fun repository(
        downloader: ApkDownloader = FakeDownloader { file(apkBytes) },
        installer: FakeInstaller = FakeInstaller(),
        archiveSigner: Set<String>? = ourSigner,
        archivePackage: String = "dev.picweight.android",
    ): Pair<UpdateRepository, FakeInstaller> {
        val signatures = FakeSignatures(
            running = AppIdentity("dev.picweight.android", 42, ourSigner),
            archive = archiveSigner?.let { AppIdentity(archivePackage, 43, it) },
        )
        return UpdateRepository(
            api = api,
            auth = auth,
            running = running,
            downloader = downloader,
            verifier = ApkVerifier(signatures),
            installer = installer,
            outcomes = InstallOutcomes(),
        ) to installer
    }

    private fun file(bytes: ByteArray): File =
        File(temp.root, "picweight-43.apk").apply { writeBytes(bytes) }

    // ---- the check --------------------------------------------------------

    @Test
    fun `a strictly newer server build becomes Available`() = runTest {
        coEvery { api.getClientVersion() } returns response(versionCode = 43)
        val (repo, _) = repository()

        val state = repo.check()

        val available = state as? UpdateState.Available ?: error("expected Available, got $state")
        assertEquals(43, available.versionCode)
        assertEquals("/picweight.apk", available.downloadPath)
        assertEquals(state, repo.state.value)
    }

    @Test
    fun `the same build is up to date`() = runTest {
        coEvery { api.getClientVersion() } returns response(versionCode = 42)
        val (repo, _) = repository()
        assertEquals(UpdateState.UpToDate, repo.check())
    }

    @Test
    fun `a backend-only deployment is up to date rather than an error`() = runTest {
        // The literal `available: false` payload the server sends when it bundles no APK.
        coEvery { api.getClientVersion() } returns response(
            available = false,
            versionCode = 0,
            versionName = "",
            sha256 = "",
            sizeBytes = 0L,
        )
        val (repo, _) = repository()
        assertEquals(UpdateState.UpToDate, repo.check())
    }

    @Test
    fun `a server too old to have the endpoint is up to date, not a failure`() = runTest {
        coEvery { api.getClientVersion() } throws HttpException(
            Response.error<Unit>(404, "".toResponseBody("application/json".toMediaType()))
        )
        val (repo, _) = repository()
        assertEquals(UpdateState.UpToDate, repo.check())
    }

    @Test
    fun `an outage is a classified failure and never claims to be up to date`() = runTest {
        coEvery { api.getClientVersion() } throws UnknownHostException("picweight.example.com")
        val (repo, _) = repository()

        val failed = repo.check() as? UpdateState.Failed ?: error("expected Failed")
        assertEquals(FailureKind.OFFLINE, failed.failure.kind)
    }

    @Test
    fun `a 500 is a failure, not a silent up to date`() = runTest {
        coEvery { api.getClientVersion() } throws HttpException(
            Response.error<Unit>(500, "".toResponseBody("application/json".toMediaType()))
        )
        val (repo, _) = repository()

        val failed = repo.check() as? UpdateState.Failed ?: error("expected Failed")
        assertEquals(FailureKind.HTTP, failed.failure.kind)
    }

    @Test
    fun `the app-start check does nothing before a server is configured`() = runTest {
        every { auth.getServerUrl() } returns null
        val (repo, _) = repository()

        repo.checkQuietly()

        // `api` is a strict mockk with no stub for getClientVersion: had it been called,
        // this would have thrown rather than left the state Unknown.
        assertEquals(UpdateState.Unknown, repo.state.value)
    }

    @Test
    fun `the app-start check does not re-ask the server within the throttle window`() = runTest {
        coEvery { api.getClientVersion() } returns response(versionCode = 42)
        val (repo, _) = repository()

        repo.checkQuietly()
        assertEquals(UpdateState.UpToDate, repo.state.value)

        // Second foregrounding a moment later: still the same answer, and cheap.
        repo.checkQuietly()
        coVerify(exactly = 1) { api.getClientVersion() }
    }

    // ---- verification gates the installer ---------------------------------

    @Test
    fun `a verified download reaches the installer`() = runTest {
        val (repo, installer) = repository()

        repo.downloadAndInstall(available())

        assertEquals(1, installer.installs)
        assertEquals(InstallState.AwaitingConfirmation, repo.install.value)
    }

    @Test
    fun `a download whose digest does not match is never installed`() = runTest {
        val (repo, installer) = repository(
            downloader = FakeDownloader { file("something else entirely".toByteArray()) }
        )

        repo.downloadAndInstall(available())

        assertEquals(0, installer.installs)
        val refused = repo.install.value as? InstallState.Refused ?: error("expected Refused")
        assertTrue(refused.reason.contains("checksum"))
    }

    @Test
    fun `a download signed by a different key is never installed`() = runTest {
        val (repo, installer) = repository(archiveSigner = setOf("ff99"))

        repo.downloadAndInstall(available())

        assertEquals(0, installer.installs)
        val refused = repo.install.value as? InstallState.Refused ?: error("expected Refused")
        assertTrue(refused.reason.contains("signed by a different key"))
    }

    @Test
    fun `an APK for another package is never installed`() = runTest {
        val (repo, installer) = repository(archivePackage = "com.example.other")

        repo.downloadAndInstall(available())

        assertEquals(0, installer.installs)
        assertTrue(repo.install.value is InstallState.Refused)
    }

    @Test
    fun `a refused download is deleted rather than left in the cache`() = runTest {
        val downloaded = file("something else entirely".toByteArray())
        val (repo, _) = repository(downloader = FakeDownloader { downloaded })

        repo.downloadAndInstall(available())

        assertTrue("a file that failed verification must not survive", !downloaded.exists())
    }

    @Test
    fun `a failed download never reaches the verifier or the installer`() = runTest {
        val (repo, installer) = repository(
            downloader = FakeDownloader { throw IOException("connection reset") }
        )

        repo.downloadAndInstall(available())

        assertEquals(0, installer.installs)
        assertTrue(repo.install.value is InstallState.Failed)
    }

    @Test
    fun `nothing is downloaded until the install permission exists`() = runTest {
        val downloader = FakeDownloader { file(apkBytes) }
        val (repo, installer) = repository(
            downloader = downloader,
            installer = FakeInstaller(permitted = false),
        )

        repo.downloadAndInstall(available())

        assertEquals(0, downloader.calls)
        assertEquals(0, installer.installs)
        assertEquals(InstallState.PermissionRequired, repo.install.value)
    }

    @Test
    fun `download progress is reported as it streams`() = runTest {
        // Sampled rather than collected: the repository publishes into a StateFlow, and
        // reading it the moment each chunk is reported is what the progress bar does.
        val seen = mutableListOf<Float>()
        lateinit var repo: UpdateRepository
        val downloader = FakeDownloader(
            progress = listOf(4L, 10L, 15L),
            onEach = { (repo.install.value as? InstallState.Downloading)?.let { seen += it.fraction } },
        ) { file(apkBytes) }
        repo = repository(downloader = downloader).first

        repo.downloadAndInstall(available())

        assertEquals(listOf(4f / 15f, 10f / 15f, 1f), seen)
    }

    // ---- platform callbacks -----------------------------------------------

    @Test
    fun `declining at the system prompt is not an error`() = runTest {
        val (repo, _) = repository()
        repo.downloadAndInstall(available())

        repo.onInstallOutcome(InstallOutcome.Declined)

        assertEquals(InstallState.Declined, repo.install.value)
    }

    @Test
    fun `a platform install failure is surfaced with its own message`() = runTest {
        val (repo, _) = repository()
        repo.downloadAndInstall(available())

        repo.onInstallOutcome(InstallOutcome.Failed("INSTALL_FAILED_VERSION_DOWNGRADE"))

        val failed = repo.install.value as? InstallState.Failed ?: error("expected Failed")
        assertEquals("INSTALL_FAILED_VERSION_DOWNGRADE", failed.reason)
    }

    @Test
    fun `dismissing returns to idle so the row can be retried`() = runTest {
        val (repo, _) = repository()
        repo.downloadAndInstall(available())
        repo.onInstallOutcome(InstallOutcome.Declined)

        repo.dismissInstall()

        assertEquals(InstallState.Idle, repo.install.value)
    }

    private fun available() = UpdateState.Available(
        versionName = "master+deadbee",
        versionCode = 43,
        sizeBytes = apkBytes.size.toLong(),
        sha256 = apkSha,
        downloadPath = "/picweight.apk",
    )
}

/** An [ApkDownloader] that produces whatever the test wants, and counts being asked. */
private class FakeDownloader(
    private val progress: List<Long> = emptyList(),
    private val onEach: () -> Unit = {},
    private val produce: () -> File,
) : ApkDownloader {

    var calls = 0
        private set

    override suspend fun download(
        available: UpdateState.Available,
        onProgress: (Long, Long) -> Unit,
    ): File {
        calls++
        progress.forEach { soFar ->
            onProgress(soFar, available.sizeBytes)
            onEach()
        }
        return produce()
    }
}

/** An [ApkInstaller] that records rather than installs — the thing under test is the count. */
private class FakeInstaller(private val permitted: Boolean = true) : ApkInstaller {

    var installs = 0
        private set

    override fun canInstall(): Boolean = permitted

    override suspend fun install(file: File) {
        installs++
    }

    override fun permissionSettingsIntent(): Intent =
        throw UnsupportedOperationException("not needed on the JVM")
}
