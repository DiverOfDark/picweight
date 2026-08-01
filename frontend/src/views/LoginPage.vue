<script setup>
/**
 * There is no local password store — the backend does authorization-code + PKCE
 * against the confidential web client and hands back an HttpOnly cookie. So the
 * page has exactly one control.
 */
import { onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import { CircleAlert, LogIn } from 'lucide-vue-next'
import { api } from '@/lib/api'
import { useAuth } from '@/composables/useAuth'
import { Button } from '@/components/ui/button'
import BrandMark from '@/components/BrandMark.vue'

const route = useRoute()
const { login, error: sessionError } = useAuth()

const version = __PICWEIGHT_VERSION__
const issuer = ref('')
const failure = ref(route.query.error || '')

/** The issuer is worth showing: it is the one thing that differs per install. */
onMounted(async () => {
  try {
    const config = await api.authConfig()
    issuer.value = config.issuer
  } catch {
    // The sign-in button works without it; nothing to report.
  }
})
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background p-6">
    <div class="w-full max-w-sm space-y-8">
      <div class="flex flex-col items-center gap-3 text-center">
        <BrandMark class="size-14 text-ink" />
        <h1 class="text-2xl font-semibold tracking-tight text-ink">picweight</h1>
        <p class="text-sm text-ink-2">
          Photograph what you eat. See what is left in the tank.
        </p>
      </div>

      <div class="tick-rule-center" aria-hidden="true" />

      <p
        v-if="failure || sessionError"
        class="flex items-start gap-2 rounded-lg border border-critical/40 bg-critical/10 px-3 py-2 text-left text-sm text-critical"
      >
        <CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        {{ failure || sessionError }}
      </p>

      <Button size="lg" class="w-full" @click="login">
        <LogIn /> Sign in with SSO
      </Button>

      <p v-if="issuer" class="num text-center text-[11px] text-ink-3">{{ issuer }}</p>
      <p class="eyebrow text-center">picweight {{ version }}</p>
    </div>
  </div>
</template>
