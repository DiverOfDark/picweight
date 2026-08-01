<script setup>
import { useVModel } from '@vueuse/core'
import { cn } from '@/lib/utils'

const props = defineProps({
  defaultValue: { type: [String, Number], default: '' },
  modelValue: { type: [String, Number], default: '' },
  /** Numeric inputs wear the mono face — a measurement is always mono here. */
  numeric: { type: Boolean, default: false },
  class: { type: null, default: '' },
})

const emits = defineEmits(['update:modelValue'])

const modelValue = useVModel(props, 'modelValue', emits, {
  passive: true,
  defaultValue: props.defaultValue,
})
</script>

<template>
  <input
    v-model="modelValue"
    :class="cn(
      'flex h-10 w-full rounded-lg border border-input bg-black/25 px-3 py-2 text-sm text-ink placeholder:text-ink-3 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50',
      numeric && 'num',
      props.class,
    )"
  >
</template>
