<script setup>
/**
 * One meal, and the argument you can have with the machine about it.
 *
 * Three things live here that exist nowhere else: the per-item reasoning notes
 * (why it said 780 kcal), the revision history with the feedback that caused
 * each, and the re-analysis box — which resumes the persisted agent session
 * rather than starting a new one, so "half the rice" is interpreted against the
 * agent's own prior reasoning and costs one short turn, not another full loop.
 */
import { computed, onMounted, ref, watch } from 'vue'
import { MEAL_STATUS, isInFlight } from '@/lib/enums'
import { useRoute, useRouter } from 'vue-router'
import {
  ArrowLeft,
  Check,
  History,
  Layers,
  Pencil,
  Plus,
  Send,
  Sparkles,
  Trash2,
  TriangleAlert,
  Utensils,
  X,
} from 'lucide-vue-next'
import { api } from '@/lib/api'
import { grams, kcal, label, localDateKey, localTime, percent, stamp, sumTotals } from '@/lib/format'
import { useMealEvents } from '@/composables/useMealEvents'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import StatusPill from '@/components/StatusPill.vue'

const route = useRoute()
const router = useRouter()

const meal = ref(null)
const revisions = ref([])
const loading = ref(true)
const error = ref('')
const notice = ref('')

const mealId = computed(() => route.params.id)
const thumbnail = computed(() => (meal.value?.thumbnail_url ? api.thumbnailUrl(meal.value.id) : null))
const analysing = computed(() => isInFlight(meal.value?.status))

async function load() {
  loading.value = true
  error.value = ''
  try {
    meal.value = await api.meal(mealId.value)
    revisions.value = (await api.revisions(mealId.value)).revisions ?? []
    if (!editing.value) resetDraft()
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

// --- Confirm / delete -------------------------------------------------------

const saving = ref(false)

async function confirm() {
  saving.value = true
  error.value = ''
  try {
    meal.value = await api.patchMeal(mealId.value, { status: MEAL_STATUS.CONFIRMED })
    notice.value = 'Confirmed. This dish now feeds recall, so the next order starts from these numbers.'
  } catch (e) {
    error.value = e.message
  } finally {
    saving.value = false
  }
}

const confirmingDelete = ref(false)

async function remove() {
  saving.value = true
  try {
    await api.deleteMeal(mealId.value)
    router.push({ name: 'today' })
  } catch (e) {
    confirmingDelete.value = false
    error.value = e.message
    saving.value = false
  }
}

// --- Manual item editing ----------------------------------------------------

const editing = ref(false)
const draft = ref([])
const draftScale = ref('1')
const draftName = ref('')

function resetDraft() {
  draft.value = (meal.value?.items ?? []).map((item) => ({
    id: item.id,
    name: item.name,
    grams: String(Math.round(item.grams)),
    kcal: String(Math.round(item.kcal)),
    protein_g: String(Math.round(item.protein_g)),
    fat_g: String(Math.round(item.fat_g)),
    carbs_g: String(Math.round(item.carbs_g)),
    barcode: item.barcode ?? null,
  }))
  draftScale.value = String(meal.value?.portion_scale ?? 1)
  draftName.value = meal.value?.dish_name ?? ''
}

function startEditing() {
  resetDraft()
  editing.value = true
}

function addItem() {
  draft.value.push({ name: '', grams: '0', kcal: '0', protein_g: '0', fat_g: '0', carbs_g: '0' })
}

const number = (value) => {
  const parsed = Number.parseFloat(value)
  return Number.isFinite(parsed) ? parsed : 0
}

/**
 * Rescale an item's macros with its grams. Typing "half the rice" as a gram
 * figure should not silently leave the kcal at the old portion.
 */
function scaleItem(item, nextGrams) {
  const before = number(item.grams)
  const after = number(nextGrams)
  item.grams = nextGrams
  if (before <= 0 || after <= 0) return
  const ratio = after / before
  for (const field of ['kcal', 'protein_g', 'fat_g', 'carbs_g']) {
    item[field] = String(Math.round(number(item[field]) * ratio))
  }
}

const draftTotals = computed(() =>
  sumTotals(
    draft.value.map((item) => ({
      kcal: number(item.kcal),
      protein_g: number(item.protein_g),
      fat_g: number(item.fat_g),
      carbs_g: number(item.carbs_g),
    })),
  ),
)

async function saveItems() {
  saving.value = true
  error.value = ''
  try {
    meal.value = await api.patchMeal(mealId.value, {
      dish_name: draftName.value.trim() || null,
      portion_scale: number(draftScale.value) || 1,
      items: draft.value
        .filter((item) => item.name.trim())
        .map((item) => ({
          id: item.id ?? null,
          name: item.name.trim(),
          grams: number(item.grams),
          kcal: number(item.kcal),
          protein_g: number(item.protein_g),
          fat_g: number(item.fat_g),
          carbs_g: number(item.carbs_g),
          barcode: item.barcode ?? null,
        })),
    })
    editing.value = false
    notice.value = 'Saved. Every change was recorded as a correction.'
    revisions.value = (await api.revisions(mealId.value)).revisions ?? []
  } catch (e) {
    error.value = e.message
  } finally {
    saving.value = false
  }
}

// --- Correction by conversation --------------------------------------------

const feedback = ref('')
const sending = ref(false)

const SUGGESTIONS = [
  'Too much rice, it was about half that',
  'This was the small portion',
  'No sour cream',
]

async function sendFeedback() {
  const text = feedback.value.trim()
  if (!text || sending.value) return
  sending.value = true
  error.value = ''
  try {
    const accepted = await api.reanalyze(mealId.value, text)
    feedback.value = ''
    notice.value = `Sent. The agent is picking up revision ${accepted.revision} from where it left off.`
    await load()
  } catch (e) {
    error.value = e.message
  } finally {
    sending.value = false
  }
}

// --- Live updates -----------------------------------------------------------

useMealEvents((event) => {
  if (event.meal_id === mealId.value) load()
})

watch(mealId, load)
onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <Button variant="ghost" size="sm" class="-ml-2" @click="router.back()">
      <ArrowLeft /> Back
    </Button>

    <p v-if="error" class="rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-sm text-critical">
      {{ error }}
    </p>
    <p v-if="notice" class="rounded-lg border border-good/40 bg-good/10 px-3 py-2 text-sm text-good">
      {{ notice }}
    </p>

    <p v-if="loading && !meal" class="text-sm text-ink-3">Loading the meal…</p>

    <template v-if="meal">
      <header class="flex flex-wrap items-start justify-between gap-4">
        <div class="min-w-0">
          <p class="eyebrow">{{ label(meal.name_source) }} · revision {{ meal.revision }}</p>
          <h1 class="mt-1.5 truncate text-2xl font-semibold tracking-tight text-ink">
            {{ meal.dish_name || 'Unnamed meal' }}
          </h1>
          <div class="mt-2 flex flex-wrap items-center gap-2 text-xs text-ink-3">
            <StatusPill :meal="meal.status" />
            <span class="num">{{ localTime(meal.eaten_at, meal.timezone_offset) }}</span>
            <router-link
              v-if="meal.group_id"
              :to="{ name: 'day', params: { date: localDateKey(meal.eaten_at, meal.timezone_offset) } }"
              class="inline-flex items-center gap-1 hover:text-ink-2"
            >
              <Layers class="size-3" aria-hidden="true" /> part of a sitting
            </router-link>
          </div>
        </div>

        <div class="flex shrink-0 flex-wrap gap-2">
          <Button v-if="meal.status !== MEAL_STATUS.CONFIRMED" :disabled="saving || analysing" @click="confirm">
            <Check /> Confirm
          </Button>
          <Button v-if="!editing" variant="outline" :disabled="analysing" @click="startEditing">
            <Pencil /> Edit items
          </Button>
          <Button
            variant="destructive"
            size="icon"
            aria-label="Delete meal"
            :disabled="saving"
            @click="confirmingDelete = true"
          >
            <Trash2 />
          </Button>
        </div>
      </header>

      <div class="tick-rule" aria-hidden="true" />

      <p
        v-if="meal.error"
        class="flex items-start gap-2 rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-sm text-critical"
      >
        <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        {{ meal.error }}
      </p>

      <div class="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-[minmax(0,20rem)_minmax(0,1fr)]">
        <!-- The evidence -->
        <div class="space-y-4">
          <div class="overflow-hidden rounded-xl border border-border bg-black/30">
            <img
              v-if="thumbnail"
              :src="thumbnail"
              :alt="`Photo of ${meal.dish_name || 'the meal'}`"
              class="aspect-square w-full object-cover"
            >
            <div v-else class="flex aspect-square w-full flex-col items-center justify-center gap-2 text-ink-3">
              <Utensils class="size-6" aria-hidden="true" />
              <p class="text-xs">Logged without a photo</p>
            </div>
          </div>

          <dl class="space-y-2 text-sm">
            <div class="flex justify-between gap-3">
              <dt class="text-ink-3">Total</dt>
              <dd class="num font-semibold text-ink">{{ kcal(meal.totals.kcal) }} kcal</dd>
            </div>
            <div class="flex justify-between gap-3">
              <dt class="text-ink-3">Protein</dt>
              <dd class="num text-ink-2">{{ grams(meal.totals.protein_g) }} g</dd>
            </div>
            <div class="flex justify-between gap-3">
              <dt class="text-ink-3">Fat</dt>
              <dd class="num text-ink-2">{{ grams(meal.totals.fat_g) }} g</dd>
            </div>
            <div class="flex justify-between gap-3">
              <dt class="text-ink-3">Carbs</dt>
              <dd class="num text-ink-2">{{ grams(meal.totals.carbs_g) }} g</dd>
            </div>
            <div v-if="meal.portion_scale !== 1" class="flex justify-between gap-3">
              <dt class="text-ink-3">Portion eaten</dt>
              <dd class="num text-ink-2">{{ Math.round(meal.portion_scale * 100) }}%</dd>
            </div>
          </dl>

          <div v-if="meal.user_comment" class="rounded-lg border border-border bg-black/20 p-3">
            <p class="eyebrow">Your comment</p>
            <p class="mt-1.5 text-sm text-ink-2">{{ meal.user_comment }}</p>
          </div>
        </div>

        <!-- Items -->
        <div class="space-y-4">
          <section v-if="!editing" class="space-y-1">
            <h2 class="eyebrow mb-2">Items</h2>

            <article
              v-for="item in meal.items"
              :key="item.id"
              class="rounded-lg border border-border bg-black/20 p-3"
            >
              <div class="flex items-baseline justify-between gap-3">
                <p class="text-sm font-medium text-ink">{{ item.name }}</p>
                <p class="num shrink-0 text-sm text-ink">{{ kcal(item.kcal) }} kcal</p>
              </div>

              <div class="num mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-xs text-ink-3">
                <span>{{ grams(item.grams) }} g</span>
                <span><span :style="{ color: 'var(--color-protein)' }">P</span> {{ grams(item.protein_g) }}</span>
                <span><span :style="{ color: 'var(--color-fat)' }">F</span> {{ grams(item.fat_g) }}</span>
                <span><span :style="{ color: 'var(--color-carbs)' }">C</span> {{ grams(item.carbs_g) }}</span>
                <span v-if="item.barcode">EAN {{ item.barcode }}</span>
              </div>

              <p v-if="item.reasoning_note" class="mt-2 border-l-2 border-rule pl-2.5 text-xs leading-relaxed text-ink-2">
                {{ item.reasoning_note }}
              </p>

              <div class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-ink-3">
                <span>grams: {{ label(item.grams_source) }}</span>
                <span>macros: {{ label(item.macro_source) }}</span>
                <span v-if="item.confidence !== null">confidence {{ percent(item.confidence) }}</span>
              </div>
            </article>

            <p v-if="!meal.items.length" class="text-sm text-ink-3">
              {{ analysing ? 'The agent is still working on this one.' : 'No items on this revision.' }}
            </p>
          </section>

          <!-- Editing -->
          <section v-else class="space-y-3">
            <h2 class="eyebrow">Edit items</h2>

            <div class="grid gap-3 sm:grid-cols-2">
              <div class="space-y-1.5">
                <Label for="dish-name">Dish name</Label>
                <Input id="dish-name" v-model="draftName" placeholder="Шаурма с курицей" />
              </div>
              <div class="space-y-1.5">
                <Label for="portion-scale">Portion eaten (1 = all of it)</Label>
                <Input id="portion-scale" v-model="draftScale" numeric type="number" step="0.05" min="0.05" max="2" />
              </div>
            </div>

            <div
              v-for="(item, index) in draft"
              :key="item.id ?? `new-${index}`"
              class="space-y-2 rounded-lg border border-border bg-black/20 p-3"
            >
              <div class="flex items-center gap-2">
                <Input v-model="item.name" placeholder="Component" class="h-8 flex-1 text-sm" />
                <Button
                  variant="ghost"
                  size="icon-sm"
                  :aria-label="`Remove ${item.name || 'item'}`"
                  @click="draft.splice(index, 1)"
                >
                  <X />
                </Button>
              </div>
              <div class="grid grid-cols-2 gap-2 sm:grid-cols-5">
                <label class="space-y-1">
                  <span class="eyebrow">grams</span>
                  <Input
                    :model-value="item.grams"
                    numeric
                    type="number"
                    class="h-8"
                    @update:model-value="scaleItem(item, $event)"
                  />
                </label>
                <label class="space-y-1">
                  <span class="eyebrow">kcal</span>
                  <Input v-model="item.kcal" numeric type="number" class="h-8" />
                </label>
                <label class="space-y-1">
                  <span class="eyebrow">protein</span>
                  <Input v-model="item.protein_g" numeric type="number" class="h-8" />
                </label>
                <label class="space-y-1">
                  <span class="eyebrow">fat</span>
                  <Input v-model="item.fat_g" numeric type="number" class="h-8" />
                </label>
                <label class="space-y-1">
                  <span class="eyebrow">carbs</span>
                  <Input v-model="item.carbs_g" numeric type="number" class="h-8" />
                </label>
              </div>
            </div>

            <Button variant="outline" size="sm" @click="addItem">
              <Plus /> Add an item
            </Button>

            <div class="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3">
              <p class="num text-sm text-ink-2">
                {{ kcal(draftTotals.kcal) }} kcal ·
                P {{ grams(draftTotals.protein_g) }} ·
                F {{ grams(draftTotals.fat_g) }} ·
                C {{ grams(draftTotals.carbs_g) }}
              </p>
              <div class="flex gap-2">
                <Button variant="ghost" @click="editing = false">Cancel</Button>
                <Button :disabled="saving" @click="saveItems">
                  <Check /> Save changes
                </Button>
              </div>
            </div>
          </section>

          <!-- Correction by conversation -->
          <Card>
            <CardHeader class="pb-3">
              <CardTitle class="flex items-center gap-2 text-sm">
                <Sparkles class="size-4 text-ink-3" aria-hidden="true" />
                Tell it what's wrong
              </CardTitle>
            </CardHeader>
            <CardContent class="space-y-3">
              <p class="text-xs leading-relaxed text-ink-3">
                The agent still has this meal's whole thread — the container it identified, what
                your history returned, what it ruled out. It picks up from there instead of
                starting over.
              </p>

              <Textarea
                v-model="feedback"
                rows="3"
                placeholder="Too much rice, it was about half that"
                :disabled="sending || analysing"
                @keydown.ctrl.enter="sendFeedback"
              />

              <div class="flex flex-wrap gap-1.5">
                <button
                  v-for="suggestion in SUGGESTIONS"
                  :key="suggestion"
                  type="button"
                  class="rounded-full border border-border px-2.5 py-1 text-[11px] text-ink-3 transition-colors hover:border-rule hover:text-ink-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  @click="feedback = suggestion"
                >
                  {{ suggestion }}
                </button>
              </div>

              <div class="flex items-center justify-between gap-3">
                <p class="text-[11px] text-ink-3">Ctrl + Enter sends</p>
                <Button :disabled="!feedback.trim() || sending || analysing" @click="sendFeedback">
                  <Send /> {{ sending ? 'Sending…' : 'Re-analyse' }}
                </Button>
              </div>
            </CardContent>
          </Card>

          <!-- Revision history -->
          <section v-if="revisions.length" class="space-y-2">
            <h2 class="eyebrow flex items-center gap-1.5">
              <History class="size-3" aria-hidden="true" /> Revision history
            </h2>

            <article
              v-for="revision in revisions"
              :key="revision.revision"
              class="rounded-lg border border-border bg-black/20 p-3"
              :class="revision.revision === meal.revision && 'border-rule'"
            >
              <div class="flex flex-wrap items-baseline justify-between gap-2">
                <p class="text-sm font-medium text-ink">
                  Revision {{ revision.revision }}
                  <span v-if="revision.revision === meal.revision" class="text-xs font-normal text-ink-3">
                    · current
                  </span>
                </p>
                <p class="num text-sm text-ink-2">{{ kcal(revision.totals.kcal) }} kcal</p>
              </div>

              <p v-if="revision.user_feedback" class="mt-2 border-l-2 border-rule pl-2.5 text-sm italic text-ink-2">
                “{{ revision.user_feedback }}”
              </p>

              <div class="num mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-ink-3">
                <span>{{ stamp(revision.created_at) }}</span>
                <span>{{ revision.model }}</span>
                <span>{{ revision.tool_calls }} tool {{ revision.tool_calls === 1 ? 'call' : 'calls' }}</span>
                <span v-if="revision.reseeded" class="font-sans">reseeded, not continued</span>
              </div>

              <ul class="num mt-2 space-y-0.5 text-xs text-ink-3">
                <li v-for="item in revision.items" :key="item.id" class="flex justify-between gap-3">
                  <span class="truncate font-sans text-ink-2">{{ item.name }}</span>
                  <span class="shrink-0">{{ grams(item.grams) }} g · {{ kcal(item.kcal) }} kcal</span>
                </li>
              </ul>
            </article>
          </section>
        </div>
      </div>

      <Dialog v-model:open="confirmingDelete">
        <DialogContent class="max-w-sm">
          <DialogHeader>
            <DialogTitle>Delete this meal?</DialogTitle>
            <DialogDescription>
              “{{ meal.dish_name || 'Unnamed meal' }}” goes, along with its items, its agent
              session and every revision. The day's totals drop by
              {{ kcal(meal.totals.kcal) }} kcal.
            </DialogDescription>
          </DialogHeader>
          <div class="flex justify-end gap-2">
            <DialogClose as-child>
              <Button variant="ghost">Keep it</Button>
            </DialogClose>
            <Button variant="destructive" :disabled="saving" @click="remove">
              <Trash2 /> Delete
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </template>
  </div>
</template>
