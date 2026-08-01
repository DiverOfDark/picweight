<script setup>
import { useVModel } from '@vueuse/core'
import { cn } from '@/lib/utils'

const props = defineProps({
  defaultValue: { type: String, default: '' },
  modelValue: { type: String, default: '' },
  class: { type: null, default: '' },
})

const emits = defineEmits(['update:modelValue'])

const modelValue = useVModel(props, 'modelValue', emits, {
  passive: true,
  defaultValue: props.defaultValue,
})
</script>

<template>
  <textarea
    v-model="modelValue"
    :class="cn(
      'flex min-h-20 w-full resize-y rounded-lg border border-input bg-black/25 px-3 py-2 text-sm leading-relaxed text-ink placeholder:text-ink-3 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50',
      props.class,
    )"
  />
</template>
