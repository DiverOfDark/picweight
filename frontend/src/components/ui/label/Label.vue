<script setup>
import { cn } from '@/lib/utils'

const props = defineProps({
  for: { type: String, default: undefined },
  class: { type: null, default: '' },
})
</script>

<template>
  <label
    :for="props.for"
    :class="cn(
      'text-sm font-medium leading-none text-ink-2 peer-disabled:cursor-not-allowed peer-disabled:opacity-70',
      props.class,
    )"
  >
    <slot />
  </label>
</template>
