package dev.picweight.android.notifications

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.picweight.android.R
import dev.picweight.android.data.remote.model.MealEvent
import dev.picweight.android.data.remote.model.MealEventKind
import dev.picweight.android.ui.MainActivity
import javax.inject.Inject
import javax.inject.Singleton
import kotlin.math.absoluteValue

/**
 * Posts the one notification a logged meal is allowed (PRD §6).
 *
 * Feedback here is strictly event-driven: there is no schedule, no daily nudge and no
 * cron anywhere in this app. The only thing that can produce a notification is *you
 * just logged something*, and each one carries genuinely new information — your actual
 * remaining budget — which is the entire defence for firing ~5 times a day.
 */
@Singleton
class MealNotifier @Inject constructor(
    @param:ApplicationContext private val context: Context,
) {
    companion object {
        const val CHANNEL_MEALS = "picweight_meals"

        /** Extras MainActivity reads to open straight onto the meal that fired. */
        const val EXTRA_MEAL_ID = "picweight.meal_id"
        const val EXTRA_GROUP_ID = "picweight.group_id"

        fun createChannels(context: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val channel = NotificationChannel(
                CHANNEL_MEALS,
                context.getString(R.string.meal_notification_channel_name),
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply {
                description = context.getString(R.string.meal_notification_channel_description)
                setShowBadge(true)
            }
            context.getSystemService(NotificationManager::class.java)
                ?.createNotificationChannel(channel)
        }
    }

    /**
     * Renders [event] and posts it. Returns false when the copy was suppressed or the
     * user has not granted POST_NOTIFICATIONS — the caller still marks the meal seen,
     * because a notification the OS refuses is not worth retrying forever.
     */
    fun notify(event: MealEvent): Boolean {
        val copy = MealCopy.forEvent(event) ?: return false
        if (!canPost()) return false

        val id = notificationId(event)
        val notification = NotificationCompat.Builder(context, CHANNEL_MEALS)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(copy.title)
            .setContentText(copy.body.lineSequence().firstOrNull() ?: copy.body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(copy.body))
            .setPriority(
                if (event.kind == MealEventKind.FAILED) NotificationCompat.PRIORITY_HIGH
                else NotificationCompat.PRIORITY_DEFAULT
            )
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            .setAutoCancel(true)
            .setContentIntent(openIntent(event, id))
            .build()

        context.getSystemService(NotificationManager::class.java)?.notify(id, notification)
        return true
    }

    private fun canPost(): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return true
        return ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
    }

    /**
     * Stable per meal (or per sitting), so a re-analysis replaces the meal's earlier
     * notification instead of stacking a second one next to numbers it just superseded.
     */
    private fun notificationId(event: MealEvent): Int {
        val key = when (event.kind) {
            MealEventKind.GROUP_SETTLED -> "group:${event.groupId}"
            else -> "meal:${event.mealId}"
        }
        return key.hashCode().absoluteValue.coerceAtLeast(1)
    }

    private fun openIntent(event: MealEvent, requestCode: Int): PendingIntent {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            event.mealId?.let { putExtra(EXTRA_MEAL_ID, it) }
            event.groupId?.let { putExtra(EXTRA_GROUP_ID, it) }
        }
        return PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
