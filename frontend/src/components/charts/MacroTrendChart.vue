<script setup>
/**
 * Daily energy, split by where it came from.
 *
 * Stacked because the parts sum to a meaningful whole — the day's kcal — and
 * that whole is what the target line is drawn against. Segments carry a 2px
 * surface gap so adjacent hues never touch. One axis only; the macro grams live
 * in the tooltip rather than on a second scale.
 */
import { computed, ref } from 'vue'
import { useElementSize } from '@vueuse/core'
import { linear, niceTicks } from '@/lib/chart'
import { grams, kcal } from '@/lib/format'

const props = defineProps({
  /** `[{ key, caption, protein_g, fat_g, carbs_g }]`, oldest first. */
  days: { type: Array, required: true },
  /** Daily energy target, drawn as a reference rule when set. */
  target: { type: Number, default: 0 },
  height: { type: Number, default: 240 },
})

/** Atwater factors: the split is energy, so grams have to be converted once. */
const SERIES = [
  { key: 'protein_g', label: 'Protein', factor: 4, colour: 'var(--color-protein)' },
  { key: 'fat_g', label: 'Fat', factor: 9, colour: 'var(--color-fat)' },
  { key: 'carbs_g', label: 'Carbs', factor: 4, colour: 'var(--color-carbs)' },
]

const wrapper = ref(null)
const { width } = useElementSize(wrapper)

const PAD = { top: 14, right: 14, bottom: 26, left: 46 }
const GAP = 2

const plotWidth = computed(() => Math.max(120, (width.value || 640) - PAD.left - PAD.right))
const plotHeight = computed(() => props.height - PAD.top - PAD.bottom)

const totals = computed(() =>
  props.days.map((day) =>
    SERIES.reduce((sum, series) => sum + (day[series.key] ?? 0) * series.factor, 0),
  ),
)

const axis = computed(() =>
  niceTicks(0, Math.max(props.target, ...totals.value, 1), 4),
)
const scaleY = computed(() => linear(axis.value.domain, [PAD.top + plotHeight.value, PAD.top]))

const slot = computed(() => plotWidth.value / Math.max(1, props.days.length))
const barWidth = computed(() => Math.max(3, Math.min(34, slot.value - 6)))

const bars = computed(() =>
  props.days.map((day, index) => {
    const x = PAD.left + slot.value * index + (slot.value - barWidth.value) / 2
    let cursor = PAD.top + plotHeight.value
    const segments = []
    for (const series of SERIES) {
      const value = (day[series.key] ?? 0) * series.factor
      const height = (PAD.top + plotHeight.value) - scaleY.value(value) - GAP
      if (height > 0.5) {
        cursor -= height
        segments.push({ ...series, y: cursor, height, kcal: value })
        cursor -= GAP
      }
    }
    return { ...day, x, segments, total: totals.value[index] }
  }),
)

const hovered = ref(null)

const tooltipStyle = computed(() => {
  if (!hovered.value) return {}
  const total = width.value || 640
  const left = Math.min(Math.max(hovered.value.x + barWidth.value / 2, 80), total - 80)
  return { left: `${left}px`, top: `${PAD.top}px` }
})
</script>

<template>
  <div ref="wrapper" class="w-full">
    <!-- Legend: three series, so identity is never colour-alone -->
    <ul class="mb-2 flex flex-wrap gap-x-4 gap-y-1">
      <li v-for="series in SERIES" :key="series.key" class="flex items-center gap-1.5 text-xs text-ink-2">
        <span
          class="inline-block size-2 rounded-[2px]"
          :style="{ background: series.colour }"
          aria-hidden="true"
        />
        {{ series.label }}
      </li>
    </ul>

    <div class="relative w-full">
      <svg
        :width="width || '100%'"
        :height="height"
        class="block w-full"
        role="img"
        aria-label="Daily energy split by macronutrient"
        @pointerleave="hovered = null"
      >
        <g>
          <template v-for="tick in axis.ticks" :key="tick">
            <line
              :x1="PAD.left"
              :x2="PAD.left + plotWidth"
              :y1="scaleY(tick)"
              :y2="scaleY(tick)"
              stroke="var(--color-grid)"
              stroke-width="1"
            />
            <text
              :x="PAD.left - 8"
              :y="scaleY(tick) + 3"
              text-anchor="end"
              class="num"
              font-size="10"
              fill="var(--color-ink-3)"
            >{{ kcal(tick) }}</text>
          </template>
        </g>

        <g v-for="bar in bars" :key="bar.key">
          <rect
            v-for="segment in bar.segments"
            :key="segment.key"
            :x="bar.x"
            :y="segment.y"
            :width="barWidth"
            :height="segment.height"
            :fill="segment.colour"
            rx="2"
          />
          <!-- Hit target spans the whole slot, so a thin bar is still easy to hover -->
          <rect
            :x="bar.x - 3"
            :y="PAD.top"
            :width="barWidth + 6"
            :height="plotHeight"
            fill="transparent"
            @pointerenter="hovered = bar"
          />
        </g>

        <g v-if="target > 0">
          <line
            :x1="PAD.left"
            :x2="PAD.left + plotWidth"
            :y1="scaleY(target)"
            :y2="scaleY(target)"
            stroke="var(--color-rule)"
            stroke-width="1"
            stroke-dasharray="4 4"
          />
          <!-- Plated on the surface colour so the label stays readable where a
               tall bar runs underneath it. -->
          <rect
            :x="PAD.left + plotWidth - 78"
            :y="scaleY(target) - 15"
            width="78"
            height="13"
            fill="var(--color-card)"
            rx="2"
          />
          <text
            :x="PAD.left + plotWidth - 2"
            :y="scaleY(target) - 5"
            text-anchor="end"
            font-size="10"
            fill="var(--color-ink-2)"
          >target {{ kcal(target) }}</text>
        </g>

        <text
          v-if="bars.length"
          :x="PAD.left"
          :y="height - 8"
          font-size="10"
          fill="var(--color-ink-3)"
        >{{ bars[0].caption }}</text>
        <text
          v-if="bars.length > 1"
          :x="PAD.left + plotWidth"
          :y="height - 8"
          text-anchor="end"
          font-size="10"
          fill="var(--color-ink-3)"
        >{{ bars[bars.length - 1].caption }}</text>
      </svg>

      <div
        v-if="hovered"
        class="pointer-events-none absolute w-40 -translate-x-1/2 rounded-md border border-border bg-popover px-2.5 py-2 text-xs shadow-lg shadow-black/50"
        :style="tooltipStyle"
      >
        <p class="text-ink-3">{{ hovered.caption }}</p>
        <p class="num mt-0.5 font-semibold text-ink">{{ kcal(hovered.total) }} kcal</p>
        <ul class="mt-1.5 space-y-0.5">
          <li v-for="series in SERIES" :key="series.key" class="flex items-center gap-1.5">
            <span
              class="inline-block size-2 shrink-0 rounded-[2px]"
              :style="{ background: series.colour }"
              aria-hidden="true"
            />
            <span class="flex-1 text-ink-2">{{ series.label }}</span>
            <span class="num text-ink">{{ grams(hovered[series.key]) }} g</span>
          </li>
        </ul>
      </div>
    </div>
  </div>
</template>
