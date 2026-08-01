<script setup>
/**
 * The signature element: the day's energy budget read off a scale bezel.
 *
 * Not a soft progress donut. It is marked like a kitchen scale — a minor tick
 * every 100 kcal, a major every 500 — because the one claim the product makes
 * is that these numbers were *measured*, and the dial should say so. The arc
 * takes `--day-accent`, so the instrument changes colour with the verdict.
 */
import { computed } from 'vue'
import { arcPath, clamp, polar } from '@/lib/chart'
import { kcal } from '@/lib/format'
import { dayStatus } from '@/lib/status'

const props = defineProps({
  /** `DayState` from the API. */
  state: { type: Object, required: true },
})

const SIZE = 260
const CENTRE = SIZE / 2
const RADIUS = 98
const START = -135
const SWEEP = 270

const consumed = computed(() => Math.max(0, props.state?.consumed_kcal ?? 0))
const target = computed(() => Math.max(0, props.state?.target_kcal ?? 0))
const hasTarget = computed(() => target.value > 0)

const fraction = computed(() =>
  hasTarget.value ? clamp(consumed.value / target.value, 0, 1) : 0,
)
const overFraction = computed(() =>
  hasTarget.value ? clamp((consumed.value - target.value) / target.value, 0, 1) : 0,
)

const trackPath = computed(() => arcPath(CENTRE, CENTRE, RADIUS, START, START + SWEEP))
const valuePath = computed(() =>
  fraction.value > 0 ? arcPath(CENTRE, CENTRE, RADIUS, START, START + SWEEP * fraction.value) : '',
)
const overPath = computed(() =>
  overFraction.value > 0
    ? arcPath(CENTRE, CENTRE, RADIUS - 13, START, START + SWEEP * overFraction.value)
    : '',
)

/** Ticks every 100 kcal, longer every 500. Capped so a huge target stays legible. */
const ticks = computed(() => {
  const total = hasTarget.value ? target.value : 2000
  let stepKcal = 100
  while (total / stepKcal > 26) stepKcal *= 2
  const out = []
  for (let value = 0; value <= total + 1; value += stepKcal) {
    const angle = START + SWEEP * (value / total)
    const major = value % (stepKcal * 5) === 0
    const outer = polar(CENTRE, CENTRE, RADIUS + 13, angle)
    const inner = polar(CENTRE, CENTRE, RADIUS + (major ? 3 : 7), angle)
    out.push({ value, major, x1: inner.x, y1: inner.y, x2: outer.x, y2: outer.y })
  }
  return out
})

const remaining = computed(() => (props.state?.remaining_kcal ?? 0))
const over = computed(() => remaining.value < 0)
const status = computed(() => dayStatus(props.state?.status))

const readout = computed(() => (hasTarget.value ? kcal(Math.abs(remaining.value)) : kcal(consumed.value)))
const readoutLabel = computed(() => {
  if (!hasTarget.value) return 'kcal logged'
  return over.value ? 'kcal over' : 'kcal left'
})

const ariaLabel = computed(() => {
  if (!hasTarget.value) return `${kcal(consumed.value)} kilocalories logged. No target set.`
  return `${kcal(consumed.value)} of ${kcal(target.value)} kilocalories consumed, ${kcal(Math.abs(remaining.value))} ${over.value ? 'over' : 'left'}. ${status.value.label}.`
})
</script>

<template>
  <figure class="flex flex-col items-center">
    <svg
      :viewBox="`0 0 ${SIZE} ${SIZE}`"
      class="w-full max-w-[260px]"
      role="img"
      :aria-label="ariaLabel"
    >
      <!-- Scale bezel -->
      <g stroke-linecap="butt">
        <line
          v-for="tick in ticks"
          :key="tick.value"
          :x1="tick.x1"
          :y1="tick.y1"
          :x2="tick.x2"
          :y2="tick.y2"
          :stroke="tick.major ? 'var(--color-rule)' : 'var(--color-ink-3)'"
          :stroke-width="tick.major ? 1.5 : 1"
          :opacity="tick.major ? 1 : 0.55"
        />
      </g>

      <!-- Track -->
      <path
        :d="trackPath"
        fill="none"
        stroke="var(--color-grid)"
        stroke-width="14"
        stroke-linecap="round"
      />

      <!-- Consumed -->
      <path
        v-if="valuePath"
        :d="valuePath"
        fill="none"
        stroke="var(--day-accent)"
        stroke-width="14"
        stroke-linecap="round"
      />

      <!-- Overshoot rides an inner track so it can never be mistaken for progress -->
      <path
        v-if="overPath"
        :d="overPath"
        fill="none"
        stroke="var(--color-critical)"
        stroke-width="5"
        stroke-linecap="round"
      />

      <text
        :x="CENTRE"
        :y="CENTRE - 22"
        text-anchor="middle"
        class="eyebrow"
        fill="var(--color-ink-3)"
      >
        {{ hasTarget ? 'Remaining' : 'Consumed' }}
      </text>
      <text
        :x="CENTRE"
        :y="CENTRE + 24"
        text-anchor="middle"
        class="num"
        :fill="over ? 'var(--color-critical)' : 'var(--color-ink)'"
        font-size="52"
        font-weight="600"
      >{{ readout }}</text>
      <text
        :x="CENTRE"
        :y="CENTRE + 46"
        text-anchor="middle"
        fill="var(--color-ink-3)"
        font-size="12"
      >{{ readoutLabel }}</text>
    </svg>

    <figcaption class="num mt-1 text-sm text-ink-2">
      <template v-if="hasTarget">
        {{ kcal(consumed) }} <span class="text-ink-3">/ {{ kcal(target) }} kcal</span>
      </template>
      <template v-else>
        <span class="font-sans text-ink-3">Set your body data to get a target.</span>
      </template>
    </figcaption>
  </figure>
</template>
