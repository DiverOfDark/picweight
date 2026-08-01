<script setup>
/**
 * Body data in, targets out.
 *
 * The numbers on this page are a formula — Mifflin-St Jeor, an activity factor
 * and a goal delta — not a model's opinion. That is why the form asks for every
 * field: the arithmetic needs all of them, and it is done server-side so the
 * phone and the browser can never disagree about your target.
 */
import { computed, onMounted, ref } from 'vue'
import { Check, Download, FileJson, FileSpreadsheet, TriangleAlert } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { grams, kcal, stamp } from '@/lib/format'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

const ACTIVITY = [
  { value: 1.2, label: 'Sedentary — desk work, little exercise' },
  { value: 1.375, label: 'Lightly active — 1–3 sessions a week' },
  { value: 1.55, label: 'Moderately active — 3–5 sessions a week' },
  { value: 1.725, label: 'Very active — 6–7 sessions a week' },
  { value: 1.9, label: 'Extremely active — physical job or two-a-days' },
]

const GOALS = [
  { value: 'Lose', label: 'Lose weight' },
  { value: 'Maintain', label: 'Maintain' },
  { value: 'Gain', label: 'Gain weight' },
]

const me = ref(null)
const loading = ref(true)
const saving = ref(false)
const error = ref('')
const warnings = ref([])
const saved = ref(false)

const form = ref({
  sex: 'Male',
  birth_date: '1990-01-01',
  height_cm: 180,
  activity_factor: 1.375,
  goal_type: 'Lose',
  target_weight_kg: 75,
  rate_kg_per_week: 0.5,
  current_weight_kg: '',
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
})

const profile = computed(() => me.value?.profile ?? null)
const onboarded = computed(() => !!profile.value)

async function load() {
  loading.value = true
  error.value = ''
  try {
    me.value = await api.me()
    if (me.value.profile) {
      const p = me.value.profile
      form.value = {
        sex: p.sex,
        birth_date: p.birth_date,
        height_cm: p.height_cm,
        activity_factor: p.activity_factor,
        goal_type: p.goal_type,
        target_weight_kg: p.target_weight_kg ?? 75,
        rate_kg_per_week: p.rate_kg_per_week ?? 0.5,
        current_weight_kg: p.current_weight_kg ?? '',
        timezone: p.timezone,
      }
    }
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

async function save() {
  saving.value = true
  error.value = ''
  warnings.value = []
  saved.value = false
  try {
    const payload = {
      sex: form.value.sex,
      birth_date: form.value.birth_date,
      height_cm: Number(form.value.height_cm),
      activity_factor: Number(form.value.activity_factor),
      goal_type: form.value.goal_type,
      target_weight_kg: Number(form.value.target_weight_kg),
      rate_kg_per_week: Number(form.value.rate_kg_per_week),
      timezone: form.value.timezone,
    }
    if (form.value.current_weight_kg !== '' && form.value.current_weight_kg !== null) {
      payload.current_weight_kg = Number(form.value.current_weight_kg)
    }
    const result = await api.updateProfile(payload)
    warnings.value = result.warnings ?? []
    saved.value = true
    await load()
  } catch (e) {
    error.value = e.message
  } finally {
    saving.value = false
  }
}

onMounted(load)

</script>

<template>
  <div class="space-y-6">
    <header>
      <p class="eyebrow">Body data and targets</p>
      <h1 class="mt-1.5 text-2xl font-semibold tracking-tight text-ink">Profile</h1>
    </header>

    <div class="tick-rule" aria-hidden="true" />

    <p v-if="error" class="rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-sm text-critical">
      {{ error }}
    </p>
    <p v-if="loading" class="text-sm text-ink-3">Loading your profile…</p>

    <div class="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(0,22rem)]">
      <!-- The form -->
      <Card>
        <CardHeader>
          <CardTitle>{{ onboarded ? 'Edit your body data' : 'Set up your targets' }}</CardTitle>
          <CardDescription>
            Targets recompute every time this changes, and again whenever you log a weight.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form class="grid gap-4 sm:grid-cols-2" @submit.prevent="save">
            <div class="space-y-1.5">
              <Label for="sex">Sex</Label>
              <select
                id="sex"
                v-model="form.sex"
                class="h-10 w-full rounded-lg border border-input bg-black/25 px-3 text-sm text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <option value="Male">Male</option>
                <option value="Female">Female</option>
              </select>
              <p class="text-xs text-ink-3">Changes the BMR constant by 166 kcal.</p>
            </div>

            <div class="space-y-1.5">
              <Label for="birth-date">Date of birth</Label>
              <Input id="birth-date" v-model="form.birth_date" numeric type="date" />
            </div>

            <div class="space-y-1.5">
              <Label for="height">Height (cm)</Label>
              <Input id="height" v-model="form.height_cm" numeric type="number" step="0.5" min="80" max="250" />
            </div>

            <div class="space-y-1.5">
              <Label for="current-weight">Current weight (kg)</Label>
              <Input
                id="current-weight"
                v-model="form.current_weight_kg"
                numeric
                type="number"
                step="0.1"
                min="20"
                max="400"
                placeholder="Leave blank to keep the last reading"
              />
            </div>

            <div class="space-y-1.5 sm:col-span-2">
              <Label for="activity">Activity level</Label>
              <select
                id="activity"
                v-model.number="form.activity_factor"
                class="h-10 w-full rounded-lg border border-input bg-black/25 px-3 text-sm text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <option v-for="option in ACTIVITY" :key="option.value" :value="option.value">
                  {{ option.label }}
                </option>
              </select>
            </div>

            <div class="space-y-1.5">
              <Label for="goal">Goal</Label>
              <select
                id="goal"
                v-model="form.goal_type"
                class="h-10 w-full rounded-lg border border-input bg-black/25 px-3 text-sm text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <option v-for="option in GOALS" :key="option.value" :value="option.value">
                  {{ option.label }}
                </option>
              </select>
            </div>

            <div class="space-y-1.5">
              <Label for="target-weight">Target weight (kg)</Label>
              <Input id="target-weight" v-model="form.target_weight_kg" numeric type="number" step="0.1" min="20" max="400" />
            </div>

            <div class="space-y-1.5">
              <Label for="rate">Rate (kg per week)</Label>
              <Input id="rate" v-model="form.rate_kg_per_week" numeric type="number" step="0.05" min="0" max="2" />
              <p class="text-xs text-ink-3">Always positive; the goal sets the direction.</p>
            </div>

            <div class="space-y-1.5">
              <Label for="timezone">Timezone</Label>
              <Input id="timezone" v-model="form.timezone" placeholder="Europe/Moscow" />
              <p class="text-xs text-ink-3">Buckets a meal that arrives without an offset.</p>
            </div>

            <div class="flex items-center gap-3 sm:col-span-2">
              <Button type="submit" :disabled="saving">
                <Check /> {{ saving ? 'Recomputing…' : 'Save and recompute' }}
              </Button>
              <span v-if="saved && !warnings.length" class="text-sm text-good">Targets updated.</span>
            </div>
          </form>

          <ul v-if="warnings.length" class="mt-4 space-y-2">
            <li
              v-for="warning in warnings"
              :key="warning"
              class="flex items-start gap-2 rounded-lg border border-serious/40 bg-serious/10 px-3 py-2 text-sm text-serious"
            >
              <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
              {{ warning }}
            </li>
          </ul>
        </CardContent>
      </Card>

      <div class="space-y-5">
        <!-- What the formula produced -->
        <Card>
          <CardHeader>
            <CardTitle>Your daily targets</CardTitle>
            <CardDescription v-if="profile?.targets_computed_at">
              Computed {{ stamp(profile.targets_computed_at) }}.
            </CardDescription>
            <CardDescription v-else>Save your body data to compute them.</CardDescription>
          </CardHeader>
          <CardContent v-if="profile">
            <dl class="space-y-2.5 text-sm">
              <div class="flex items-baseline justify-between gap-3">
                <dt class="text-ink-3">Energy</dt>
                <dd class="num font-semibold text-ink">{{ kcal(profile.target_kcal ?? 0) }} kcal</dd>
              </div>
              <div class="flex items-baseline justify-between gap-3">
                <dt class="text-ink-3">Protein floor</dt>
                <dd class="num text-ink-2">{{ grams(profile.target_protein_g ?? 0) }} g</dd>
              </div>
              <div class="flex items-baseline justify-between gap-3">
                <dt class="text-ink-3">Fat floor</dt>
                <dd class="num text-ink-2">{{ grams(profile.target_fat_g ?? 0) }} g</dd>
              </div>
              <div class="flex items-baseline justify-between gap-3">
                <dt class="text-ink-3">Carbs</dt>
                <dd class="num text-ink-2">{{ grams(profile.target_carbs_g ?? 0) }} g</dd>
              </div>
              <div class="flex items-baseline justify-between gap-3 border-t border-border pt-2.5">
                <dt class="text-ink-3">Calibration factor</dt>
                <dd class="num text-ink-2">{{ profile.calibration_factor.toFixed(2) }}×</dd>
              </div>
            </dl>
            <p class="mt-3 text-xs leading-relaxed text-ink-3">
              The calibration factor is learned from your corrections and nudges every estimate,
              because hidden cooking fat is invisible to a camera.
            </p>
          </CardContent>
        </Card>

        <!-- Export -->
        <Card>
          <CardHeader>
            <CardTitle>Export</CardTitle>
            <CardDescription>Everything you have logged, in a file you own.</CardDescription>
          </CardHeader>
          <CardContent class="flex flex-wrap gap-2">
            <Button as-child variant="outline" size="sm">
              <a :href="api.exportUrl('json')" download>
                <FileJson /> JSON
              </a>
            </Button>
            <Button as-child variant="outline" size="sm">
              <a :href="api.exportUrl('csv')" download>
                <FileSpreadsheet /> CSV
              </a>
            </Button>
            <Button as-child variant="ghost" size="sm">
              <a :href="api.exportUrl('json')" target="_blank" rel="noopener">
                <Download /> Open JSON
              </a>
            </Button>
          </CardContent>
        </Card>

        <!-- Account -->
        <Card v-if="me">
          <CardHeader>
            <CardTitle>Account</CardTitle>
          </CardHeader>
          <CardContent>
            <dl class="space-y-2 text-sm">
              <div class="flex justify-between gap-3">
                <dt class="text-ink-3">Name</dt>
                <dd class="truncate text-ink-2">{{ me.user.display_name || '—' }}</dd>
              </div>
              <div class="flex justify-between gap-3">
                <dt class="text-ink-3">Email</dt>
                <dd class="truncate text-ink-2">{{ me.user.email || '—' }}</dd>
              </div>
              <div class="flex justify-between gap-3">
                <dt class="text-ink-3">Since</dt>
                <dd class="num text-ink-2">{{ stamp(me.user.created_at) }}</dd>
              </div>
              <div class="flex justify-between gap-3">
                <dt class="text-ink-3">Backend</dt>
                <dd class="num text-ink-2">{{ me.version }}</dd>
              </div>
            </dl>
          </CardContent>
        </Card>
      </div>
    </div>
  </div>
</template>
