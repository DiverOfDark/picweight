<script setup>
/**
 * A failed meal's reason, and the one thing that can be done about it.
 *
 * PRD §5 makes an exhausted quota loud rather than silent, which was only half
 * the job: the user could see why and still do nothing, because the only way
 * out was to delete the meal and re-photograph food that had since been eaten.
 * The photo is not lost — the upload succeeded and the 768px thumbnail is on
 * the server — so a retry re-runs the agent at the *same revision* against data
 * the server already has. Nothing is asked of the phone.
 *
 * The reason travels with the button on purpose, everywhere the button appears.
 * "You exceeded your current quota" is worth another tap; "the image could not
 * be decoded" never will be, and only the reason tells those two apart.
 */
import { ref, watch } from 'vue'
import { RefreshCw, TriangleAlert } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'

const props = defineProps({
  /** A meal whose `status` is `failed`. */
  meal: { type: Object, required: true },
  /** Tighter type and padding, for a ledger row or a history card. */
  compact: { type: Boolean, default: false },
  class: { type: null, default: '' },
})

/** Emitted with the `MealAcceptedResponse`, so the parent can update at once. */
const emit = defineEmits(['retried'])

const retrying = ref(false)
const queued = ref(false)
const failure = ref('')

/**
 * Re-enqueue the analysis.
 *
 * Guarded twice — `disabled` on the button and this early return — because the
 * person most likely to tap Retry twice is the one who just watched it fail,
 * and a second tap that slipped through would mean a second agent loop against
 * the same quota that just ran out.
 */
async function retry() {
  if (retrying.value || queued.value) return
  retrying.value = true
  failure.value = ''
  try {
    const accepted = await api.retryMeal(props.meal.id)
    queued.value = true
    emit('retried', accepted)
  } catch (e) {
    // A 409 lands here too — already queued, or no longer failed. The API
    // phrases both, so show its sentence rather than inventing a worse one.
    failure.value = e.message
  } finally {
    retrying.value = false
  }
}

// Rows are recycled as lists reload; a fresh meal must not inherit the last
// one's "queued again" state.
watch(
  () => props.meal.id,
  () => {
    queued.value = false
    failure.value = ''
  },
)
</script>

<template>
  <div
    aria-live="polite"
    :class="cn(
      'rounded-lg border',
      queued ? 'border-border bg-white/5' : 'border-critical/40 bg-critical/10',
      compact ? 'px-2 py-1.5' : 'px-3 py-2',
      props.class,
    )"
  >
    <div class="flex flex-wrap items-start justify-between gap-x-3 gap-y-1.5">
      <p
        v-if="queued"
        :class="cn('flex min-w-0 items-start gap-2 text-ink-2', compact ? 'text-[11px]' : 'text-sm')"
      >
        <RefreshCw
          :class="cn('mt-0.5 shrink-0 animate-spin', compact ? 'size-3' : 'size-4')"
          aria-hidden="true"
        />
        <span class="min-w-0">Queued again — the agent is re-reading the stored photo.</span>
      </p>

      <template v-else>
        <p
          :class="cn('flex min-w-0 items-start gap-2 text-critical', compact ? 'text-[11px]' : 'text-sm')"
        >
          <TriangleAlert
            :class="cn('mt-0.5 shrink-0', compact ? 'size-3' : 'size-4')"
            aria-hidden="true"
          />
          <span class="min-w-0 break-words">{{ meal.error || 'The analysis failed.' }}</span>
        </p>

        <Button
          variant="outline"
          :size="compact ? 'xs' : 'sm'"
          :disabled="retrying"
          :aria-label="`Retry the analysis of ${meal.dish_name || 'this meal'}`"
          @click="retry"
        >
          <RefreshCw :class="retrying && 'animate-spin'" aria-hidden="true" />
          {{ retrying ? 'Retrying…' : 'Retry' }}
        </Button>
      </template>
    </div>

    <p
      v-if="failure"
      :class="cn('mt-1.5 text-critical', compact ? 'text-[11px]' : 'text-xs')"
    >
      {{ failure }}
    </p>
  </div>
</template>
