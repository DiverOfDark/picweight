package dev.picweight.android.data.repository

import android.content.Context
import com.fasterxml.jackson.databind.ObjectMapper
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealDao
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.sync.SyncScheduler
import io.mockk.coEvery
import io.mockk.mockk
import io.mockk.verify
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The sitting rules from PRD §5, which are easy to state and easy to get wrong.
 *
 * - Every photo is an independent upload, so N dishes run N concurrent loops.
 * - `group_id` is stamped on every photo of a *multi-shot* sitting.
 * - `group_size` rides along with the final shot, so the sitting settles the moment all
 *   N jobs are terminal rather than waiting out the 90s debounce.
 * - A sitting that turned out to hold one photo sends no `group_id` at all, so it earns
 *   the per-meal notification that leads with its dish name (§6) instead of a group
 *   summary line.
 */
class SittingQueueTest {

    private lateinit var store: LinkedHashMap<String, MealEntity>
    private lateinit var dao: MealDao
    private lateinit var scheduler: SyncScheduler
    private lateinit var repository: MealRepository

    @Before
    fun setUp() {
        store = LinkedHashMap()
        dao = mockk(relaxed = true)
        scheduler = mockk(relaxed = true)

        coEvery { dao.upsert(any()) } answers {
            val meal = firstArg<MealEntity>()
            store[meal.clientUuid] = meal
        }
        coEvery { dao.bySitting(any()) } answers {
            store.values.filter { it.sittingId == firstArg<String>() }
        }
        coEvery { dao.heldInSitting(any()) } answers {
            store.values.lastOrNull {
                it.sittingId == firstArg<String>() && it.status == LocalMealStatus.HELD
            }
        }
        coEvery { dao.staleHeld(any()) } answers {
            store.values.filter { it.status == LocalMealStatus.HELD && it.createdAt < firstArg<Long>() }
        }

        repository = MealRepository(
            dao = dao,
            api = mockk<PicweightApi>(relaxed = true),
            scheduler = scheduler,
            mapper = ObjectMapper(),
            context = mockk<Context>(relaxed = true),
        )
    }

    private suspend fun shoot(sitting: String, dish: String? = null): String =
        repository.addShot(
            sittingId = sitting,
            photo = null,
            dishName = dish,
            comment = null,
            nameSource = if (dish == null) NameSource.VISION else NameSource.RECENT_CHIP,
        )

    @Test
    fun `a solo meal is sent without a group so its notification names the dish`() = runTest {
        val sitting = repository.newSitting()
        val uuid = shoot(sitting, "Шаурма")

        // Held, not queued: the shot does not yet know whether a second one is coming.
        assertEquals(LocalMealStatus.HELD, store.getValue(uuid).status)

        repository.closeSitting(sitting)

        val meal = store.getValue(uuid)
        assertEquals(LocalMealStatus.QUEUED, meal.status)
        assertNull("a sitting of one carries no group_id", meal.groupId)
        assertNull("and therefore no settle hint", meal.groupSize)
        verify(exactly = 1) { scheduler.enqueueUpload(uuid) }
    }

    @Test
    fun `three photos become three independent uploads under one group`() = runTest {
        val sitting = repository.newSitting()
        val first = shoot(sitting, "Шаурма")
        val second = shoot(sitting, "Салат")
        val third = shoot(sitting, "Кола")

        // Taking the next shot is what proves this is a sitting, so the previous one is
        // released immediately — its loop starts while the user is still shooting.
        assertEquals(LocalMealStatus.QUEUED, store.getValue(first).status)
        assertEquals(LocalMealStatus.QUEUED, store.getValue(second).status)
        assertEquals(LocalMealStatus.HELD, store.getValue(third).status)

        repository.closeSitting(sitting)

        listOf(first, second, third).forEach {
            assertEquals(sitting, store.getValue(it).groupId)
            assertEquals(LocalMealStatus.QUEUED, store.getValue(it).status)
        }
        // The hint rides on the final shot only.
        assertNull(store.getValue(first).groupSize)
        assertNull(store.getValue(second).groupSize)
        assertEquals(3, store.getValue(third).groupSize)

        verify(exactly = 1) { scheduler.enqueueUpload(first) }
        verify(exactly = 1) { scheduler.enqueueUpload(second) }
        verify(exactly = 1) { scheduler.enqueueUpload(third) }
    }

    @Test
    fun `every shot is durable before it is queued`() = runTest {
        val sitting = repository.newSitting()
        val uuid = shoot(sitting, "Плов")

        // The row exists the instant the shutter fires — that is what makes "never lose
        // a logged meal" a property of storage rather than of the request (G5).
        val meal = store.getValue(uuid)
        assertEquals("Плов", meal.dishName)
        assertEquals(NameSource.RECENT_CHIP.value, meal.nameSource)
        assertTrue(meal.eatenAt > 0)
    }

    @Test
    fun `an abandoned sitting is released rather than stranded`() = runTest {
        val sitting = repository.newSitting()
        val uuid = shoot(sitting, "Борщ")

        // Simulate the app dying with the shot still held.
        store[uuid] = store.getValue(uuid).copy(createdAt = 0L)

        repository.releaseStaleHolds(olderThanMs = 1_000L)

        assertEquals(LocalMealStatus.QUEUED, store.getValue(uuid).status)
        verify(exactly = 1) { scheduler.enqueueUpload(uuid) }
    }
}
