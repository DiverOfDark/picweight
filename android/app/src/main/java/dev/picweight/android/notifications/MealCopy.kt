package dev.picweight.android.notifications

import dev.picweight.android.data.remote.model.DayState
import dev.picweight.android.data.remote.model.DayStatus
import dev.picweight.android.data.remote.model.MealEvent
import dev.picweight.android.data.remote.model.MealEventKind
import java.text.NumberFormat
import java.util.Locale
import kotlin.math.roundToLong

/** A notification, split into what the collapsed and expanded views show. */
data class NotificationCopy(val title: String, val body: String)

/**
 * Builds the per-meal notification (PRD §6).
 *
 * The rule that shapes every line: the notification fires 20–30s after capture, by
 * which time the phone is probably back in a pocket, so it has to make sense read
 * cold minutes later. It therefore **leads with the dish name** and carries the whole
 * day's standing — never "your analysis is ready, tap to see".
 *
 * The backend phrases `headline`/`body` (the verdict line is LLM-worded from the
 * numbers, with its own templated fallback). This object uses that copy when it is
 * present and reconstructs an equivalent from the day state when it is not, so a
 * degraded server still produces a notification worth reading.
 */
object MealCopy {

    /** Kinds that are worth interrupting the user for. */
    fun isNotifiable(kind: MealEventKind?): Boolean = when (kind) {
        MealEventKind.COMPLETED,
        MealEventKind.REANALYZED,
        MealEventKind.GROUP_SETTLED,
        MealEventKind.FAILED,
        -> true

        else -> false
    }

    /** Returns the copy for an event, or null if this event should stay silent. */
    fun forEvent(event: MealEvent): NotificationCopy? {
        if (!isNotifiable(event.kind)) return null

        if (event.kind == MealEventKind.FAILED) {
            // A failure is loud and specific: a silent stall is the one outcome §5 rules out.
            return NotificationCopy(
                title = event.dishName?.let { "Couldn't estimate $it" } ?: "Meal analysis failed",
                body = event.error ?: "The estimate didn't come back. Open picweight to retry.",
            )
        }

        val title = event.headline?.takeIf { it.isNotBlank() } ?: fallbackHeadline(event)
        val body = event.body?.takeIf { it.isNotBlank() } ?: fallbackBody(event)
        return NotificationCopy(title, body)
    }

    /** Line 1 — what was logged. Dish name first, always. */
    fun fallbackHeadline(event: MealEvent): String {
        val kcal = event.totals?.kcal
        val name = event.dishName?.takeIf { it.isNotBlank() }
            ?: if (event.kind == MealEventKind.GROUP_SETTLED) "Sitting logged" else "Meal logged"
        return if (kcal != null) "$name — ${kcal.kcal()} kcal" else name
    }

    /** Lines 2–4, rebuilt from the day state the event carries. */
    fun fallbackBody(event: MealEvent): String {
        val day = event.day ?: return "Logged. Open picweight for today's numbers."
        return listOf(standing(day), macroStatus(day), verdict(day))
            .filter { it.isNotBlank() }
            .joinToString("\n")
    }

    /** Line 2 — where you stand. */
    fun standing(day: DayState): String {
        if (day.targetKcal <= 0.0) return "${day.consumedKcal.kcal()} kcal today · no target set yet"
        val remaining = day.remainingKcal
        val tail = if (remaining >= 0) "${remaining.kcal()} left" else "${(-remaining).kcal()} over"
        return "${day.consumedKcal.kcal()} / ${day.targetKcal.kcal()} today · $tail"
    }

    /**
     * Line 3 — the macro that actually binds. Protein is the floor you can miss by
     * accident; fat and carbs sort themselves out once the energy budget is respected.
     */
    fun macroStatus(day: DayState): String {
        if (day.targetProteinG <= 0.0) return ""
        val consumed = day.consumedProteinG.grams()
        val target = day.targetProteinG.grams()
        val short = day.remainingProteinG
        if (short <= 0.5) return "Protein $consumed/${target}g — floor met."
        val budget = day.remainingKcal
        return if (budget > 0) {
            "Protein $consumed/${target}g — ${short.grams()}g short with ${budget.kcal()} kcal to spend."
        } else {
            "Protein $consumed/${target}g — ${short.grams()}g short and the kcal budget is spent."
        }
    }

    /** Line 4 — templated verdict, used only when the server phrased none. */
    fun verdict(day: DayState): String = when (day.status) {
        DayStatus.ON_TRACK -> "On track."
        DayStatus.TIGHT -> "Tight, but it works if the rest is lean."
        DayStatus.OVER -> "Over for today — tomorrow starts clean."
        DayStatus.PROTEIN_UNREACHABLE ->
            "The protein floor is out of reach on what's left; take the miss and get it earlier tomorrow."
        DayStatus.NO_TARGETS -> "Set your body data to get a target."
    }

    private fun Double.kcal(): String = INTEGER.format(this.roundToLong())

    private fun Double.grams(): String = INTEGER.format(this.roundToLong())

    private val INTEGER: NumberFormat = NumberFormat.getIntegerInstance(Locale.getDefault())
}
