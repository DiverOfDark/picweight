package dev.picweight.android.sync

import android.content.Context
import android.util.Log
import androidx.hilt.work.HiltWorker
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import dagger.assisted.Assisted
import dagger.assisted.AssistedInject
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.data.repository.UploadOutcome

/**
 * Uploads one captured photo.
 *
 * One worker per photo, with exponential backoff and `NetworkType.CONNECTED` — so
 * airplane mode is not an error path, it is just a constraint that has not been met
 * yet. Replay is free: `client_uuid` is the server's idempotency key, so a retry after
 * a response that never made it back cannot create a second meal (PRD §8).
 */
@HiltWorker
class MealUploadWorker @AssistedInject constructor(
    @Assisted appContext: Context,
    @Assisted params: WorkerParameters,
    private val meals: MealRepository,
) : CoroutineWorker(appContext, params) {

    override suspend fun doWork(): Result {
        val clientUuid = inputData.getString(SyncScheduler.KEY_CLIENT_UUID)
            ?: return Result.failure()

        return when (meals.uploadOne(clientUuid)) {
            UploadOutcome.DONE -> Result.success()
            UploadOutcome.RETRY -> {
                Log.i(TAG, "Retrying upload of $clientUuid (attempt ${runAttemptCount + 1})")
                Result.retry()
            }
            // The row is already marked failed with a readable reason; retrying a payload
            // the server has rejected on its merits would only burn battery.
            UploadOutcome.GIVE_UP -> Result.failure()
        }
    }

    private companion object {
        const val TAG = "MealUploadWorker"
    }
}
