<script setup>
/** An empty screen is an invitation to act, so it always says what to do next. */
defineProps({
  title: { type: String, required: true },
  hint: { type: String, default: '' },
})
</script>

<template>
  <div class="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border px-6 py-10 text-center">
    <p class="text-sm font-medium text-ink-2">{{ title }}</p>
    <p v-if="hint" class="max-w-sm text-xs text-ink-3">{{ hint }}</p>
    <slot />
  </div>
</template>
