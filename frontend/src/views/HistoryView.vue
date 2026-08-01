<script setup>
/**
 * History, photo-forward.
 *
 * The thumbnail is the memory hook — you recognise last Tuesday's lunch by the
 * picture long before you recognise it by the name. Days are bucketed by each
 * meal's own `timezone_offset`, not the browser's clock, so a meal eaten at
 * 00:30 in Moscow stays on the Moscow day it was eaten on.
 */
import { computed, onMounted, ref } from 'vue'
import { Layers, Utensils } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { kcal, localDateKey, localTime, relativeDayLabel, shiftDateKey, todayKey } from '@/lib/format'
import { Button } from '@/components/ui/button'
import EmptyState from '@/components/EmptyState.vue'

const RANGES = [
  { days: 7, label: '7 days' },
  { days: 30, label: '30 days' },
  { days: 90, label: '90 days' },
]

const range = ref(30)
const meals = ref([])
const loading = ref(true)
const error = ref('')
const expanded = ref(new Set())

async function load() {
  loading.value = true
  error.value = ''
  try {
    meals.value = await api.meals({
      from: shiftDateKey(todayKey(), -(range.value - 1)),
      to: todayKey(),
      limit: 500,
    })
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

function toggle(groupId) {
  const next = new Set(expanded.value)
  if (next.has(groupId)) next.delete(groupId)
  else next.add(groupId)
  expanded.value = next
}

/**
 * Bucket into local days, then collapse each sitting into a single card. The
 * API returns a flat list — grouping is a display concern, so it happens here.
 */
const days = computed(() => {
  const buckets = new Map()

  for (const meal of meals.value) {
    const key = localDateKey(meal.eaten_at, meal.timezone_offset)
    if (!buckets.has(key)) buckets.set(key, { key, cards: [], groups: new Map(), kcal: 0 })
    const bucket = buckets.get(key)
    bucket.kcal += meal.totals?.kcal ?? 0

    if (!meal.group_id) {
      bucket.cards.push({ kind: 'meal', id: meal.id, at: meal.eaten_at, meal })
      continue
    }

    let card = bucket.groups.get(meal.group_id)
    if (!card) {
      card = {
        kind: 'group',
        id: meal.group_id,
        at: meal.eaten_at,
        members: [],
        kcal: 0,
      }
      bucket.groups.set(meal.group_id, card)
      bucket.cards.push(card)
    }
    card.members.push(meal)
    card.kcal += meal.totals?.kcal ?? 0
    if (new Date(meal.eaten_at) < new Date(card.at)) card.at = meal.eaten_at
  }

  return [...buckets.values()]
    .sort((a, b) => (a.key < b.key ? 1 : -1))
    .map((bucket) => ({
      ...bucket,
      cards: bucket.cards.sort((a, b) => new Date(b.at) - new Date(a.at)),
    }))
})

const thumbnail = (meal) => (meal.thumbnail_url ? api.thumbnailUrl(meal.id) : null)

onMounted(load)

</script>

<template>
  <div class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div>
        <p class="eyebrow">Everything logged</p>
        <h1 class="mt-1.5 text-2xl font-semibold tracking-tight text-ink">History</h1>
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
    <p v-else-if="loading" class="text-sm text-ink-3">Loading history…</p>

    <EmptyState
      v-else-if="!days.length"
      title="Nothing in this window"
      hint="Widen the range, or log a meal from the phone app."
    />

    <section v-for="day in days" :key="day.key" class="space-y-3">
      <div class="flex items-baseline justify-between gap-3">
        <router-link
          :to="{ name: 'day', params: { date: day.key } }"
          class="text-sm font-semibold text-ink hover:underline"
        >
          {{ relativeDayLabel(day.key) }}
        </router-link>
        <p class="num text-xs text-ink-3">{{ kcal(day.kcal) }} kcal</p>
      </div>

      <div class="grid grid-cols-2 gap-2.5 sm:grid-cols-3 md:grid-cols-4 xl:grid-cols-5">
        <template v-for="card in day.cards" :key="card.id">
          <!-- A sitting: one card until you open it -->
          <button
            v-if="card.kind === 'group'"
            type="button"
            class="group overflow-hidden rounded-lg border border-border bg-black/20 text-left transition-colors hover:border-rule focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            :aria-expanded="expanded.has(card.id)"
            @click="toggle(card.id)"
          >
            <div class="relative aspect-square w-full bg-black/40">
              <img
                v-if="thumbnail(card.members[0])"
                :src="thumbnail(card.members[0])"
                alt=""
                loading="lazy"
                class="size-full object-cover opacity-80"
              >
              <Layers v-else class="absolute inset-0 m-auto size-5 text-ink-3" aria-hidden="true" />
              <span
                class="num absolute right-1.5 top-1.5 rounded-md border border-border bg-black/70 px-1.5 py-0.5 text-[11px] text-ink"
              >{{ card.members.length }}</span>
            </div>
            <div class="p-2">
              <p class="truncate text-xs font-medium text-ink">
                {{ expanded.has(card.id) ? 'Hide the sitting' : `${card.members.length} dishes` }}
              </p>
              <p class="num mt-0.5 text-[11px] text-ink-3">{{ kcal(card.kcal) }} kcal</p>
            </div>
          </button>

          <!-- Expanded members follow their sitting in place -->
          <template v-if="card.kind === 'group' && expanded.has(card.id)">
            <router-link
              v-for="member in card.members"
              :key="member.id"
              :to="{ name: 'meal', params: { id: member.id } }"
              class="overflow-hidden rounded-lg border border-rule bg-black/20 transition-colors hover:border-ink-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <div class="relative aspect-square w-full bg-black/40">
                <img
                  v-if="thumbnail(member)"
                  :src="thumbnail(member)"
                  :alt="member.dish_name || 'Meal photo'"
                  loading="lazy"
                  class="size-full object-cover"
                >
                <Utensils v-else class="absolute inset-0 m-auto size-5 text-ink-3" aria-hidden="true" />
              </div>
              <div class="p-2">
                <p class="truncate text-xs font-medium text-ink">{{ member.dish_name || 'Unnamed' }}</p>
                <p class="num mt-0.5 text-[11px] text-ink-3">{{ kcal(member.totals.kcal) }} kcal</p>
              </div>
            </router-link>
          </template>

          <!-- A solo meal -->
          <router-link
            v-if="card.kind === 'meal'"
            :to="{ name: 'meal', params: { id: card.meal.id } }"
            class="overflow-hidden rounded-lg border border-border bg-black/20 transition-colors hover:border-rule focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <div class="relative aspect-square w-full bg-black/40">
              <img
                v-if="thumbnail(card.meal)"
                :src="thumbnail(card.meal)"
                :alt="card.meal.dish_name || 'Meal photo'"
                loading="lazy"
                class="size-full object-cover"
              >
              <Utensils v-else class="absolute inset-0 m-auto size-5 text-ink-3" aria-hidden="true" />
              <span class="num absolute left-1.5 top-1.5 rounded-md bg-black/70 px-1.5 py-0.5 text-[11px] text-ink-2">
                {{ localTime(card.meal.eaten_at, card.meal.timezone_offset) }}
              </span>
            </div>
            <div class="p-2">
              <p class="truncate text-xs font-medium text-ink">{{ card.meal.dish_name || 'Unnamed' }}</p>
              <p class="num mt-0.5 text-[11px] text-ink-3">{{ kcal(card.meal.totals.kcal) }} kcal</p>
            </div>
          </router-link>
        </template>
      </div>
    </section>
  </div>
</template>
