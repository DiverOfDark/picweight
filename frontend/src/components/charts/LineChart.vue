<script setup>
/**
 * A single-series trend line — weight, currently.
 *
 * One series, so no legend: the title names it. Hover gives a crosshair and a
 * tooltip, because an SVG chart in a browser is interactive by default.
 */
import { computed, ref } from 'vue'
import { useElementSize } from '@vueuse/core'
import { linePath, linear, niceTicks } from '@/lib/chart'

const props = defineProps({
  /** `[{ at: Date | string, value: number, caption: string }]`, oldest first. */
  points: { type: Array, required: true },
  /**
   * Series colour. The default is an *ink* token rather than a categorical
   * slot on purpose: this chart carries one series, its identity comes from
   * the card title, and the three categorical hues are spoken for by the macro
   * chart beside it — a fourth hue does not clear the colourblind-separation
   * floor against those three, so the method says take an ink, not a hue.
   */
  colour: { type: String, default: 'var(--color-ink-2)' },
  /** Appended to every value in the tooltip and the axis. */
  unit: { type: String, default: '' },
  /** Decimal places on the axis and tooltip. */
  precision: { type: Number, default: 1 },
  height: { type: Number, default: 220 },
})

const wrapper = ref(null)
const { width } = useElementSize(wrapper)

const PAD = { top: 14, right: 14, bottom: 26, left: 46 }

const plotWidth = computed(() => Math.max(120, (width.value || 640) - PAD.left - PAD.right))
const plotHeight = computed(() => props.height - PAD.top - PAD.bottom)

const values = computed(() => props.points.map((point) => point.value))
const axis = computed(() => niceTicks(Math.min(...values.value), Math.max(...values.value), 4))

const scaleY = computed(() =>
  linear(axis.value.domain, [PAD.top + plotHeight.value, PAD.top]),
)
const scaleX = computed(() =>
  linear([0, Math.max(1, props.points.length - 1)], [PAD.left, PAD.left + plotWidth.value]),
)

const laid = computed(() =>
  props.points.map((point, index) => ({
    ...point,
    x: scaleX.value(index),
    y: scaleY.value(point.value),
  })),
)

const path = computed(() => linePath(laid.value))

const hovered = ref(null)

function onMove(event) {
  if (!laid.value.length) return
  const box = event.currentTarget.getBoundingClientRect()
  const x = event.clientX - box.left
  let nearest = laid.value[0]
  for (const point of laid.value) {
    if (Math.abs(point.x - x) < Math.abs(nearest.x - x)) nearest = point
  }
  hovered.value = nearest
}

const format = (value) => `${value.toFixed(props.precision)}${props.unit ? ` ${props.unit}` : ''}`

/** Keep the tooltip inside the plot rather than letting it hang off an edge. */
const tooltipStyle = computed(() => {
  if (!hovered.value) return {}
  const total = width.value || 640
  const left = Math.min(Math.max(hovered.value.x, 70), total - 70)
  return { left: `${left}px`, top: `${Math.max(0, hovered.value.y - 12)}px` }
})
</script>

<template>
  <div ref="wrapper" class="relative w-full">
    <svg
      :width="width || '100%'"
      :height="height"
      class="block w-full"
      role="img"
      :aria-label="`Trend of ${points.length} readings`"
      @pointermove="onMove"
      @pointerleave="hovered = null"
    >
      <!-- Gridlines, recessive by design -->
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
          >{{ tick.toFixed(precision) }}</text>
        </template>
      </g>

      <path :d="path" fill="none" :stroke="colour" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" />

      <circle
        v-for="point in laid"
        :key="`${point.x}-${point.y}`"
        :cx="point.x"
        :cy="point.y"
        r="3.5"
        :fill="colour"
        stroke="var(--color-card)"
        stroke-width="1.5"
      />

      <g v-if="hovered">
        <line
          :x1="hovered.x"
          :x2="hovered.x"
          :y1="PAD.top"
          :y2="PAD.top + plotHeight"
          stroke="var(--color-rule)"
          stroke-width="1"
        />
        <circle
          :cx="hovered.x"
          :cy="hovered.y"
          r="5"
          :fill="colour"
          stroke="var(--color-card)"
          stroke-width="2"
        />
      </g>

      <!-- First and last captions anchor the axis without crowding it -->
      <text
        v-if="laid.length"
        :x="PAD.left"
        :y="height - 8"
        font-size="10"
        fill="var(--color-ink-3)"
      >{{ laid[0].caption }}</text>
      <text
        v-if="laid.length > 1"
        :x="PAD.left + plotWidth"
        :y="height - 8"
        text-anchor="end"
        font-size="10"
        fill="var(--color-ink-3)"
      >{{ laid[laid.length - 1].caption }}</text>
    </svg>

    <div
      v-if="hovered"
      class="pointer-events-none absolute -translate-x-1/2 -translate-y-full rounded-md border border-border bg-popover px-2 py-1 text-xs shadow-lg shadow-black/50"
      :style="tooltipStyle"
    >
      <p class="num font-semibold text-ink">{{ format(hovered.value) }}</p>
      <p class="text-ink-3">{{ hovered.caption }}</p>
    </div>
  </div>
</template>
