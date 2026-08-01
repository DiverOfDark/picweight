<script setup>
/**
 * A lifecycle or day status, always icon + word. Colour never carries the
 * meaning on its own.
 */
import { computed } from 'vue'
import { MEAL_STATUS } from '@/lib/enums'
import {
  AlertTriangle,
  Check,
  CircleAlert,
  Clock,
  Eye,
  Loader2,
} from 'lucide-vue-next'
import { cn } from '@/lib/utils'
import { label } from '@/lib/format'
import { MEAL_TONE, TONE_CLASS, dayStatus } from '@/lib/status'

const props = defineProps({
  /** A `MealStatus` variant. */
  meal: { type: String, default: null },
  /** A `DayStatus` variant. */
  day: { type: String, default: null },
  class: { type: null, default: '' },
})

const ICONS = {
  [MEAL_STATUS.PENDING]: Clock,
  [MEAL_STATUS.ANALYZING]: Loader2,
  [MEAL_STATUS.NEEDS_REVIEW]: Eye,
  [MEAL_STATUS.CONFIRMED]: Check,
  [MEAL_STATUS.FAILED]: CircleAlert,
  on_track: Check,
  tight: AlertTriangle,
  over: CircleAlert,
  protein_unreachable: AlertTriangle,
  no_targets: Clock,
}

const tone = computed(() =>
  props.meal ? (MEAL_TONE[props.meal] ?? 'muted') : dayStatus(props.day).tone,
)
const text = computed(() => (props.meal ? label(props.meal) : dayStatus(props.day).label))
const icon = computed(() => ICONS[props.meal ?? props.day] ?? Clock)
const spinning = computed(() => props.meal === MEAL_STATUS.ANALYZING)
</script>

<template>
  <span
    :class="cn(
      'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[11px] font-medium leading-tight',
      TONE_CLASS[tone],
      props.class,
    )"
  >
    <component :is="icon" :class="cn('size-3', spinning && 'animate-spin')" aria-hidden="true" />
    {{ text }}
  </span>
</template>
