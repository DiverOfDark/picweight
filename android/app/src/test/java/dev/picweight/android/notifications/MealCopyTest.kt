package dev.picweight.android.notifications

import dev.picweight.android.data.remote.model.DayState
import dev.picweight.android.data.remote.model.DayStatus
import dev.picweight.android.data.remote.model.MacroTotals
import dev.picweight.android.data.remote.model.MealEvent
import dev.picweight.android.data.remote.model.MealEventKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.LocalDate

/**
 * The notification is the product surface for §6, and its stated requirement is that it
 * makes sense read cold minutes after the phone went back in a pocket. These assert the
 * properties that requirement actually implies.
 */
class MealCopyTest {

    private fun dayState(
        consumed: Double = 1450.0,
        target: Double = 2050.0,
        protein: Double = 82.0,
        proteinTarget: Double = 165.0,
        status: DayStatus = DayStatus.TIGHT,
    ) = DayState().apply {
        date = LocalDate.of(2026, 8, 1)
        consumedKcal = consumed
        targetKcal = target
        remainingKcal = target - consumed
        consumedProteinG = protein
        targetProteinG = proteinTarget
        remainingProteinG = (proteinTarget - protein).coerceAtLeast(0.0)
        consumedFatG = 50.0
        targetFatG = 60.0
        consumedCarbsG = 120.0
        targetCarbsG = 200.0
        mealsLogged = 3
        this.status = status
    }

    private fun event(
        kind: MealEventKind = MealEventKind.COMPLETED,
        dish: String? = "Шаурма с курицей",
        kcal: Double? = 780.0,
        headline: String? = null,
        body: String? = null,
        day: DayState? = dayState(),
    ) = MealEvent().apply {
        this.kind = kind
        this.mealId = "meal-1"
        this.revision = 1
        this.dishName = dish
        this.headline = headline
        this.body = body
        this.day = day
        if (kcal != null) {
            totals = MacroTotals().apply {
                this.kcal = kcal
                proteinG = 60.0
                fatG = 30.0
                carbsG = 50.0
            }
        }
    }

    @Test
    fun `server phrasing wins when present`() {
        val copy = MealCopy.forEvent(
            event(headline = "Шаурма с курицей — 780 kcal", body = "1,450 / 2,050 today · 600 left")
        )
        assertEquals("Шаурма с курицей — 780 kcal", copy?.title)
        assertEquals("1,450 / 2,050 today · 600 left", copy?.body)
    }

    @Test
    fun `headline leads with the dish name, not with a status word`() {
        val copy = MealCopy.forEvent(event())
        assertTrue(
            "headline must open with the dish: ${copy?.title}",
            copy!!.title.startsWith("Шаурма с курицей"),
        )
        assertTrue(copy.title.contains("780"))
    }

    @Test
    fun `body is self-contained - standing, the binding macro and a verdict`() {
        val copy = MealCopy.forEvent(event())!!
        val lines = copy.body.lines()
        assertEquals(3, lines.size)
        assertTrue("standing line: ${lines[0]}", lines[0].contains("600 left"))
        assertTrue("macro line: ${lines[1]}", lines[1].contains("83") && lines[1].contains("protein", true))
        assertTrue("verdict line must not be empty", lines[2].isNotBlank())
    }

    @Test
    fun `going over reads as over rather than as a negative remainder`() {
        val over = dayState(consumed = 2400.0, status = DayStatus.OVER)
        assertTrue(MealCopy.standing(over).contains("350 over"))
        assertFalse(MealCopy.standing(over).contains("-350"))
    }

    @Test
    fun `a met protein floor says so instead of reporting a zero shortfall`() {
        val met = dayState(protein = 170.0)
        assertTrue(MealCopy.macroStatus(met).contains("floor met"))
    }

    @Test
    fun `an unreachable protein floor names the constraint rather than cheering`() {
        val stuck = dayState(consumed = 2050.0, protein = 60.0, status = DayStatus.PROTEIN_UNREACHABLE)
        assertTrue(MealCopy.macroStatus(stuck).contains("budget is spent"))
        assertTrue(MealCopy.verdict(stuck).contains("out of reach"))
    }

    @Test
    fun `a failure is specific and never silent`() {
        val copy = MealCopy.forEvent(
            event(kind = MealEventKind.FAILED, kcal = null, day = null).apply {
                error = "OpenAI quota exhausted"
            }
        )!!
        assertTrue(copy.title.contains("Шаурма с курицей"))
        assertEquals("OpenAI quota exhausted", copy.body)
    }

    @Test
    fun `intermediate lifecycle events stay silent`() {
        assertNull(MealCopy.forEvent(event(kind = MealEventKind.QUEUED)))
        assertNull(MealCopy.forEvent(event(kind = MealEventKind.ANALYZING)))
        assertNull(MealCopy.forEvent(event(kind = MealEventKind.UPDATED)))
        assertFalse(MealCopy.isNotifiable(MealEventKind.QUEUED))
        assertTrue(MealCopy.isNotifiable(MealEventKind.GROUP_SETTLED))
    }

    @Test
    fun `a day with no targets still produces readable copy`() {
        val none = dayState(target = 0.0, proteinTarget = 0.0, status = DayStatus.NO_TARGETS)
        val copy = MealCopy.forEvent(event(day = none))!!
        assertTrue(copy.body.contains("no target set yet"))
        assertFalse("must not divide by a zero target", copy.body.contains("NaN"))
    }
}
