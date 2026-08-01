package dev.picweight.android.update

import android.content.Context
import android.util.Log
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.picweight.android.data.repository.AuthRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.File
import java.io.IOException
import javax.inject.Inject
import javax.inject.Singleton

/** Fetches the APK the server advertised into a file this app can read. */
interface ApkDownloader {

    /**
     * Downloads [available] and returns the file it landed in.
     *
     * [onProgress] is called with (bytes so far, total bytes) as the body streams;
     * callers use it to drive a progress bar and should expect it on an IO thread.
     * Throws [IOException] for anything that goes wrong on the wire.
     */
    suspend fun download(
        available: UpdateState.Available,
        onProgress: (Long, Long) -> Unit,
    ): File
}

/**
 * [ApkDownloader] over the app's existing OkHttp client.
 *
 * Deliberately the *same* client the API uses: it already carries the base-URL
 * rewrite, the session header and the timeouts that were tuned for this server. The
 * APK endpoint is public, so the Authorization header it adds is surplus rather than
 * required — but sharing the client means a self-hosted server behind a proxy that
 * needs anything unusual only has to be taught once.
 */
@Singleton
class OkHttpApkDownloader @Inject constructor(
    @param:ApplicationContext private val context: Context,
    private val client: OkHttpClient,
    private val auth: AuthRepository,
) : ApkDownloader {

    override suspend fun download(
        available: UpdateState.Available,
        onProgress: (Long, Long) -> Unit,
    ): File = withContext(Dispatchers.IO) {
        val url = resolve(available.downloadPath)

        // App-specific cache, never external storage: an APK on shared storage is
        // world-readable and can be swapped between the digest check and the install.
        // Here the file is inside the app sandbox for its whole life, and the installer
        // is fed it by streaming through our own process rather than by URI.
        val directory = File(context.cacheDir, CACHE_DIR).apply { mkdirs() }
        // Nothing here is worth keeping across attempts — a stale half-download would
        // only ever fail its digest check, more slowly.
        directory.listFiles()?.forEach { it.delete() }
        val target = File(directory, "picweight-${available.versionCode}.apk")

        val response = client.newCall(Request.Builder().url(url).build()).execute()
        response.use {
            if (!it.isSuccessful) {
                throw IOException("Server returned HTTP ${it.code} for ${available.downloadPath}")
            }
            val body = it.body
            val total = available.sizeBytes
            var written = 0L

            body.byteStream().use { input ->
                target.outputStream().use { output ->
                    val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                    while (true) {
                        val read = input.read(buffer)
                        if (read <= 0) break
                        written += read
                        // Bounded by the advertised size. Without this a server that
                        // answered with an endless stream would fill the cache
                        // partition; with it the worst case is one APK's worth of
                        // wasted bytes, and the digest check rejects the result anyway.
                        if (written > total) {
                            throw IOException(
                                "Download exceeded the advertised $total bytes — refusing to keep reading"
                            )
                        }
                        output.write(buffer, 0, read)
                        onProgress(written, total)
                    }
                }
            }

            if (written != total) {
                throw IOException("Download stopped at $written of $total bytes")
            }
        }

        Log.i(TAG, "Fetched ${target.length()} bytes of ${available.versionName} to ${target.name}")
        target
    }

    /**
     * Turns the server-supplied download path into an absolute URL, and refuses one
     * that leaves the configured server.
     *
     * The path is data from the network, and this is the step where it would otherwise
     * become "which host do we download executable code from". Pinning the host to the
     * server the user typed on the login screen means a mangled or hostile
     * `download_path` can at worst point at a different file on a server that is
     * already trusted with everything else in the app.
     */
    private fun resolve(downloadPath: String): okhttp3.HttpUrl {
        val serverUrl = auth.getServerUrl()
            ?: throw IOException("No server configured — can't download an update")
        val base = serverUrl.trimEnd('/').toHttpUrlOrNull()
            ?: throw IOException("Configured server URL isn't a URL: $serverUrl")
        val resolved = base.resolve(downloadPath)
            ?: throw IOException("Server advertised an unusable download path: $downloadPath")
        if (resolved.host != base.host) {
            throw IOException(
                "Server pointed the update at ${resolved.host}, which isn't ${base.host} — refusing"
            )
        }
        // Pin the scheme too, not just the host. `download_path` is attacker-shaped
        // input in the threat model this feature lives in — it decides where the app
        // fetches code from — and a path of "http://<same-host>/x" against an https
        // base passes the host check while silently downgrading to cleartext. The
        // digest and signature gates would still hold, but the point of this method
        // is that the bytes cannot come from anywhere other than the configured
        // server, and "same host, no TLS" is somewhere else.
        if (resolved.scheme != base.scheme) {
            throw IOException(
                "Server tried to switch the update from ${base.scheme} to ${resolved.scheme} — refusing"
            )
        }
        return resolved
    }

    private companion object {
        const val TAG = "ApkDownloader"
        const val CACHE_DIR = "updates"
    }
}
