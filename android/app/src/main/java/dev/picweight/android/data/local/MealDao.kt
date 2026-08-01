package dev.picweight.android.data.local

import androidx.room.Dao
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.Query
import androidx.room.Transaction
import androidx.room.Upsert
import kotlinx.coroutines.flow.Flow

/** Reads and writes for the local meal store. Every method is user-agnostic: the
 *  database is wiped on logout, so a device only ever holds one account's data. */
@Dao
interface MealDao {

    @Upsert
    suspend fun upsert(meal: MealEntity)

    @Upsert
    suspend fun upsertAll(meals: List<MealEntity>)

    @Query("SELECT * FROM meals WHERE clientUuid = :clientUuid")
    suspend fun byClientUuid(clientUuid: String): MealEntity?

    @Query("SELECT * FROM meals WHERE clientUuid = :clientUuid")
    fun flowByClientUuid(clientUuid: String): Flow<MealEntity?>

    @Query("SELECT * FROM meals WHERE serverId = :serverId")
    suspend fun byServerId(serverId: String): MealEntity?

    @Query("SELECT * FROM meals WHERE sittingId = :sittingId ORDER BY createdAt ASC")
    fun flowBySitting(sittingId: String): Flow<List<MealEntity>>

    @Query("SELECT * FROM meals WHERE sittingId = :sittingId ORDER BY createdAt ASC")
    suspend fun bySitting(sittingId: String): List<MealEntity>

    @Query("SELECT * FROM meals WHERE sittingId = :sittingId AND status = 'HELD' ORDER BY createdAt DESC LIMIT 1")
    suspend fun heldInSitting(sittingId: String): MealEntity?

    @Query("SELECT * FROM meals WHERE status = 'HELD' AND createdAt < :olderThan")
    suspend fun staleHeld(olderThan: Long): List<MealEntity>

    @Query("SELECT * FROM meals WHERE status IN ('HELD', 'QUEUED')")
    suspend fun awaitingUpload(): List<MealEntity>

    @Query("SELECT * FROM meals WHERE status IN ('PENDING', 'ANALYZING') AND serverId IS NOT NULL")
    suspend fun awaitingAnalysis(): List<MealEntity>

    @Query("SELECT COUNT(*) FROM meals WHERE status IN ('HELD', 'QUEUED', 'PENDING', 'ANALYZING')")
    suspend fun countInFlight(): Int

    /**
     * Meals whose local day equals [date], newest first.
     *
     * "What did I eat today" is a question about *your* day, not UTC's (PRD §8), so the
     * bucket is computed from each meal's own captured offset.
     */
    @Query(
        """
        SELECT * FROM meals
        WHERE date((eatenAt / 1000) + (timezoneOffset * 60), 'unixepoch') = :date
        ORDER BY eatenAt DESC
        """
    )
    fun flowForLocalDay(date: String): Flow<List<MealEntity>>

    @Query("SELECT * FROM meals ORDER BY eatenAt DESC LIMIT :limit")
    fun flowRecent(limit: Int): Flow<List<MealEntity>>

    @Query("UPDATE meals SET status = :status, updatedAt = :now WHERE clientUuid = :clientUuid")
    suspend fun setStatus(clientUuid: String, status: LocalMealStatus, now: Long)

    @Query("UPDATE meals SET status = 'FAILED', error = :reason, updatedAt = :now WHERE clientUuid = :clientUuid")
    suspend fun markFailed(clientUuid: String, reason: String, now: Long)

    @Query("UPDATE meals SET notified = 1, updatedAt = :now WHERE clientUuid = :clientUuid")
    suspend fun markNotified(clientUuid: String, now: Long)

    @Query("UPDATE meals SET notified = 1, updatedAt = :now WHERE groupId = :groupId")
    suspend fun markGroupNotified(groupId: String, now: Long)

    @Query("UPDATE meals SET photoPath = NULL WHERE clientUuid = :clientUuid")
    suspend fun clearPhotoPath(clientUuid: String)

    @Query("DELETE FROM meals WHERE clientUuid = :clientUuid")
    suspend fun delete(clientUuid: String)

    // ---- items -------------------------------------------------------------

    @Query("SELECT * FROM meal_items WHERE mealClientUuid = :clientUuid ORDER BY position ASC")
    fun flowItems(clientUuid: String): Flow<List<MealItemEntity>>

    @Query("SELECT * FROM meal_items WHERE mealClientUuid = :clientUuid ORDER BY position ASC")
    suspend fun items(clientUuid: String): List<MealItemEntity>

    @Query("DELETE FROM meal_items WHERE mealClientUuid = :clientUuid")
    suspend fun deleteItems(clientUuid: String)

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun insertItems(items: List<MealItemEntity>)

    /** Replaces a meal's items wholesale — the server always sends the full list. */
    @Transaction
    suspend fun replaceItems(clientUuid: String, items: List<MealItemEntity>) {
        deleteItems(clientUuid)
        insertItems(items)
    }

    @Transaction
    suspend fun upsertWithItems(meal: MealEntity, items: List<MealItemEntity>) {
        upsert(meal)
        deleteItems(meal.clientUuid)
        insertItems(items)
    }

    // ---- caches ------------------------------------------------------------

    @Query("SELECT * FROM recent_dishes ORDER BY lastEatenAt DESC LIMIT :limit")
    fun flowRecentDishes(limit: Int): Flow<List<RecentDishEntity>>

    @Query("DELETE FROM recent_dishes")
    suspend fun clearRecentDishes()

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun insertRecentDishes(dishes: List<RecentDishEntity>)

    @Transaction
    suspend fun replaceRecentDishes(dishes: List<RecentDishEntity>) {
        clearRecentDishes()
        insertRecentDishes(dishes)
    }

    @Upsert
    suspend fun upsertDayState(day: DayStateEntity)

    @Query("SELECT * FROM day_states WHERE date = :date")
    fun flowDayState(date: String): Flow<DayStateEntity?>

    @Query("SELECT * FROM day_states WHERE date = :date")
    suspend fun dayState(date: String): DayStateEntity?

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun claimGroupNotification(group: NotifiedGroupEntity): Long

    @Query("DELETE FROM notified_groups WHERE notifiedAt < :olderThan")
    suspend fun pruneNotifiedGroups(olderThan: Long)

    // ---- logout ------------------------------------------------------------

    @Query("DELETE FROM meals")
    suspend fun deleteAllMeals()

    @Query("DELETE FROM day_states")
    suspend fun deleteAllDayStates()

    @Query("DELETE FROM notified_groups")
    suspend fun deleteAllNotifiedGroups()

    @Transaction
    suspend fun wipe() {
        deleteAllMeals()
        clearRecentDishes()
        deleteAllDayStates()
        deleteAllNotifiedGroups()
    }
}
