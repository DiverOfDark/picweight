<script setup>
/**
 * A sitting, collapsed.
 *
 * N photos of one restaurant order are N independent meals with N independent
 * agent loops — grouping is purely a display and notification concern (PRD §5).
 * So this row shows the combined totals, and expanding it fetches the members
 * and hands each back to the ordinary meal row, still independently correctable.
 */
import { ref } from 'vue'
import { ChevronDown, Layers, TriangleAlert } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { grams, kcal, localTime } from '@/lib/format'
import MealRow from '@/components/MealRow.vue'

const props = defineProps({
  /** A `GroupSummary` from the day view. */
  group: { type: Object, required: true },
  /**
   * Minutes east of UTC to render `eaten_at` in; the members carry their own.
   * `null` — the default — means the viewer's own offset at that instant, which
   * is the right answer for a `GroupSummary`, since it has no offset field.
   */
  timezoneOffset: { type: Number, default: null },
})

const expanded = ref(false)
const members = ref([])
const loading = ref(false)
const error = ref('')

async function toggle() {
  expanded.value = !expanded.value
  if (!expanded.value || members.value.length || loading.value) return

  loading.value = true
  error.value = ''
  try {
    const sitting = await api.group(props.group.group_id)
    members.value = sitting.meals ?? []
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="rounded-lg border border-border bg-black/20">
    <button
      type="button"
      class="flex w-full items-center gap-3 rounded-lg px-2 py-2.5 text-left transition-colors hover:bg-white/[0.04] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      :aria-expanded="expanded"
      @click="toggle"
    >
      <div
        class="relative flex size-11 shrink-0 items-center justify-center rounded-md border border-border bg-black/30"
      >
        <Layers class="size-4 text-ink-2" aria-hidden="true" />
        <span
          class="num absolute -bottom-1 -right-1 rounded-full border border-border bg-card px-1 text-[10px] leading-4 text-ink-2"
        >{{ group.member_count }}</span>
      </div>

      <div class="min-w-0 flex-1">
        <p class="truncate text-sm font-medium text-ink">
          {{ group.dish_names.filter(Boolean).join(' · ') || `${group.member_count} dishes` }}
        </p>
        <div class="mt-0.5 flex flex-wrap items-center gap-x-2 text-xs text-ink-3">
          <span class="num">{{ localTime(group.eaten_at, timezoneOffset) }}</span>
          <span>one sitting, {{ group.member_count }} dishes</span>
          <span v-if="group.has_failures" class="inline-flex items-center gap-1 text-serious">
            <TriangleAlert class="size-3" aria-hidden="true" />
            a dish failed
          </span>
        </div>
      </div>

      <div class="shrink-0 text-right">
        <p class="num text-sm font-semibold text-ink">
          {{ kcal(group.totals.kcal) }}<span class="text-ink-3"> kcal</span>
        </p>
        <p class="num mt-0.5 text-[11px] text-ink-3">
          <span :style="{ color: 'var(--color-protein)' }">P</span> {{ grams(group.totals.protein_g) }}
          <span :style="{ color: 'var(--color-fat)' }">F</span> {{ grams(group.totals.fat_g) }}
          <span :style="{ color: 'var(--color-carbs)' }">C</span> {{ grams(group.totals.carbs_g) }}
        </p>
      </div>

      <ChevronDown
        class="size-4 shrink-0 text-ink-3 transition-transform"
        :class="expanded && 'rotate-180'"
        aria-hidden="true"
      />
    </button>

    <div v-if="expanded" class="border-t border-border px-2 pb-1 pt-1">
      <p v-if="loading" class="px-2 py-3 text-xs text-ink-3">Loading the sitting…</p>
      <p v-else-if="error" class="px-2 py-3 text-xs text-critical">{{ error }}</p>
      <MealRow v-for="meal in members" :key="meal.id" :meal="meal" nested />
    </div>
  </div>
</template>
