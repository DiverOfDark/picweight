<script setup>
/**
 * The day view: what you have eaten, and what is left in the tank.
 *
 * Sittings arrive from the API already collapsed (`DayResponse.groups`) and are
 * interleaved with the loose meals by time, so a five-dish restaurant order
 * occupies one row until you ask it not to.
 */
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ChevronLeft, ChevronRight, RefreshCw, Radio } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { dayLabelFull, dayOffsetMinutes, instantMs, kcal, relativeDayLabel, shiftDateKey, todayKey } from '@/lib/format'
import { applyDayAccent } from '@/lib/status'
import { useMealEvents } from '@/composables/useMealEvents'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import EmptyState from '@/components/EmptyState.vue'
import KcalRing from '@/components/KcalRing.vue'
import MacroBars from '@/components/MacroBars.vue'
import MealRow from '@/components/MealRow.vue'
import SittingRow from '@/components/SittingRow.vue'
import StatusPill from '@/components/StatusPill.vue'

const route = useRoute()
const router = useRouter()

const day = ref(null)
const loading = ref(true)
const error = ref('')

const dateKey = computed(() => route.params.date || todayKey())
const isToday = computed(() => dateKey.value === todayKey())

/** Groups and loose meals share one timeline; the API returns them apart. */
const entries = computed(() => {
  if (!day.value) return []
  const rows = [
    ...(day.value.groups ?? []).map((group) => ({
      kind: 'group',
      id: group.group_id,
      at: group.eaten_at,
      group,
    })),
    ...(day.value.meals ?? []).map((meal) => ({ kind: 'meal', id: meal.id, at: meal.eaten_at, meal })),
  ]
  return rows.sort((a, b) => instantMs(b.at) - instantMs(a.at))
})

async function load() {
  loading.value = true
  error.value = ''
  try {
    // `tz_offset` buckets the day server-side, so it has to be the offset in
    // force on *that* day rather than the one in force right now — otherwise a
    // January day read in July is bucketed an hour out.
    day.value = await api.day(dateKey.value, dayOffsetMinutes(dateKey.value))
    applyDayAccent(day.value.state.status)
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

function go(days) {
  router.push({ name: 'day', params: { date: shiftDateKey(dateKey.value, days) } })
}

// Analysis is asynchronous, so the day reloads when the loop lands rather than
// asking the user to refresh.
const { connected } = useMealEvents(() => load())

watch(dateKey, load)
onMounted(load)

</script>

<template>
  <div class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div>
        <p class="eyebrow">{{ relativeDayLabel(dateKey) }}</p>
        <h1 class="mt-1.5 text-2xl font-semibold tracking-tight text-ink">
          {{ dayLabelFull(dateKey) }}
        </h1>
      </div>

      <div class="flex items-center gap-1.5">
        <span
          v-if="connected"
          class="mr-1 hidden items-center gap-1.5 text-xs text-ink-3 sm:inline-flex"
          title="Listening for analysis results"
        >
          <Radio class="size-3" aria-hidden="true" /> live
        </span>
        <Button variant="outline" size="icon-sm" aria-label="Previous day" @click="go(-1)">
          <ChevronLeft />
        </Button>
        <Button variant="outline" size="sm" :disabled="isToday" @click="router.push({ name: 'today' })">
          Today
        </Button>
        <Button variant="outline" size="icon-sm" aria-label="Next day" :disabled="isToday" @click="go(1)">
          <ChevronRight />
        </Button>
        <Button variant="ghost" size="icon-sm" aria-label="Reload" @click="load">
          <RefreshCw :class="loading && 'animate-spin'" />
        </Button>
      </div>
    </header>

    <div class="tick-rule" aria-hidden="true" />

    <p v-if="error" class="rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-sm text-critical">
      {{ error }}
    </p>

    <div v-if="day" class="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-[minmax(0,22rem)_minmax(0,1fr)]">
      <!-- The instrument panel -->
      <div class="space-y-5">
        <Card>
          <CardContent class="space-y-5 pt-5">
            <KcalRing :state="day.state" />

            <div class="flex items-center justify-center">
              <StatusPill :day="day.state.status" />
            </div>

            <p class="text-center text-sm leading-relaxed text-ink-2">{{ day.verdict }}</p>

            <div class="tick-rule" aria-hidden="true" />

            <MacroBars :state="day.state" />
          </CardContent>
        </Card>
      </div>

      <!-- The ledger -->
      <section class="space-y-3">
        <div class="flex items-baseline justify-between">
          <h2 class="eyebrow">Logged</h2>
          <p class="num text-xs text-ink-3">
            {{ day.state.meals_logged }} {{ day.state.meals_logged === 1 ? 'meal' : 'meals' }} ·
            {{ kcal(day.state.consumed_kcal) }} kcal
          </p>
        </div>

        <div class="space-y-1.5">
          <template v-for="entry in entries" :key="entry.id">
            <!--
              No offset passed: a `GroupSummary` carries none of its own, so the
              row renders in the viewer's zone as it stood at that instant.
              Passing the offset as it stands *now* slid every row by an hour
              once the clocks changed.
            -->
            <SittingRow v-if="entry.kind === 'group'" :group="entry.group" />
            <MealRow v-else :meal="entry.meal" />
          </template>
        </div>

        <EmptyState
          v-if="!entries.length && !loading"
          title="Nothing logged on this day"
          hint="Meals arrive from the phone app. Photograph a plate and it shows up here about half a minute later."
        />
      </section>
    </div>

    <p v-else-if="loading" class="text-sm text-ink-3">Loading the day…</p>
  </div>
</template>
