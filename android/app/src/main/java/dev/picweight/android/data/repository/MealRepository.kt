package dev.picweight.android.data.repository

import android.content.Context
import android.util.Log
import com.fasterxml.jackson.databind.ObjectMapper
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.picweight.android.data.local.DayStateEntity
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.local.MealItemEntity
import dev.picweight.android.data.local.NotifiedGroupEntity
import dev.picweight.android.data.local.RecentDishEntity
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.DayResponse
import dev.picweight.android.data.remote.model.DayState
import dev.picweight.android.data.remote.model.ErrorBody
import dev.picweight.android.data.remote.model.FoodResponse
import dev.picweight.android.data.remote.model.MealResponse
import dev.picweight.android.data.remote.model.MealStatus
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.data.remote.model.PatchMealItem
import dev.picweight.android.data.remote.model.PatchMealRequest
import dev.picweight.android.data.remote.model.ReanalyzeRequest
import dev.picweight.android.data.remote.model.RevisionsResponse
import dev.picweight.android.sync.SyncScheduler
import kotlinx.coroutines.flow.Flow
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.asRequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import retrofit2.HttpException
import java.io.File
import java.io.IOException
import java.time.Instant
import java.time.LocalDate
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.UUID
import javax.inject.Inject
import javax.inject.Singleton

/** What the upload queue should do next with a meal. */
enum class UploadOutcome {
    /** The server has it. Nothing left to do. */
    DONE,

    /** Transient — WorkManager should back off and try again. */
    RETRY,

    /** The server will never accept this payload; the row is marked failed. */
    GIVE_UP,
}

/**
 * Local-first meal store.
 *
 * The UI reads Room and only Room. Everything that touches the network goes through
 * here, writes what it learned back into Room, and lets the Flows fan the change out.
 * That inversion is what PRD §7's "Room as local source of truth; UI never blocks on
 * network" buys, and it is the reason airplane-mode capture is not a special case.
 */
@Singleton
class MealRepository @Inject constructor(
    private val dao: MealDao,
    private val api: PicweightApi,
    private val scheduler: SyncScheduler,
    private val mapper: ObjectMapper,
    @param:ApplicationContext private val context: Context,
) {
    companion object {
        private const val TAG = "MealRepository"

        /**
         * A shot is held only long enough to learn whether the sitting will gain another
         * photo. If the app dies mid-sitting, anything older than this is released on the
         * next foreground pass — a held meal must never become a lost meal (G5).
         */
        const val HOLD_GRACE_MS = 2 * 60 * 1000L

        /**
         * How many bounced attempts before the row stops saying "waiting" and starts
         * naming the reason. One transient failure is normal; two is a symptom.
         */
        const val NOISY_AFTER_ATTEMPTS = 2

        private val RFC3339: DateTimeFormatter = DateTimeFormatter.ISO_OFFSET_DATE_TIME
        private val TEXT = "text/plain; charset=utf-8".toMediaType()
        private val JPEG = "image/jpeg".toMediaType()
    }

    // ---- capture ----------------------------------------------------------

    /** Directory the camera writes into; files are deleted once uploaded. */
    fun captureDir(): File = File(context.filesDir, "captures").apply { mkdirs() }

    /** Minutes east of UTC right now — stamped on every meal so days bucket locally. */
    fun currentOffsetMinutes(): Int =
        ZoneId.systemDefault().rules.getOffset(Instant.now()).totalSeconds / 60

    /** Opens a sitting. One photo or five, the client id is generated up front (§5). */
    fun newSitting(): String = UUID.randomUUID().toString()

    /**
     * Persists one shot of a sitting and returns its `client_uuid`.
     *
     * The row lands in Room before this returns, so the meal survives a crash, a kill or
     * airplane mode from the instant the shutter fires. It is *held* rather than queued:
     * a shot only learns whether it belongs to a multi-dish sitting when the next shot
     * arrives (or does not), and both `group_id` and the `group_size` settle hint have to
     * be right at ingest time — the server accepts neither afterwards.
     */
    suspend fun addShot(
        sittingId: String,
        photo: File?,
        dishName: String?,
        comment: String?,
        nameSource: NameSource,
        mealType: String? = null,
        eatenAt: Instant = Instant.now(),
        timezoneOffsetMinutes: Int = currentOffsetMinutes(),
    ): String {
        // A second shot proves this is a sitting: release the previous one as a member.
        releaseHeld(sittingId, asGroupMember = true, groupSize = null)

        val now = System.currentTimeMillis()
        val clientUuid = UUID.randomUUID().toString()
        dao.upsert(
            MealEntity(
                clientUuid = clientUuid,
                sittingId = sittingId,
                dishName = dishName?.takeIf { it.isNotBlank() },
                comment = comment?.takeIf { it.isNotBlank() },
                nameSource = nameSource.value,
                mealType = mealType,
                eatenAt = eatenAt.toEpochMilli(),
                timezoneOffset = timezoneOffsetMinutes,
                photoPath = photo?.absolutePath,
                status = LocalMealStatus.HELD,
                createdAt = now,
                updatedAt = now,
            )
        )
        return clientUuid
    }

    /**
     * Ends a sitting: the final shot carries the `group_size` hint, which settles the
     * group the moment all N jobs are terminal instead of waiting out the 90s debounce.
     *
     * A sitting of one is sent with no `group_id` at all, so it gets the per-meal
     * notification that leads with its dish name rather than a "1 dish" summary (§6).
     */
    suspend fun closeSitting(sittingId: String) {
        val members = dao.bySitting(sittingId)
        if (members.isEmpty()) return
        val grouped = members.size > 1
        releaseHeld(
            sittingId = sittingId,
            asGroupMember = grouped,
            groupSize = if (grouped) members.size else null,
        )
    }

    /**
     * Releases anything left held by an app that died mid-sitting. Called on every
     * foreground pass; the debounce on the server is the second line of defence.
     */
    suspend fun releaseStaleHolds(olderThanMs: Long = HOLD_GRACE_MS) {
        val cutoff = System.currentTimeMillis() - olderThanMs
        dao.staleHeld(cutoff)
            .map { it.sittingId }
            .distinct()
            .forEach { closeSitting(it) }
    }

    private suspend fun releaseHeld(sittingId: String, asGroupMember: Boolean, groupSize: Int?) {
        val held = dao.heldInSitting(sittingId) ?: return
        val released = held.copy(
            groupId = if (asGroupMember) sittingId else null,
            groupSize = groupSize,
            status = LocalMealStatus.QUEUED,
            updatedAt = System.currentTimeMillis(),
        )
        dao.upsert(released)
        enqueueUpload(released.clientUuid)
    }

    /**
     * Hands one meal to WorkManager, loudly.
     *
     * The row is written QUEUED *before* this runs, and every caller wraps the release in
     * `runCatching`, so a throw from `enqueueUpload` used to strand a meal at QUEUED —
     * "Waiting for a connection" forever — leaving no trace anywhere. If the queue cannot
     * accept work, that is the single most important fact about this app's state.
     */
    private fun enqueueUpload(clientUuid: String) {
        runCatching { scheduler.enqueueUpload(clientUuid) }
            .onFailure { Log.e(TAG, "Could not enqueue the upload of $clientUuid", it) }
    }

    /** Re-enqueues everything the queue still owes the server. */
    suspend fun resumeQueue() {
        releaseStaleHolds()
        // Per-item isolation: one meal that cannot be enqueued must not abort the loop
        // and silently strand every meal behind it — nor skip the event stream below.
        dao.awaitingUpload()
            .filter { it.status == LocalMealStatus.QUEUED }
            .forEach { enqueueUpload(it.clientUuid) }
        if (dao.awaitingAnalysis().isNotEmpty()) {
            runCatching { scheduler.enqueueEventStream() }
                .onFailure { Log.e(TAG, "Could not start the meal event stream", it) }
        }
    }

    // ---- upload -----------------------------------------------------------

    /**
     * Uploads one queued meal. Safe to call repeatedly: `client_uuid` dedupes.
     *
     * [attempt] is WorkManager's `runAttemptCount` (0 on the first run). It exists so a
     * retry that will never succeed stops looking like one that is merely waiting for a
     * signal: from the second attempt on, the row carries the actual reason.
     *
     * Nothing thrown escapes. An upload that dies on something other than I/O is a bug,
     * and a bug in a worker that surfaces as "waiting for a connection" is a bug you
     * find in a week rather than in a minute — so it is logged at [Log.e] and written
     * onto the row where the user (and logcat) can see it.
     */
    suspend fun uploadOne(clientUuid: String, attempt: Int = 0): UploadOutcome = try {
        upload(clientUuid, attempt)
    } catch (e: kotlinx.coroutines.CancellationException) {
        // The worker was stopped, not broken. Never swallow this: it would break
        // structured concurrency and make a cancelled upload look like a failed one.
        throw e
    } catch (t: Throwable) {
        Log.e(TAG, "Upload of $clientUuid crashed on attempt ${attempt + 1}", t)
        runCatching {
            dao.markFailed(clientUuid, crashReason(t), System.currentTimeMillis())
        }.onFailure { Log.e(TAG, "Could not even record the crash for $clientUuid", it) }
        UploadOutcome.GIVE_UP
    }

    private suspend fun upload(clientUuid: String, attempt: Int): UploadOutcome {
        val meal = dao.byClientUuid(clientUuid) ?: return UploadOutcome.DONE
        if (meal.status.isUploaded) return UploadOutcome.DONE
        if (meal.status == LocalMealStatus.HELD) return UploadOutcome.RETRY

        val parts = buildMap {
            put("client_uuid", meal.clientUuid.toPart())
            put(
                "eaten_at",
                OffsetDateTime.ofInstant(
                    Instant.ofEpochMilli(meal.eatenAt),
                    ZoneOffset.ofTotalSeconds(meal.timezoneOffset * 60),
                ).format(RFC3339).toPart(),
            )
            put("timezone_offset", meal.timezoneOffset.toString().toPart())
            put("name_source", meal.nameSource.toPart())
            meal.dishName?.let { put("dish_name", it.toPart()) }
            meal.comment?.let { put("comment", it.toPart()) }
            meal.mealType?.let { put("meal_type", it.toPart()) }
            meal.groupId?.let { put("group_id", it.toPart()) }
            meal.groupSize?.let { put("group_size", it.toString().toPart()) }
        }

        val file = meal.photoPath?.let(::File)?.takeIf { it.isFile }
        if (meal.photoPath != null && file == null) {
            // The photo is gone (cache wipe, user cleared storage). Sending nothing would
            // silently turn a photographed meal into a manual entry, so fail loudly.
            dao.markFailed(clientUuid, "Photo file disappeared before upload", System.currentTimeMillis())
            return UploadOutcome.GIVE_UP
        }
        val photoPart = file?.let {
            MultipartBody.Part.createFormData("photo", it.name, it.asRequestBody(JPEG))
        }

        return try {
            val response = api.createMeal(parts, photoPart)
            val body = response.body()
            when {
                response.isSuccessful && body != null -> {
                    val now = System.currentTimeMillis()
                    dao.upsert(
                        meal.copy(
                            serverId = body.mealId,
                            status = LocalMealStatus.fromWire(body.status.value),
                            revision = body.revision,
                            // The photo deliberately survives the upload. `202 Accepted`
                            // carries no `thumbnail_url` — that only arrives with a later
                            // MealResponse — so deleting the file here opens a window,
                            // the whole length of the analysis, in which the meal has
                            // neither a local image nor a server one and renders as a
                            // grey placeholder. [applyServerMeal] drops it once the
                            // server's thumbnail actually exists.
                            error = null,
                            updatedAt = now,
                        )
                    )
                    scheduler.enqueueEventStream()
                    UploadOutcome.DONE
                }

                response.code() in RETRYABLE_CODES -> {
                    noteRetry(clientUuid, attempt, "the server returned HTTP ${response.code()}")
                    UploadOutcome.RETRY
                }

                else -> {
                    dao.markFailed(
                        clientUuid,
                        describe(response.code(), response.errorBody()?.string()),
                        System.currentTimeMillis(),
                    )
                    UploadOutcome.GIVE_UP
                }
            }
        } catch (e: IOException) {
            noteRetry(clientUuid, attempt, ioReason(e))
            UploadOutcome.RETRY
        } catch (e: HttpException) {
            if (e.code() in RETRYABLE_CODES) {
                noteRetry(clientUuid, attempt, "the server returned HTTP ${e.code()}")
                UploadOutcome.RETRY
            } else {
                dao.markFailed(clientUuid, describe(e.code(), e.message()), System.currentTimeMillis())
                UploadOutcome.GIVE_UP
            }
        }
    }

    /**
     * Records a bounced attempt on the row and in logcat.
     *
     * The row stays QUEUED so WorkManager keeps retrying — a meal is never dropped (G5)
     * — but from the second attempt onward it stops claiming to be waiting for a
     * connection and names what actually happened. The first bounce stays quiet at
     * [Log.i]: one lost request while walking into a lift is not an event.
     */
    private suspend fun noteRetry(clientUuid: String, attempt: Int, reason: String) {
        val attempts = attempt + 1
        if (attempts < NOISY_AFTER_ATTEMPTS) {
            Log.i(TAG, "Upload of $clientUuid deferred: $reason")
            return
        }
        val note = "Upload failed $attempts times: $reason"
        Log.w(TAG, "Upload of $clientUuid: $note")
        runCatching { dao.noteUploadError(clientUuid, note, System.currentTimeMillis()) }
            .onFailure { Log.e(TAG, "Could not record the upload failure for $clientUuid", it) }
    }

    /** Names an I/O failure in words a user can act on. */
    private fun ioReason(e: IOException): String =
        e.message?.takeIf { it.isNotBlank() }?.let { "${e.javaClass.simpleName}: $it" }
            ?: e.javaClass.simpleName

    private fun crashReason(t: Throwable): String {
        val detail = t.message?.takeIf { it.isNotBlank() }?.let { ": $it" }.orEmpty()
        return "Upload failed unexpectedly (${t.javaClass.simpleName}$detail)"
    }

    private fun describe(code: Int, rawBody: String?): String {
        val parsed = rawBody
            ?.takeIf { it.isNotBlank() }
            ?.let { runCatching { mapper.readValue(it, ErrorBody::class.java) }.getOrNull() }
        return parsed?.message ?: parsed?.error ?: "Server rejected the upload (HTTP $code)"
    }

    // ---- reads ------------------------------------------------------------

    fun flowMeal(clientUuid: String): Flow<MealEntity?> = dao.flowByClientUuid(clientUuid)

    fun flowItems(clientUuid: String): Flow<List<MealItemEntity>> = dao.flowItems(clientUuid)

    fun flowSitting(sittingId: String): Flow<List<MealEntity>> = dao.flowBySitting(sittingId)

    fun flowForLocalDay(date: LocalDate): Flow<List<MealEntity>> =
        dao.flowForLocalDay(date.toString())

    fun flowRecent(limit: Int): Flow<List<MealEntity>> = dao.flowRecent(limit)

    fun flowRecentDishes(limit: Int): Flow<List<RecentDishEntity>> = dao.flowRecentDishes(limit)

    fun flowDayState(date: LocalDate): Flow<DayStateEntity?> = dao.flowDayState(date.toString())

    suspend fun mealByServerId(serverId: String): MealEntity? = dao.byServerId(serverId)

    suspend fun mealByClientUuid(clientUuid: String): MealEntity? = dao.byClientUuid(clientUuid)

    suspend fun dayStateFor(date: LocalDate): DayStateEntity? = dao.dayState(date.toString())

    /** Records a status the stream reported without a full re-fetch. */
    suspend fun applyLocalStatus(meal: MealEntity, status: LocalMealStatus) {
        dao.upsert(meal.copy(status = status, updatedAt = System.currentTimeMillis()))
    }

    suspend fun revisions(serverId: String): RevisionsResponse = api.mealRevisions(serverId)

    // ---- server → Room ----------------------------------------------------

    /** Pulls one meal and folds it into Room. */
    suspend fun refreshMeal(serverId: String): MealEntity? {
        val response = runCatching { api.getMeal(serverId) }.getOrElse {
            // The exception itself, not just its message: a parse failure's message is
            // useless without the path, and its class is what distinguishes "no network"
            // from "this build can't read what the server sent".
            Log.w(TAG, "Refresh of $serverId failed", it)
            return null
        }
        applyServerMeal(response)
        return dao.byClientUuid(response.clientUuid)
    }

    /**
     * Merges a server meal into Room, preserving the fields only the phone knows —
     * which sitting it belonged to and whether its notification has already fired.
     */
    suspend fun applyServerMeal(remote: MealResponse) {
        val existing = dao.byClientUuid(remote.clientUuid)
        val totals = remote.totals
        // Hand the thumbnail over rather than dropping it: the local file is released
        // only once the server has one to show in its place, so the row never goes
        // imageless in between.
        val serverHasThumbnail = !remote.thumbnailUrl.isNullOrBlank()
        val keptPhotoPath = if (serverHasThumbnail) null else existing?.photoPath
        if (serverHasThumbnail) {
            existing?.photoPath?.let { runCatching { File(it).delete() } }
        }
        val entity = MealEntity(
            clientUuid = remote.clientUuid,
            serverId = remote.id,
            sittingId = existing?.sittingId ?: remote.groupId ?: remote.clientUuid,
            groupId = remote.groupId,
            groupSize = remote.groupSize ?: existing?.groupSize,
            dishName = remote.dishName ?: existing?.dishName,
            comment = remote.userComment ?: existing?.comment,
            nameSource = remote.nameSource.value,
            mealType = remote.mealType,
            eatenAt = remote.eatenAt.toInstant().toEpochMilli(),
            timezoneOffset = remote.timezoneOffset,
            photoPath = keptPhotoPath,
            thumbnailUrl = remote.thumbnailUrl,
            status = LocalMealStatus.fromWire(remote.status.value),
            revision = remote.revision,
            portionScale = remote.portionScale,
            kcal = totals.kcal,
            proteinG = totals.proteinG,
            fatG = totals.fatG,
            carbsG = totals.carbsG,
            error = remote.error,
            notified = existing?.notified ?: false,
            createdAt = existing?.createdAt ?: remote.createdAt.toInstant().toEpochMilli(),
            updatedAt = System.currentTimeMillis(),
        )
        val items = remote.items.orEmpty().map { item ->
            MealItemEntity(
                mealClientUuid = remote.clientUuid,
                position = item.position,
                serverId = item.id,
                name = item.name,
                grams = item.grams,
                kcal = item.kcal,
                proteinG = item.proteinG,
                fatG = item.fatG,
                carbsG = item.carbsG,
                gramsSource = item.gramsSource.value,
                macroSource = item.macroSource.value,
                confidence = item.confidence,
                reasoningNote = item.reasoningNote,
                barcode = item.barcode,
            )
        }
        dao.upsertWithItems(entity, items)
    }

    /** Refreshes one local day and caches its state; returns the full response. */
    suspend fun refreshDay(date: LocalDate, tzOffsetMinutes: Int? = currentOffsetMinutes()): DayResponse {
        val response = api.getDay(date.toString(), tzOffsetMinutes)
        cacheDay(response)
        response.meals.orEmpty().forEach { applyServerMeal(it) }
        return response
    }

    private suspend fun cacheDay(response: DayResponse) {
        val state = response.state
        dao.upsertDayState(
            DayStateEntity(
                date = state.date.toString(),
                consumedKcal = state.consumedKcal,
                targetKcal = state.targetKcal,
                remainingKcal = state.remainingKcal,
                consumedProteinG = state.consumedProteinG,
                targetProteinG = state.targetProteinG,
                remainingProteinG = state.remainingProteinG,
                consumedFatG = state.consumedFatG,
                targetFatG = state.targetFatG,
                consumedCarbsG = state.consumedCarbsG,
                targetCarbsG = state.targetCarbsG,
                mealsLogged = state.mealsLogged,
                status = state.status.value,
                verdict = response.verdict,
                updatedAt = System.currentTimeMillis(),
            )
        )
    }

    /** Caches the day state carried by an SSE completion frame — no extra round trip. */
    suspend fun cacheDayState(state: DayState) {
        val existing = dao.dayState(state.date.toString())
        dao.upsertDayState(
            DayStateEntity(
                date = state.date.toString(),
                consumedKcal = state.consumedKcal,
                targetKcal = state.targetKcal,
                remainingKcal = state.remainingKcal,
                consumedProteinG = state.consumedProteinG,
                targetProteinG = state.targetProteinG,
                remainingProteinG = state.remainingProteinG,
                consumedFatG = state.consumedFatG,
                targetFatG = state.targetFatG,
                consumedCarbsG = state.consumedCarbsG,
                targetCarbsG = state.targetCarbsG,
                mealsLogged = state.mealsLogged,
                status = state.status.value,
                verdict = existing?.verdict,
                updatedAt = System.currentTimeMillis(),
            )
        )
    }

    /** Refreshes the recent-dish chips. */
    suspend fun refreshRecentDishes(limit: Long = 8) {
        val dishes = api.recentDishes(limit).map {
            RecentDishEntity(
                dishNameNormalized = it.dishNameNormalized,
                dishName = it.dishName,
                lastEatenAt = it.lastEatenAt.toInstant().toEpochMilli(),
                occurrences = it.occurrences,
                lastMealId = it.lastMealId,
                kcal = it.totals.kcal,
                proteinG = it.totals.proteinG,
                fatG = it.totals.fatG,
                carbsG = it.totals.carbsG,
            )
        }
        dao.replaceRecentDishes(dishes)
    }

    /** Pulls the most recent history page into Room. */
    suspend fun refreshHistory(limit: Long = 60) {
        api.listMeals(limit = limit).forEach { applyServerMeal(it) }
    }

    // ---- mutations --------------------------------------------------------

    /** Confirms a meal — the act that feeds `recall_similar_meals` (§5 flywheel). */
    suspend fun confirm(serverId: String, dishName: String? = null) {
        val request = PatchMealRequest().apply {
            status = MealStatus.CONFIRMED
            dishName?.takeIf { it.isNotBlank() }?.let { this.dishName = it }
        }
        applyServerMeal(api.patchMeal(serverId, request))
    }

    /** "Ate 60% of it". */
    suspend fun setPortionScale(serverId: String, scale: Double) {
        applyServerMeal(api.patchMeal(serverId, PatchMealRequest().apply { portionScale = scale }))
    }

    /** Replaces the item list; every changed field is recorded server-side. */
    suspend fun replaceItems(serverId: String, items: List<PatchMealItem>) {
        applyServerMeal(api.patchMeal(serverId, PatchMealRequest().apply { this.items = items }))
    }

    /**
     * Sends free-text feedback and resumes the persisted agent session, producing a new
     * revision without re-running the tool calls (PRD §5 "correction by conversation").
     */
    suspend fun reanalyze(serverId: String, feedback: String) {
        api.reanalyzeMeal(serverId, ReanalyzeRequest().apply { this.feedback = feedback })
        dao.byServerId(serverId)?.let {
            dao.upsert(it.copy(status = LocalMealStatus.ANALYZING, notified = false, updatedAt = System.currentTimeMillis()))
        }
        scheduler.enqueueEventStream()
    }

    /** Deletes locally first so the row disappears immediately, then server-side. */
    suspend fun delete(clientUuid: String) {
        val meal = dao.byClientUuid(clientUuid) ?: return
        meal.photoPath?.let { File(it).delete() }
        dao.delete(clientUuid)
        meal.serverId?.let { runCatching { api.deleteMeal(it) } }
    }

    suspend fun resolveBarcode(ean: String): FoodResponse = api.resolveBarcode(ean)

    // ---- notification bookkeeping ----------------------------------------

    suspend fun markNotified(clientUuid: String) =
        dao.markNotified(clientUuid, System.currentTimeMillis())

    suspend fun markGroupNotified(groupId: String) =
        dao.markGroupNotified(groupId, System.currentTimeMillis())

    /** True the first time this sitting's single notification is claimed (§5). */
    suspend fun claimGroupNotification(groupId: String): Boolean =
        dao.claimGroupNotification(NotifiedGroupEntity(groupId, System.currentTimeMillis())) != -1L

    suspend fun countInFlight(): Int = dao.countInFlight()

    suspend fun awaitingAnalysis(): List<MealEntity> = dao.awaitingAnalysis()

    suspend fun wipe() {
        captureDir().listFiles()?.forEach { it.delete() }
        dao.wipe()
    }

    private fun String.toPart(): RequestBody = toRequestBody(TEXT)
}

private val RETRYABLE_CODES = setOf(401, 408, 429, 500, 502, 503, 504)
