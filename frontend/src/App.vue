<script setup>
import { computed, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import {
  CalendarDays,
  Check,
  Coins,
  Download,
  Images,
  LineChart as LineChartIcon,
  LogOut,
  Settings,
  Smartphone,
  User,
} from 'lucide-vue-next'
import { cn } from '@/lib/utils'
import { api } from '@/lib/api'
import { applyDayAccent } from '@/lib/status'
import { useAuth } from '@/composables/useAuth'
import { Button } from '@/components/ui/button'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from '@/components/ui/sheet'
import { Label } from '@/components/ui/label'
import BrandMark from '@/components/BrandMark.vue'

// --- Version ---
const version = __PICWEIGHT_VERSION__

// --- Auth ---
const { claims, displayName, logout } = useAuth()
const initial = computed(() => (displayName.value || '?').charAt(0))

// --- Router ---
const route = useRoute()
const currentView = computed(() => route.meta.view || 'today')
const showChrome = computed(() => route.meta.chrome !== false)

const NAV = [
  { view: 'today', to: { name: 'today' }, label: 'Today', icon: CalendarDays },
  { view: 'history', to: { name: 'history' }, label: 'History', icon: Images },
  { view: 'trends', to: { name: 'trends' }, label: 'Trends', icon: LineChartIcon },
  { view: 'usage', to: { name: 'usage' }, label: 'Usage', icon: Coins },
  { view: 'profile', to: { name: 'profile' }, label: 'Profile', icon: User },
]

// --- Android App ---
const apkAvailable = ref(false)

async function checkApkAvailable() {
  try {
    const res = await fetch('/picweight.apk', { method: 'HEAD' })
    // Missing static files fall back to index.html, so a 200 alone isn't enough.
    const type = res.headers.get('content-type') || ''
    apkAvailable.value = res.ok && !type.includes('text/html')
  } catch {
    apkAvailable.value = false
  }
}

/**
 * The accent tracks today's verdict everywhere, not only on the day view — one
 * request on load so a cold landing on /trends or /profile is not neutral grey.
 * The day view refreshes it whenever its own figures change.
 */
async function primeDayAccent() {
  try {
    const me = await api.me()
    applyDayAccent(me.today?.status)
  } catch {
    // Leave the neutral default; the day view will set it when it loads.
  }
}

// --- Init ---
onMounted(() => {
  checkApkAvailable()
  primeDayAccent()
})
</script>

<template>
  <router-view v-if="!showChrome" />

  <div v-else class="flex min-h-screen flex-col bg-background text-foreground">
    <header class="sticky top-0 z-40 border-b border-border bg-background/85 backdrop-blur-xl">
      <div class="mx-auto flex h-16 max-w-6xl items-center justify-between gap-4 px-4 sm:px-6">
        <div class="flex items-center gap-6">
          <router-link
            :to="{ name: 'today' }"
            class="flex items-center gap-2.5 rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <BrandMark class="size-8 text-day" />
            <span class="text-lg font-semibold tracking-tight text-ink">picweight</span>
          </router-link>

          <nav class="hidden items-center gap-1 md:flex">
            <router-link
              v-for="item in NAV"
              :key="item.view"
              :to="item.to"
              custom
              v-slot="{ navigate }"
            >
              <Button
                variant="ghost"
                size="sm"
                :class="cn(
                  'gap-2 px-3',
                  currentView === item.view ? 'bg-white/[0.06] text-ink' : 'text-ink-3',
                )"
                @click="navigate"
              >
                <component :is="item.icon" />
                {{ item.label }}
              </Button>
            </router-link>
          </nav>
        </div>

        <div class="flex items-center gap-2">
          <!-- Settings -->
          <Sheet>
            <SheetTrigger as-child>
              <Button variant="ghost" size="icon" aria-label="Settings">
                <Settings />
              </Button>
            </SheetTrigger>
            <SheetContent>
              <SheetHeader>
                <SheetTitle>Settings</SheetTitle>
                <SheetDescription>This install, and the app that feeds it.</SheetDescription>
              </SheetHeader>

              <div class="space-y-6">
                <!-- Android App -->
                <div class="space-y-3">
                  <div class="flex items-center gap-2">
                    <Smartphone class="size-4 text-ink-3" aria-hidden="true" />
                    <Label class="text-sm font-medium">Android app</Label>
                    <span
                      v-if="apkAvailable"
                      class="num ml-auto rounded border border-good/40 bg-good/10 px-2 py-0.5 text-[10px] font-bold text-good"
                    >{{ version }}</span>
                  </div>
                  <p class="text-xs leading-relaxed text-ink-3">
                    Capture happens on the phone: point the camera at the plate and the estimate
                    lands here about half a minute later.
                  </p>
                  <Button v-if="apkAvailable" as-child size="sm">
                    <a href="/picweight.apk" download>
                      <Download /> Download APK
                    </a>
                  </Button>
                  <p v-else class="text-xs text-ink-3">
                    The APK is not bundled with this build.
                  </p>
                </div>

                <div class="tick-rule" aria-hidden="true" />

                <!-- Session -->
                <div class="space-y-3">
                  <Label class="text-sm font-medium">Signed in</Label>
                  <dl class="space-y-2 text-sm">
                    <div class="flex justify-between gap-3">
                      <dt class="text-ink-3">Name</dt>
                      <dd class="truncate text-ink-2">{{ claims?.name || '—' }}</dd>
                    </div>
                    <div class="flex justify-between gap-3">
                      <dt class="text-ink-3">Email</dt>
                      <dd class="truncate text-ink-2">{{ claims?.email || '—' }}</dd>
                    </div>
                    <div class="flex justify-between gap-3">
                      <dt class="text-ink-3">Issuer</dt>
                      <dd class="num truncate text-ink-2">{{ claims?.iss || '—' }}</dd>
                    </div>
                  </dl>
                  <Button variant="outline" size="sm" @click="logout">
                    <LogOut /> Sign out
                  </Button>
                </div>

                <div class="tick-rule" aria-hidden="true" />

                <div class="space-y-2">
                  <Label class="text-sm font-medium">Backups</Label>
                  <p class="text-xs leading-relaxed text-ink-3">
                    picweight keeps its database and thumbnails on one volume and stays out of the
                    backup business. Point rclone at the volume, or take the export from the
                    profile page.
                  </p>
                  <Button as-child variant="ghost" size="sm">
                    <router-link :to="{ name: 'profile' }">
                      <Check /> Go to export
                    </router-link>
                  </Button>
                </div>
              </div>
            </SheetContent>
          </Sheet>

          <!-- Identity -->
          <div v-if="claims" class="flex items-center gap-2">
            <span
              class="flex size-7 shrink-0 items-center justify-center rounded-full border border-border bg-white/5 text-xs font-bold uppercase text-ink-2"
              :title="displayName"
            >{{ initial }}</span>
            <span class="hidden max-w-32 truncate text-sm text-ink-3 sm:inline">{{ displayName }}</span>
          </div>
        </div>
      </div>
    </header>

    <main class="mx-auto w-full max-w-6xl flex-1 px-4 pb-28 pt-6 sm:px-6 md:pb-12">
      <router-view />
    </main>

    <footer class="border-t border-border py-6 text-center">
      <p class="eyebrow">picweight {{ version }}</p>
    </footer>

    <!-- Mobile navigation -->
    <nav
      class="fixed inset-x-4 bottom-4 z-40 flex items-center justify-around rounded-xl border border-border bg-popover/95 p-1.5 shadow-2xl shadow-black/60 backdrop-blur-xl md:hidden"
    >
      <router-link
        v-for="item in NAV"
        :key="item.view"
        :to="item.to"
        custom
        v-slot="{ navigate }"
      >
        <Button
          variant="ghost"
          size="icon"
          :aria-label="item.label"
          :class="cn(currentView === item.view ? 'bg-white/[0.06] text-day' : 'text-ink-3')"
          @click="navigate"
        >
          <component :is="item.icon" />
        </Button>
      </router-link>
    </nav>
  </div>
</template>
