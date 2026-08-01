<script setup>
/**
 * What the agent has cost you.
 *
 * Every analysis already recorded its model, tokens and wall clock on
 * `analysis_jobs`; until now nothing read them back. Two things about this
 * screen are deliberate:
 *
 * 1. **Cost is an estimate, and says so.** The server recomputes spend from
 *    token counts at the rates configured *now*, rather than summing the value
 *    frozen on each row. So correcting `PICWEIGHT_MODEL_PRICING` also corrects
 *    the history instead of leaving a total that blends two pricing regimes.
 * 2. **Provenance is shown, not hidden.** A figure priced from a rate you
 *    supplied is worth acting on; one priced from the built-in fallback because
 *    nothing matched is a guess with a dollar sign attached. Rendering those
 *    identically is how a number becomes trusted before it has earned it, so
 *    unpriced models are called out rather than quietly averaged in.
 */
import { computed, onMounted, ref } from 'vue'
import { Coins, TriangleAlert } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { PRICING_SOURCE } from '@/lib/generated/enums'
import { dayLabel, kcal as thousands, shiftDateKey, todayKey } from '@/lib/format'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import EmptyState from '@/components/EmptyState.vue'
import LineChart from '@/components/charts/LineChart.vue'

const RANGES = [
  { days: 7, label: '7 days' },
  { days: 30, label: '30 days' },
  { days: 90, label: '90 days' },
]

const range = ref(30)
const usage = ref(null)
const loading = ref(true)
const error = ref('')

async function load() {
  loading.value = true
  error.value = ''
  try {
    usage.value = await api.usage({
      from: shiftDateKey(todayKey(), -(range.value - 1)),
      to: todayKey(),
    })
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

function setRange(days) {
  range.value = days
  load()
}

/**
 * Micro-USD to a readable amount.
 *
 * Sub-cent totals are the normal case for a single meal, so rounding to two
 * decimals would render most of this screen as `$0.00` and make it look broken.
 * Below a cent it switches to four decimals rather than lying about zero.
 */
function usd(micro) {
  const dollars = (micro ?? 0) / 1_000_000
  if (dollars === 0) return '$0'
  if (Math.abs(dollars) < 0.01) return `$${dollars.toFixed(4)}`
  return `$${dollars.toFixed(2)}`
}

/** Rates are quoted per million tokens, which is how providers quote them. */
const perMillion = (micro) => `$${((micro ?? 0) / 1_000_000).toFixed(2)}`

const tokens = (n) => thousands(n ?? 0)

const totalTokens = computed(
  () => (usage.value?.prompt_tokens ?? 0) + (usage.value?.completion_tokens ?? 0),
)

const costPoints = computed(() =>
  (usage.value?.by_day ?? []).map((day) => ({
    at: `${day.date}T12:00:00Z`,
    value: day.cost_micro_usd / 1_000_000,
    caption: dayLabel(day.date),
  })),
)

const SOURCE_LABEL = {
  [PRICING_SOURCE.CONFIGURED]: 'your rate',
  [PRICING_SOURCE.BUILT_IN]: 'built-in rate',
  [PRICING_SOURCE.FALLBACK]: 'no rate — guessed',
}

const SOURCE_CLASS = {
  [PRICING_SOURCE.CONFIGURED]: 'text-good border-good/40 bg-good/10',
  [PRICING_SOURCE.BUILT_IN]: 'text-ink-3 border-border bg-white/[0.03]',
  [PRICING_SOURCE.FALLBACK]: 'text-serious border-serious/40 bg-serious/10',
}

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div>
        <p class="eyebrow">What the agent has cost</p>
        <h1 class="mt-1.5 text-2xl font-semibold tracking-tight text-ink">AI usage</h1>
      </div>
      <div class="flex gap-1.5">
        <Button
          v-for="option in RANGES"
          :key="option.days"
          size="sm"
          :variant="range === option.days ? 'secondary' : 'ghost'"
          @click="setRange(option.days)"
        >
          {{ option.label }}
        </Button>
      </div>
    </header>

    <div class="tick-rule" aria-hidden="true" />

    <p v-if="error" class="rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-sm text-critical">
      {{ error }}
    </p>
    <p v-if="loading" class="text-sm text-ink-3">Loading usage…</p>

    <template v-if="usage && !loading">
      <!-- The caveat sits above the number it qualifies, not in a footnote
           under it: a total nobody can source should be read as provisional
           before it is read at all. -->
      <p
        v-if="usage.has_estimated_pricing"
        class="flex items-start gap-2 rounded-lg border border-serious/40 bg-serious/10 px-3 py-2 text-sm text-serious"
      >
        <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        <span>
          At least one model below has no configured rate, so its cost is a guess at default
          rates. Set <code class="num text-xs">PICWEIGHT_MODEL_PRICING</code> to what you
          actually pay — the history is repriced, not just new runs.
        </span>
      </p>

      <div class="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <Card>
          <CardHeader class="pb-2"><CardDescription>Estimated spend</CardDescription></CardHeader>
          <CardContent>
            <p class="num text-2xl font-semibold text-ink">{{ usd(usage.cost_micro_usd) }}</p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader class="pb-2"><CardDescription>Per meal</CardDescription></CardHeader>
          <CardContent>
            <p class="num text-2xl font-semibold text-ink">{{ usd(usage.cost_per_meal_micro_usd) }}</p>
            <p class="num mt-0.5 text-xs text-ink-3">{{ usage.meals }} analysed</p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader class="pb-2"><CardDescription>Tokens</CardDescription></CardHeader>
          <CardContent>
            <p class="num text-2xl font-semibold text-ink">{{ tokens(totalTokens) }}</p>
            <p class="num mt-0.5 text-xs text-ink-3">
              {{ tokens(usage.prompt_tokens) }} in · {{ tokens(usage.completion_tokens) }} out
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader class="pb-2"><CardDescription>Agent runs</CardDescription></CardHeader>
          <CardContent>
            <p class="num text-2xl font-semibold text-ink">{{ usage.jobs }}</p>
            <!-- Failures and retries burned tokens without producing an
                 estimate, which is exactly the thing worth noticing here. -->
            <p v-if="usage.failed_jobs || usage.retried_jobs" class="num mt-0.5 text-xs text-ink-3">
              <span v-if="usage.failed_jobs" class="text-critical">{{ usage.failed_jobs }} failed</span>
              <span v-if="usage.failed_jobs && usage.retried_jobs"> · </span>
              <span v-if="usage.retried_jobs">{{ usage.retried_jobs }} retried</span>
            </p>
          </CardContent>
        </Card>
      </div>

      <div class="grid grid-cols-[minmax(0,1fr)] gap-5 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Daily spend</CardTitle>
            <CardDescription>Estimated, in dollars, at current rates.</CardDescription>
          </CardHeader>
          <CardContent>
            <LineChart v-if="costPoints.length > 1" :points="costPoints" unit="$" :precision="4" />
            <EmptyState
              v-else
              title="Not enough days to draw a line"
              hint="Log a few more meals and the trend appears here."
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>By model</CardTitle>
            <CardDescription>Most expensive first, with the rate each figure used.</CardDescription>
          </CardHeader>
          <CardContent>
            <EmptyState
              v-if="!usage.by_model.length"
              title="Nothing analysed in this window"
              hint="Photograph a meal and it shows up here."
            />
            <div v-else class="overflow-x-auto">
              <table class="w-full text-sm">
                <thead>
                  <tr class="border-b border-border text-left text-xs text-ink-3">
                    <th class="pb-2 font-medium">Model</th>
                    <th class="pb-2 text-right font-medium">Runs</th>
                    <th class="pb-2 text-right font-medium">Tokens</th>
                    <th class="pb-2 text-right font-medium">Cost</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="m in usage.by_model" :key="m.model" class="border-b border-border/50 last:border-0">
                    <td class="py-2.5 pr-3">
                      <p class="truncate font-medium text-ink">{{ m.model }}</p>
                      <span
                        class="mt-1 inline-block rounded border px-1.5 py-0.5 text-[11px]"
                        :class="SOURCE_CLASS[m.pricing_source]"
                      >
                        {{ SOURCE_LABEL[m.pricing_source] }}
                        · {{ perMillion(m.input_rate_micro_usd) }}/{{ perMillion(m.output_rate_micro_usd) }}
                        per 1M
                      </span>
                    </td>
                    <td class="num py-2.5 text-right text-ink-2">{{ m.jobs }}</td>
                    <td class="num py-2.5 text-right text-ink-2">
                      {{ tokens(m.prompt_tokens + m.completion_tokens) }}
                    </td>
                    <td class="num py-2.5 text-right font-semibold text-ink">
                      {{ usd(m.cost_micro_usd) }}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </CardContent>
        </Card>
      </div>

      <p class="flex items-start gap-2 text-xs text-ink-3">
        <Coins class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
        <span>
          Token counts are measured and exact. Dollars are derived from them at the rates shown,
          so they are an estimate of what these runs were worth — not a bill. The real spend cap
          lives on the provider side.
        </span>
      </p>
    </template>
  </div>
</template>
