package dev.picweight.android.sync

import android.content.Context
import android.util.Log
import androidx.hilt.work.HiltWorker
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import com.fasterxml.jackson.databind.ObjectMapper
import dagger.assisted.Assisted
import dagger.assisted.AssistedInject
import dev.picweight.android.data.local.DayStateEntity
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.BaseUrlInterceptor
import dev.picweight.android.data.remote.model.DayState
import dev.picweight.android.data.remote.model.DayStatus
import dev.picweight.android.data.remote.model.MacroTotals
import dev.picweight.android.data.remote.model.MealEvent
import dev.picweight.android.data.remote.model.MealEventKind
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.notifications.MealCopy
import dev.picweight.android.notifications.MealNotifier
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources
import java.time.LocalDate
import java.time.OffsetDateTime
import java.util.concurrent.TimeUnit
import javax.inject.Inject

/**
 * Subscribes to `/api/v1/meals/events` while anything is in flight, folds each frame
 * into Room and fires the one notification a meal (or a sitting) is allowed.
 *
 * The completion frame carries the whole day state, so the notification renders from a
 * single message with no follow-up round trip (PRD §9) — that is what makes it possible
 * to notify ~25s after capture rather than after a chain of requests.
 *
 * The stream is opened, not polled, and it closes itself once nothing is pending: a
 * long-lived connection would be the wrong trade for an app that is analysing something
 * for maybe two minutes a day.
 */
@HiltWorker
class MealEventWorker @AssistedInject constructor(
    @Assisted appContext: Context,
    @Assisted params: WorkerParameters,
    private val meals: MealRepository,
    private val notifier: MealNotifier,
    private val authRepository: AuthRepository,
    private val okHttpClient: OkHttpClient,
    private val mapper: ObjectMapper,
) : CoroutineWorker(appContext, params) {

    private sealed interface Signal {
        @JvmInline
        value class Frame(val event: MealEvent) : Signal
        data object Closed : Signal
    }

    override suspend fun doWork(): Result {
        if (authRepository.getServerUrl() == null || authRepository.getToken() == null) {
            return Result.success()
        }

        // Catch up first: a meal may well have finished while the phone was asleep or
        // offline, and that notification is still worth firing.
        reconcile()
        if (meals.awaitingAnalysis().isEmpty()) return Result.success()

        val client = okHttpClient.newBuilder()
            .readTimeout(0, TimeUnit.MILLISECONDS) // an SSE stream is meant to idle
            .build()
        val request = Request.Builder()
            .url(BaseUrlInterceptor.PLACEHOLDER_BASE_URL + "api/v1/meals/events")
            .header("Accept", "text/event-stream")
            .build()

        val signals = Channel<Signal>(Channel.BUFFERED)
        val listener = object : EventSourceListener() {
            override fun onEvent(eventSource: EventSource, id: String?, type: String?, data: String) {
                if (data.isBlank()) return
                val event = runCatching { mapper.readValue(data, MealEvent::class.java) }
                    .onFailure { Log.w(TAG, "Unparseable event frame", it) }
                    .getOrNull() ?: return
                signals.trySend(Signal.Frame(event))
            }

            override fun onFailure(eventSource: EventSource, t: Throwable?, response: Response?) {
                Log.i(TAG, "Event stream closed: ${t?.message ?: response?.code}")
                signals.trySend(Signal.Closed)
                signals.close()
            }

            override fun onClosed(eventSource: EventSource) {
                signals.trySend(Signal.Closed)
                signals.close()
            }
        }

        val source = EventSources.createFactory(client).newEventSource(request, listener)
        var streamDropped = false
        try {
            val deadline = System.currentTimeMillis() + MAX_RUNTIME_MS
            var idleSince: Long? = null
            while (System.currentTimeMillis() < deadline) {
                val wait = minOf(TICK_MS, deadline - System.currentTimeMillis()).coerceAtLeast(1L)
                val received = withTimeoutOrNull(wait) { signals.receiveCatching() }
                val signal = received?.getOrNull()
                when {
                    received == null -> Unit // tick: fall through to the idle check
                    signal is Signal.Frame -> handle(signal.event)
                    else -> {
                        streamDropped = true
                        break
                    }
                }

                val now = System.currentTimeMillis()
                if (meals.awaitingAnalysis().isEmpty()) {
                    // Linger briefly: a grouped sitting settles just after its last member.
                    if (idleSince == null) idleSince = now else if (now - idleSince > IDLE_GRACE_MS) break
                } else {
                    idleSince = null
                }
            }
        } finally {
            source.cancel()
        }

        reconcile()
        // Anything still pending means the stream died early; back off and reconnect.
        return if (streamDropped && meals.awaitingAnalysis().isNotEmpty()) Result.retry() else Result.success()
    }

    /** Applies one frame: Room first, then — if it earns one — the notification. */
    private suspend fun handle(event: MealEvent) {
        event.day?.let { runCatching { meals.cacheDayState(it) } }

        val mealId = event.mealId
        when (event.kind) {
            MealEventKind.ANALYZING -> mealId?.let { markAnalyzing(it) }
            MealEventKind.QUEUED -> Unit
            else -> mealId?.let { runCatching { meals.refreshMeal(it) } }
        }

        if (!MealCopy.isNotifiable(event.kind)) return

        if (event.kind == MealEventKind.GROUP_SETTLED) {
            val groupId = event.groupId ?: return
            // Exactly one notification per sitting, however many photos it held (§5).
            if (!meals.claimGroupNotification(groupId)) return
            notifier.notify(event)
            meals.markGroupNotified(groupId)
            return
        }

        val local = mealId?.let { meals.mealByServerId(it) } ?: return
        // A member of a sitting stays quiet: its group fires one notification for all.
        if (local.groupId != null) return
        if (local.notified) return
        notifier.notify(event)
        meals.markNotified(local.clientUuid)
    }

    private suspend fun markAnalyzing(serverId: String) {
        val local = meals.mealByServerId(serverId) ?: return
        if (local.status == LocalMealStatus.PENDING) {
            meals.applyLocalStatus(local, LocalMealStatus.ANALYZING)
        }
    }

    /**
     * Polls anything the stream may have missed. This is the safety net that makes the
     * notification survive a dropped connection, a doze window or a reboot: without it,
     * a completed meal could sit unnoticed until the user next opened the app.
     */
    private suspend fun reconcile() {
        val pending = runCatching { meals.awaitingAnalysis() }.getOrDefault(emptyList())
        for (meal in pending) {
            val serverId = meal.serverId ?: continue
            val refreshed = runCatching { meals.refreshMeal(serverId) }.getOrNull() ?: continue
            if (refreshed.status.isInFlight || refreshed.notified) continue
            if (refreshed.groupId != null) {
                // Let the group settle on its own; only claim it once every member is done.
                continue
            }
            notifier.notify(synthesiseEvent(refreshed) ?: continue)
            meals.markNotified(refreshed.clientUuid)
        }
    }

    /**
     * Rebuilds a notification payload for a meal we learned about by polling rather than
     * from the stream. The copy comes out of [dev.picweight.android.notifications.MealCopy]'s
     * templated path, which is exactly the "LLM unavailable" fallback §6 already requires.
     */
    private suspend fun synthesiseEvent(meal: MealEntity): MealEvent? {
        val date = LocalDate.ofEpochDay(
            Math.floorDiv(meal.eatenAt / 1000 + meal.timezoneOffset * 60L, 86_400L)
        )
        val cached = runCatching { meals.dayStateFor(date) }.getOrNull()
        return MealEvent().apply {
            kind = if (meal.status == LocalMealStatus.FAILED) MealEventKind.FAILED else MealEventKind.COMPLETED
            this.mealId = meal.serverId
            this.dishName = meal.dishName
            this.revision = meal.revision
            this.emittedAt = OffsetDateTime.now()
            this.error = meal.error
            this.totals = MacroTotals().apply {
                kcal = meal.kcal
                proteinG = meal.proteinG
                fatG = meal.fatG
                carbsG = meal.carbsG
            }
            this.day = cached?.toWire()
        }
    }

    private companion object {
        const val TAG = "MealEventWorker"

        /** WorkManager stops a worker at 10 minutes; leave headroom to shut down cleanly. */
        const val MAX_RUNTIME_MS = 8 * 60 * 1000L

        /** How long to keep listening after the last meal turns terminal. */
        const val IDLE_GRACE_MS = 20 * 1000L

        const val TICK_MS = 5 * 1000L
    }
}

private fun DayStateEntity.toWire(): DayState = DayState().apply {
    date = LocalDate.parse(this@toWire.date)
    consumedKcal = this@toWire.consumedKcal
    targetKcal = this@toWire.targetKcal
    remainingKcal = this@toWire.remainingKcal
    consumedProteinG = this@toWire.consumedProteinG
    targetProteinG = this@toWire.targetProteinG
    remainingProteinG = this@toWire.remainingProteinG
    consumedFatG = this@toWire.consumedFatG
    targetFatG = this@toWire.targetFatG
    consumedCarbsG = this@toWire.consumedCarbsG
    targetCarbsG = this@toWire.targetCarbsG
    mealsLogged = this@toWire.mealsLogged
    status = runCatching { DayStatus.fromValue(this@toWire.status) }.getOrNull() ?: DayStatus.NO_TARGETS
}
