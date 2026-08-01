<script setup>
import { DialogClose, DialogContent, DialogPortal, useForwardPropsEmits } from 'reka-ui'
import { X } from 'lucide-vue-next'
import DialogOverlay from '../dialog/DialogOverlay.vue'
import { cn } from '@/lib/utils'

const props = defineProps({
  class: { type: null, default: '' },
})

const emits = defineEmits([
  'escapeKeyDown',
  'pointerDownOutside',
  'focusOutside',
  'interactOutside',
  'openAutoFocus',
  'closeAutoFocus',
])
const forwarded = useForwardPropsEmits(props, emits)
</script>

<template>
  <DialogPortal>
    <DialogOverlay />
    <DialogContent
      v-bind="forwarded"
      :class="cn(
        'fixed inset-y-0 right-0 z-50 flex h-full w-full max-w-md flex-col gap-6 overflow-y-auto border-l border-border bg-popover p-6 shadow-2xl shadow-black/60 transition duration-300 ease-in-out data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right',
        props.class,
      )"
    >
      <slot />

      <DialogClose
        class="absolute right-4 top-4 rounded-md p-1 text-ink-3 transition-colors hover:bg-white/5 hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <X class="h-4 w-4" />
        <span class="sr-only">Close</span>
      </DialogClose>
    </DialogContent>
  </DialogPortal>
</template>
