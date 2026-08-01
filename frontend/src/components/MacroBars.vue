<script setup>
/**
 * Protein, fat and carbohydrate against their floors — a bullet chart, not a
 * progress bar: the track is scaled to whichever is larger, consumed or target,
 * and the target sits on it as a rule. A bar that pins at 100% would hide the
 * only thing worth knowing on a day you went over.
 *
 * Every row is directly labelled, so identity never rests on colour alone and
 * no legend is needed.
 */
import { computed } from 'vue'
import { grams } from '@/lib/format'

const props = defineProps({
  /** `DayState`, or anything with the same consumed_/target_ fields. */
  state: { type: Object, required: true },
  /** Drop the target rules when there is nothing to compare against. */
  compact: { type: Boolean, default: false },
})

const ROWS = [
  { key: 'protein', label: 'Protein', colour: 'var(--color-protein)', floor: true },
  { key: 'fat', label: 'Fat', colour: 'var(--color-fat)', floor: true },
  { key: 'carbs', label: 'Carbs', colour: 'var(--color-carbs)', floor: false },
]

const rows = computed(() =>
  ROWS.map((row) => {
    const consumed = Math.max(0, props.state?.[`consumed_${row.key}_g`] ?? 0)
    const target = Math.max(0, props.state?.[`target_${row.key}_g`] ?? 0)
    const scale = Math.max(consumed, target, 1)
    return {
      ...row,
      consumed,
      target,
      hasTarget: target > 0,
      fillPercent: (consumed / scale) * 100,
      targetPercent: (target / scale) * 100,
      short: Math.max(0, target - consumed),
    }
  }),
)
</script>

<template>
  <div class="space-y-3.5">
    <div v-for="row in rows" :key="row.key">
      <div class="flex items-baseline justify-between gap-3">
        <div class="flex items-center gap-2">
          <span
            class="inline-block size-2 shrink-0 rounded-[2px]"
            :style="{ background: row.colour }"
            aria-hidden="true"
          />
          <span class="text-sm text-ink-2">{{ row.label }}</span>
        </div>
        <p class="num text-sm text-ink">
          {{ grams(row.consumed) }}<span
            v-if="row.hasTarget"
            class="text-ink-3"
          > / {{ grams(row.target) }}</span><span class="text-ink-3"> g</span>
        </p>
      </div>

      <div class="relative mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-grid">
        <div
          class="h-full rounded-full"
          :style="{ width: `${row.fillPercent}%`, background: row.colour }"
        />
      </div>

      <!-- The target rule sits outside the clipped track so it stays crisp. -->
      <div v-if="row.hasTarget && !compact" class="relative h-2">
        <span
          class="absolute top-0 h-1.5 w-px bg-rule"
          :style="{ left: `calc(${row.targetPercent}% - 0.5px)` }"
          aria-hidden="true"
        />
      </div>

      <p v-if="row.floor && row.hasTarget && row.short > 0 && !compact" class="mt-0.5 text-xs text-ink-3">
        <span class="num">{{ grams(row.short) }} g</span> short of the floor
      </p>
    </div>
  </div>
</template>
