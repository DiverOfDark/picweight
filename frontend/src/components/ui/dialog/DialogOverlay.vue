<script setup>
import { DialogOverlay } from 'reka-ui'
import { cn } from '@/lib/utils'

const props = defineProps({
  class: { type: null, default: '' },
})
</script>

<template>
  <DialogOverlay
    :class="cn(
      'fixed inset-0 z-50 bg-black/70 backdrop-blur-sm data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0',
      props.class,
    )"
  />
</template>
