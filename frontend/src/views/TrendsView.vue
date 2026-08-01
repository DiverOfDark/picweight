<script setup>
/**
 * Two questions, two charts, one axis each: is the weight moving, and where is
 * the energy coming from. Never one chart with two scales.
 */
import { computed, onMounted, ref } from 'vue'
import { Scale } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { dayLabel, grams, kcal, kg, localDateKey, shiftDateKey, stamp, todayKey } from '@/lib/format'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import EmptyState from '@/components/EmptyState.vue'
import LineChart from '@/components/charts/LineChart.vue'
import MacroTrendChart from '@/components/charts/MacroTrendChart.vue'

const RANGES = [
  { days: 30, label: '30 days' },
  { days: 90, label: '90 days' },
  { days: 365, label: '1 year' },
]

const range = ref(90)
const weights = ref([])
const meals = ref([])
const profile = ref(null)
const loading = ref(true)
const error = ref('')

async function load() {
  loading.value = true
  error.value = ''
  const from = shiftDateKey(todayKey(), -(range.value - 1))
  try {
    const [weightRows, mealRows, me] = await Promise.all([
      api.weights({ from, to: todayKey(), limit: 500 }),
      api.meals({ from, to: todayKey(), limit: 1000 }),
      api.me(),
    ])
    weights.value = weightRows
    meals.value = mealRows
    profile.value = me.profile
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

function setRange(days) {
  range.value = days
  load()
}

/** The API returns readings newest first; a trend line reads oldest first. */
const weightPoints = computed(() =>
  [...weights.value]
    .sort((a, b) => new Date(a.logged_at) - new Date(b.logged_at))
    .map((reading) => ({
      at: reading.logged_at,
      value: reading.weight_kg,
      caption: dayLabel(localDateKey(reading.logged_at, 0)),
    })),
)

const latestWeight = computed(() => weightPoints.value.at(-1)?.value ?? null)
const weightDelta = computed(() => {
  if (weightPoints.value.length < 2) return null
  return weightPoints.value.at(-1).value - weightPoints.value[0].value
})

/** Daily macro totals, bucketed by each meal's own offset. */
const macroDays = computed(() => {
  const buckets = new Map()
  for (const meal of meals.value) {
    if (meal.status === 'Failed') continue
    const key = localDateKey(meal.eaten_at, meal.timezone_offset)
    if (!buckets.has(key)) buckets.set(key, { key, caption: dayLabel(key), protein_g: 0, fat_g: 0, carbs_g: 0 })
    const bucket = buckets.get(key)
    bucket.protein_g += meal.totals?.protein_g ?? 0
    bucket.fat_g += meal.totals?.fat_g ?? 0
    bucket.carbs_g += meal.totals?.carbs_g ?? 0
  }
  return [...buckets.values()].sort((a, b) => (a.key < b.key ? -1 : 1))
})

const macroAverage = computed(() => {
  if (!macroDays.value.length) return null
  const sum = macroDays.value.reduce(
    (acc, day) => ({
      protein_g: acc.protein_g + day.protein_g,
      fat_g: acc.fat_g + day.fat_g,
      carbs_g: acc.carbs_g + day.carbs_g,
    }),
    { protein_g: 0, fat_g: 0, carbs_g: 0 },
  )
  const n = macroDays.value.length
  return {
    protein_g: sum.protein_g / n,
    fat_g: sum.fat_g / n,
    carbs_g: sum.carbs_g / n,
    kcal: (sum.protein_g * 4 + sum.fat_g * 9 + sum.carbs_g * 4) / n,
  }
})

// --- Logging a weight -------------------------------------------------------

const newWeight = ref('')
const logging = ref(false)
const logNotice = ref('')

async function logWeight() {
  const value = Number.parseFloat(newWeight.value)
  if (!Number.isFinite(value)) return
  logging.value = true
  error.value = ''
  logNotice.value = ''
  try {
    const result = await api.logWeight({ weight_kg: value, source: 'Manual' })
    newWeight.value = ''
    logNotice.value = result.targets_recomputed
      ? 'Logged. Targets were recomputed from the new weight.'
      : 'Logged.'
    await load()
  } catch (e) {
    error.value = e.message
  } finally {
    logging.value = false
  }
}

onMounted(load)

</script>

<template>
  <div class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div>
        <p class="eyebrow">Where things are heading</p>
        <h1 class="mt-1.5 text-2xl font-semibold tracking-tight text-ink">Trends</h1>
      </div>
      <div class="flex gap-1.5">
        <Button
          v-for="option in RANGES"
          :key="option.days"
          size="sm"
          :variant="range === option.days ? 'secondary' : 'ghost'"
          @click="setRange(option.days)"
        >
          {{ option.label }}
        </Button>
      </div>
    </header>

    <div class="tick-rule" aria-hidden="true" />

    <p v-if="error" class="rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-sm text-critical">
      {{ error }}
    </p>
    <p v-if="logNotice" class="rounded-lg border border-good/40 bg-good/10 px-3 py-2 text-sm text-good">
      {{ logNotice }}
    </p>
    <p v-if="loading" class="text-sm text-ink-3">Loading trends…</p>

    <div class="grid grid-cols-[minmax(0,1fr)] gap-5 xl:grid-cols-2">
      <Card>
        <CardHeader>
          <CardTitle>Weight</CardTitle>
          <CardDescription>
            <template v-if="latestWeight !== null">
              <span class="num text-ink">{{ kg(latestWeight) }} kg</span> now<template v-if="weightDelta !== null">,
              <span class="num" :class="weightDelta <= 0 ? 'text-good' : 'text-serious'">
                {{ weightDelta > 0 ? '+' : '' }}{{ kg(weightDelta) }} kg
              </span> over the window</template>.
            </template>
            <template v-else>No readings yet.</template>
          </CardDescription>
        </CardHeader>
        <CardContent class="space-y-4">
          <LineChart
            v-if="weightPoints.length > 1"
            :points="weightPoints"
            unit="kg"
            :precision="1"
          />
          <EmptyState
            v-else
            title="Not enough readings to draw a line"
            hint="Log a weight below; targets recompute from the newest one."
          />

          <form class="flex flex-wrap items-end gap-2 border-t border-border pt-4" @submit.prevent="logWeight">
            <div class="min-w-32 flex-1 space-y-1.5">
              <Label for="weight">Log a weight</Label>
              <Input
                id="weight"
                v-model="newWeight"
                numeric
                type="number"
                step="0.1"
                min="20"
                max="400"
                placeholder="82.4"
              />
            </div>
            <Button type="submit" :disabled="logging || !newWeight">
              <Scale /> {{ logging ? 'Saving…' : 'Log kg' }}
            </Button>
          </form>

          <ul v-if="weights.length" class="num space-y-1 text-xs text-ink-3">
            <li v-for="reading in weights.slice(0, 5)" :key="reading.id" class="flex justify-between gap-3">
              <span class="font-sans">{{ stamp(reading.logged_at) }}</span>
              <span class="text-ink-2">{{ kg(reading.weight_kg) }} kg</span>
            </li>
          </ul>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Daily energy by macro</CardTitle>
          <CardDescription>
            <template v-if="macroAverage">
              Averaging <span class="num text-ink">{{ kcal(macroAverage.kcal) }} kcal</span> a day —
              <span class="num">{{ grams(macroAverage.protein_g) }} g</span> protein,
              <span class="num">{{ grams(macroAverage.fat_g) }} g</span> fat,
              <span class="num">{{ grams(macroAverage.carbs_g) }} g</span> carbs.
            </template>
            <template v-else>Nothing logged in this window.</template>
          </CardDescription>
        </CardHeader>
        <CardContent>
          <MacroTrendChart
            v-if="macroDays.length"
            :days="macroDays"
            :target="profile?.target_kcal ?? 0"
          />
          <EmptyState
            v-else
            title="No meals in this window"
            hint="Log a few days of meals and the split shows up here."
          />
        </CardContent>
      </Card>
    </div>
  </div>
</template>
