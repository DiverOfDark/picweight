package dev.picweight.android

import android.app.Application
import android.content.Context
import androidx.hilt.work.HiltWorkerFactory
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import androidx.work.Configuration
import coil3.ImageLoader
import coil3.SingletonImageLoader
import coil3.disk.DiskCache
import coil3.memory.MemoryCache
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import dagger.hilt.android.HiltAndroidApp
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.data.repository.TokenRefreshManager
import dev.picweight.android.notifications.MealNotifier
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import okio.Path.Companion.toOkioPath
import java.io.File
import javax.inject.Inject

@HiltAndroidApp
class PicweightApplication : Application(), Configuration.Provider, SingletonImageLoader.Factory {

    @Inject lateinit var workerFactory: HiltWorkerFactory
    @Inject lateinit var okHttpClient: OkHttpClient
    @Inject lateinit var tokenRefreshManager: TokenRefreshManager
    @Inject lateinit var meals: MealRepository

    private val appScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override val workManagerConfiguration: Configuration
        get() = Configuration.Builder()
            .setWorkerFactory(workerFactory)
            .build()

    override fun onCreate() {
        super.onCreate()
        MealNotifier.createChannels(this)

        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) {
                appScope.launch {
                    // Renew the session before the UI (and Coil's thumbnail requests)
                    // start hitting the API.
                    tokenRefreshManager.ensureFreshToken()
                    // Then make sure nothing captured earlier is still sitting on the
                    // phone: a held shot from a sitting the user walked away from, or a
                    // queue that stalled while the device was offline.
                    runCatching { meals.resumeQueue() }
                }
            }
        })
    }

    override fun newImageLoader(context: Context): ImageLoader {
        return ImageLoader.Builder(context)
            .components {
                // Thumbnails are behind the session JWT, so they go through the
                // authenticated client rather than Coil's own.
                add(OkHttpNetworkFetcherFactory(callFactory = { okHttpClient }))
            }
            .memoryCache {
                MemoryCache.Builder().maxSizePercent(context, 0.25).build()
            }
            .diskCache {
                DiskCache.Builder()
                    // 768px JPEGs at ~80KB — a year of meals fits in well under this.
                    .directory(File(cacheDir, "image_cache").toOkioPath())
                    .maxSizeBytes(128L * 1024 * 1024)
                    .build()
            }
            .build()
    }
}
