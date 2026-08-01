<script setup>
import { DialogTitle } from 'reka-ui'
import { cn } from '@/lib/utils'

const props = defineProps({
  class: { type: null, default: '' },
})
</script>

<template>
  <DialogTitle
    :class="cn('text-lg font-semibold leading-none tracking-tight text-ink', props.class)"
  >
    <slot />
  </DialogTitle>
</template>
