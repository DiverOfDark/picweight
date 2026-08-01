<script setup>
/**
 * The mark is the gauge, reduced: a 270° bezel with its ticks and a weighted
 * arc. Same geometry as the kcal ring, so the logo and the instrument are
 * plainly the same object.
 */
import { computed } from 'vue'
import { arcPath, polar } from '@/lib/chart'

defineProps({
  class: { type: null, default: '' },
})

const START = -135
const SWEEP = 270

const track = arcPath(16, 16, 11, START, START + SWEEP)
const value = arcPath(16, 16, 11, START, START + SWEEP * 0.62)

const ticks = computed(() =>
  Array.from({ length: 7 }, (_, index) => {
    const angle = START + (SWEEP / 6) * index
    const outer = polar(16, 16, 15, angle)
    const inner = polar(16, 16, 13.5, angle)
    return { key: index, x1: inner.x, y1: inner.y, x2: outer.x, y2: outer.y }
  }),
)
</script>

<template>
  <svg viewBox="0 0 32 32" :class="$props.class" role="img" aria-label="picweight">
    <g stroke="currentColor" stroke-width="1" opacity="0.45">
      <line v-for="tick in ticks" :key="tick.key" :x1="tick.x1" :y1="tick.y1" :x2="tick.x2" :y2="tick.y2" />
    </g>
    <path :d="track" fill="none" stroke="currentColor" stroke-width="3" opacity="0.2" stroke-linecap="round" />
    <path :d="value" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
    <circle cx="16" cy="16" r="2" fill="currentColor" />
  </svg>
</template>
