package dev.picweight.android.data.local

import androidx.room.Database
import androidx.room.RoomDatabase
import androidx.room.TypeConverter
import androidx.room.TypeConverters

/** Stores [LocalMealStatus] by name so the DAO's string literals stay readable. */
class Converters {
    @TypeConverter
    fun toStatus(value: String): LocalMealStatus =
        runCatching { LocalMealStatus.valueOf(value) }.getOrDefault(LocalMealStatus.PENDING)

    @TypeConverter
    fun fromStatus(status: LocalMealStatus): String = status.name
}

/**
 * The phone's source of truth (PRD §7). The UI reads only from here; the network layer
 * writes into it. That is what makes the app usable in airplane mode and what makes
 * "never lose a logged meal" (G5) a property of the storage rather than of the request.
 */
@Database(
    entities = [
        MealEntity::class,
        MealItemEntity::class,
        RecentDishEntity::class,
        DayStateEntity::class,
        NotifiedGroupEntity::class,
    ],
    version = 1,
    exportSchema = false,
)
@TypeConverters(Converters::class)
abstract class PicweightDatabase : RoomDatabase() {
    abstract fun mealDao(): MealDao

    companion object {
        const val NAME = "picweight.db"
    }
}
