import { createRouter, createWebHistory } from 'vue-router'
import { setUnauthorizedHandler } from '@/lib/api'
import { useAuth } from '@/composables/useAuth'
import HistoryView from '@/views/HistoryView.vue'
import LoginPage from '@/views/LoginPage.vue'
import MealDetail from '@/views/MealDetail.vue'
import ProfileView from '@/views/ProfileView.vue'
import TodayView from '@/views/TodayView.vue'
import UsageView from '@/views/UsageView.vue'
import TrendsView from '@/views/TrendsView.vue'

const routes = [
  {
    path: '/login',
    name: 'login',
    component: LoginPage,
    meta: { public: true, chrome: false },
  },
  {
    path: '/',
    name: 'today',
    component: TodayView,
    meta: { view: 'today' },
  },
  {
    // A dated day view is the same component; the backend buckets by local day.
    path: '/day/:date',
    name: 'day',
    component: TodayView,
    meta: { view: 'today' },
  },
  {
    path: '/meal/:id',
    name: 'meal',
    component: MealDetail,
    meta: { view: 'today' },
  },
  {
    path: '/history',
    name: 'history',
    component: HistoryView,
    meta: { view: 'history' },
  },
  {
    path: '/trends',
    name: 'trends',
    component: TrendsView,
    meta: { view: 'trends' },
  },
  {
    path: '/usage',
    name: 'usage',
    component: UsageView,
    meta: { view: 'usage' },
  },
  {
    path: '/profile',
    name: 'profile',
    component: ProfileView,
    meta: { view: 'profile' },
  },
  {
    // The backend serves index.html for any unmatched path, so the SPA owns 404.
    path: '/:pathMatch(.*)*',
    redirect: { name: 'today' },
  },
]

export const router = createRouter({
  history: createWebHistory(),
  routes,
  scrollBehavior: (to, from, saved) => saved ?? { top: 0 },
})

router.beforeEach(async (to) => {
  if (to.meta.public) return true

  const { isAuthenticated, checked, fetchSession } = useAuth()
  if (!checked.value) await fetchSession()
  if (!isAuthenticated.value) return { name: 'login' }
  return true
})

// A session that expires mid-session lands the user back on /login rather than
// leaving a view stuck on an error it cannot recover from.
setUnauthorizedHandler(() => {
  const { forget } = useAuth()
  forget()
  if (router.currentRoute.value.name !== 'login') router.push({ name: 'login' })
})
