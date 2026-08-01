<script setup>
/**
 * One line of the ledger. Numbers right-aligned in the mono face so a column of
 * meals reads as a column of measurements.
 */
import { computed } from 'vue'
import { Utensils } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { grams, kcal, localTime } from '@/lib/format'
import StatusPill from '@/components/StatusPill.vue'

const props = defineProps({
  meal: { type: Object, required: true },
  /** Inset rows sit inside an expanded sitting. */
  nested: { type: Boolean, default: false },
})

const time = computed(() => localTime(props.meal.eaten_at, props.meal.timezone_offset))
const name = computed(() => props.meal.dish_name || 'Unnamed meal')
const thumbnail = computed(() =>
  props.meal.thumbnail_url ? api.thumbnailUrl(props.meal.id) : null,
)
const pending = computed(() => ['Pending', 'Analyzing'].includes(props.meal.status))
</script>

<template>
  <router-link
    :to="{ name: 'meal', params: { id: meal.id } }"
    class="group flex items-center gap-3 rounded-lg px-2 py-2.5 transition-colors hover:bg-white/[0.04] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    :class="nested && 'pl-3'"
  >
    <div
      class="relative size-11 shrink-0 overflow-hidden rounded-md border border-border bg-black/30"
    >
      <img
        v-if="thumbnail"
        :src="thumbnail"
        :alt="`Photo of ${name}`"
        loading="lazy"
        class="size-full object-cover"
      >
      <Utensils v-else class="absolute inset-0 m-auto size-4 text-ink-3" aria-hidden="true" />
    </div>

    <div class="min-w-0 flex-1">
      <p class="truncate text-sm font-medium text-ink">{{ name }}</p>
      <div class="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-ink-3">
        <span class="num">{{ time }}</span>
        <StatusPill v-if="meal.status !== 'Confirmed'" :meal="meal.status" />
        <span v-if="meal.revision > 1" class="num">rev {{ meal.revision }}</span>
        <span v-if="meal.error" class="truncate text-critical">{{ meal.error }}</span>
      </div>
    </div>

    <div class="shrink-0 text-right">
      <p class="num text-sm font-semibold text-ink">
        <template v-if="pending">—</template>
        <template v-else>{{ kcal(meal.totals.kcal) }}<span class="text-ink-3"> kcal</span></template>
      </p>
      <p v-if="!pending" class="num mt-0.5 text-[11px] text-ink-3">
        <span :style="{ color: 'var(--color-protein)' }">P</span> {{ grams(meal.totals.protein_g) }}
        <span :style="{ color: 'var(--color-fat)' }">F</span> {{ grams(meal.totals.fat_g) }}
        <span :style="{ color: 'var(--color-carbs)' }">C</span> {{ grams(meal.totals.carbs_g) }}
      </p>
    </div>
  </router-link>
</template>
