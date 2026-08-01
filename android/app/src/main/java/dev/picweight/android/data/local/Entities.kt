package dev.picweight.android.data.local

import androidx.room.Entity
import androidx.room.ForeignKey
import androidx.room.Index
import androidx.room.PrimaryKey
import dev.picweight.android.data.remote.model.MealStatus

/**
 * Where a meal is in its life, from the phone's point of view.
 *
 * The first three states have no server counterpart: they exist because Room, not the
 * backend, is the source of truth (PRD §7). A meal is durable the instant the shutter
 * fires, and stays visible through airplane mode until the queue drains (G5).
 */
enum class LocalMealStatus {
    /** Captured and persisted, not yet handed to the upload queue. */
    HELD,

    /** Handed to WorkManager; waiting on a connection or a retry. */
    QUEUED,

    /** Accepted by the server; the analysis job is enqueued or running. */
    PENDING,

    /** The agent loop is running. */
    ANALYZING,

    /** A draft the user has not confirmed. */
    NEEDS_REVIEW,

    /** Confirmed — the only status `recall_similar_meals` reads from. */
    CONFIRMED,

    /** Loud failure, with a reason the user can read. */
    FAILED;

    /** True while the app should keep an eye on this meal. */
    val isInFlight: Boolean
        get() = this == HELD || this == QUEUED || this == PENDING || this == ANALYZING

    /** True once the server has a copy — no upload work left to do. */
    val isUploaded: Boolean
        get() = this != HELD && this != QUEUED

    companion object {
        /**
         * Maps a wire [MealStatus] value.
         *
         * Written against [MealStatus]'s own constants rather than string literals on
         * purpose: the wire values are generated from `openapi.json` (lowercase
         * snake_case — `needs_review`), and a hand-written `"NeedsReview"` compiled
         * perfectly while matching nothing, so every server status silently fell through
         * to [PENDING]. That is invisible at compile time and looks like a stuck meal at
         * runtime. Referencing the generated enum makes the compiler the thing that
         * notices when the contract moves.
         */
        fun fromWire(value: String?): LocalMealStatus = when (value) {
            MealStatus.PENDING.value -> PENDING
            MealStatus.ANALYZING.value -> ANALYZING
            MealStatus.NEEDS_REVIEW.value -> NEEDS_REVIEW
            MealStatus.CONFIRMED.value -> CONFIRMED
            MealStatus.FAILED.value -> FAILED
            else -> PENDING
        }
    }
}

/**
 * One meal — one photo, one agent loop (PRD §5).
 *
 * Keyed by `clientUuid`, generated on the phone at capture, because that is the
 * idempotency key the server dedupes on: a replayed upload can never create a second
 * meal, so the queue may retry as often as it likes.
 */
@Entity(
    tableName = "meals",
    indices = [
        Index("serverId", unique = true),
        Index("sittingId"),
        Index("eatenAt"),
        Index("status"),
    ],
)
data class MealEntity(
    @PrimaryKey val clientUuid: String,

    /** `meals.id` once the server has accepted the upload. */
    val serverId: String? = null,

    /**
     * Local sitting id. Always set, even for a solo meal, because the capture screen's
     * thumbnail strip is scoped to it.
     */
    val sittingId: String,

    /**
     * The `group_id` actually sent to the server — null for a sitting that turned out to
     * hold a single photo, so that meal gets the per-meal notification that leads with
     * its dish name rather than a "1 dish" group summary (PRD §6).
     */
    val groupId: String? = null,

    /** The settle hint, sent with the final shot of a sitting (PRD §5). */
    val groupSize: Int? = null,

    val dishName: String? = null,
    val comment: String? = null,

    /** Wire value of [dev.picweight.android.data.remote.model.NameSource]. */
    val nameSource: String,

    val mealType: String? = null,

    /** Epoch millis. */
    val eatenAt: Long,

    /** Minutes east of UTC at capture; buckets the local day. */
    val timezoneOffset: Int,

    /** Absolute path of the captured JPEG; deleted once the upload lands. */
    val photoPath: String? = null,

    /** Server-relative thumbnail path, once analysis has produced one. */
    val thumbnailUrl: String? = null,

    val status: LocalMealStatus,

    val revision: Int = 1,
    val portionScale: Double = 1.0,

    val kcal: Double = 0.0,
    val proteinG: Double = 0.0,
    val fatG: Double = 0.0,
    val carbsG: Double = 0.0,

    /**
     * Why this meal is unhappy — never silent.
     *
     * Set when [status] is [LocalMealStatus.FAILED], and *also* while a meal is still
     * [LocalMealStatus.QUEUED] and its upload keeps bouncing. A queued row whose upload
     * has failed six times is not "waiting for a connection", and saying so is the
     * difference between a diagnosable bug and one that hides for a week.
     */
    val error: String? = null,

    /** Guards against notifying twice for the same meal across worker restarts. */
    val notified: Boolean = false,

    val createdAt: Long,
    val updatedAt: Long,
)

/** One component of a meal, at the meal's current revision. */
@Entity(
    tableName = "meal_items",
    primaryKeys = ["mealClientUuid", "position"],
    foreignKeys = [
        ForeignKey(
            entity = MealEntity::class,
            parentColumns = ["clientUuid"],
            childColumns = ["mealClientUuid"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
)
data class MealItemEntity(
    val mealClientUuid: String,
    val position: Int,
    val serverId: String? = null,
    val name: String,
    val grams: Double,
    val kcal: Double,
    val proteinG: Double,
    val fatG: Double,
    val carbsG: Double,
    val gramsSource: String? = null,
    val macroSource: String? = null,
    val confidence: Double? = null,
    /** One line of "why", shown next to the item. */
    val reasoningNote: String? = null,
    val barcode: String? = null,
)

/**
 * A previously confirmed dish, cached so the capture screen's chips render offline.
 * One tap sets the dish name — faster than typing and more accurate than vision (§1).
 */
@Entity(tableName = "recent_dishes")
data class RecentDishEntity(
    @PrimaryKey val dishNameNormalized: String,
    val dishName: String,
    val lastEatenAt: Long,
    val occurrences: Long,
    val lastMealId: String,
    val kcal: Double,
    val proteinG: Double,
    val fatG: Double,
    val carbsG: Double,
)

/** A day's totals against its targets, cached so home renders before the network does. */
@Entity(tableName = "day_states")
data class DayStateEntity(
    /** ISO-8601 local calendar date. */
    @PrimaryKey val date: String,
    val consumedKcal: Double,
    val targetKcal: Double,
    val remainingKcal: Double,
    val consumedProteinG: Double,
    val targetProteinG: Double,
    val remainingProteinG: Double,
    val consumedFatG: Double,
    val targetFatG: Double,
    val consumedCarbsG: Double,
    val targetCarbsG: Double,
    val mealsLogged: Long,
    /** Wire value of [dev.picweight.android.data.remote.model.DayStatus]. */
    val status: String,
    val verdict: String? = null,
    val updatedAt: Long,
)

/**
 * A sitting that has already fired its single notification.
 *
 * N photos produce N independent loops but exactly one notification (PRD §5); this is
 * what keeps that true across worker restarts and a re-delivered SSE frame.
 */
@Entity(tableName = "notified_groups")
data class NotifiedGroupEntity(
    @PrimaryKey val groupId: String,
    val notifiedAt: Long,
)
