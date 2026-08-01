# picweight — web app

Vue 3 + Vite + Tailwind 4 + shadcn-vue/reka-ui + lucide, matching phos's stack.

The phone is where capture happens; this is the review surface — the day's
budget, the history with thumbnails, the weight and macro trends, the profile
and targets, the export, and the agent-reasoning inspector where you can tell a
bad estimate what is wrong and watch the revision it produces.

## Running it

```bash
npm install
npm run dev      # http://localhost:5173, proxying /api to the backend
npm run build    # → dist/, copied to /app/static in the container image
```

`npm run dev` proxies `/api`, `/healthz` and `/picweight.apk` to
`http://localhost:33100`. Point it elsewhere with `PICWEIGHT_BACKEND`.

The footer version comes from `PICWEIGHT_VERSION`, falling back to the current
git tag, branch or short sha.

## How it is put together

| Path | What lives there |
|---|---|
| `src/lib/api.js` | the only place that talks to the backend; turns `ErrorBody` into readable messages and routes a 401 back to `/login` |
| `src/lib/format.js` | every number and local-day calculation, so rounding is decided once |
| `src/lib/chart.js` | scales and arc/line path builders for the two hand-rolled SVG charts |
| `src/lib/status.js` | `DayStatus` → the accent colour the whole page takes |
| `src/composables/useAuth.js` | session claims from `GET /api/auth/me` |
| `src/composables/useMealEvents.js` | one shared `EventSource` on `/api/v1/meals/events` |
| `src/components/ui/` | shadcn-vue primitives over reka-ui |
| `src/views/` | one file per route |

## Two design rules

1. **Every measurement is mono with tabular figures; every sentence is sans.**
   Numbers and prose never wear each other's clothes.
2. **The accent is the day's verdict.** `--day-accent` is rewritten from the
   `DayStatus` the rules engine returns, so the gauge, the brand mark and the
   active nav item shift green → amber → red with how the day is going. Nothing
   else in the interface is coloured for decoration.

Macro colours are fixed and never reused: protein `#3987e5`, fat `#d95926`,
carbs `#199e70` — validated for colourblind separation against the dark
surface across all pairs. The weight trend deliberately takes an ink colour
rather than a fourth hue, because no fourth hue clears that floor against those
three.

## Authentication

There is no token in the browser. `GET /api/auth/login` starts
authorization-code + PKCE against the confidential web client and the callback
sets an HttpOnly `picweight_session` cookie; the SPA only ever reads
`GET /api/auth/me` to find out whether it has one.
