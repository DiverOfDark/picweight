package dev.picweight.android.sync

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import dagger.hilt.android.qualifiers.ApplicationContext
import java.util.concurrent.TimeUnit
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The upload queue's front door.
 *
 * Every photo is its own independent piece of work (PRD §5): a five-dish sitting is
 * five uploads that succeed or retry on their own, so one flaky request cannot sink
 * the other four.
 */
@Singleton
class SyncScheduler @Inject constructor(
    @param:ApplicationContext private val context: Context,
) {
    companion object {
        const val KEY_CLIENT_UUID = "client_uuid"
        const val EVENT_WORK_NAME = "picweight-meal-events"

        private const val UPLOAD_WORK_PREFIX = "picweight-upload:"

        /** WorkManager's floor is 10s; 30s keeps a dead network from being hammered. */
        private const val UPLOAD_BACKOFF_SECONDS = 30L
        private const val EVENT_BACKOFF_SECONDS = 15L

        private val CONNECTED = Constraints.Builder()
            .setRequiredNetworkType(NetworkType.CONNECTED)
            .build()

        fun uploadWorkName(clientUuid: String): String = UPLOAD_WORK_PREFIX + clientUuid
    }

    private val workManager: WorkManager get() = WorkManager.getInstance(context)

    /**
     * Queues one photo. Keyed on `client_uuid` and [ExistingWorkPolicy.KEEP], so a
     * double tap or a second foreground pass cannot produce two uploads — and even if
     * one slipped through, the server dedupes on the same key.
     */
    fun enqueueUpload(clientUuid: String) {
        val request = OneTimeWorkRequestBuilder<MealUploadWorker>()
            .setConstraints(CONNECTED)
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, UPLOAD_BACKOFF_SECONDS, TimeUnit.SECONDS)
            .setInputData(Data.Builder().putString(KEY_CLIENT_UUID, clientUuid).build())
            .addTag(TAG_UPLOAD)
            .build()

        workManager.enqueueUniqueWork(
            uploadWorkName(clientUuid),
            ExistingWorkPolicy.KEEP,
            request,
        )
    }

    /**
     * Starts (or leaves running) the single subscriber to `/api/v1/meals/events`.
     * One stream serves every in-flight meal, so N concurrent loops still cost one
     * connection.
     */
    fun enqueueEventStream() {
        val request = OneTimeWorkRequestBuilder<MealEventWorker>()
            .setConstraints(CONNECTED)
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, EVENT_BACKOFF_SECONDS, TimeUnit.SECONDS)
            .addTag(TAG_EVENTS)
            .build()

        workManager.enqueueUniqueWork(
            EVENT_WORK_NAME,
            ExistingWorkPolicy.KEEP,
            request,
        )
    }

    /** Cancels everything — used on logout, where the queue's contents are wiped too. */
    fun cancelAll() {
        workManager.cancelAllWorkByTag(TAG_UPLOAD)
        workManager.cancelUniqueWork(EVENT_WORK_NAME)
    }
}

private const val TAG_UPLOAD = "picweight-upload"
private const val TAG_EVENTS = "picweight-events"
