# picweight — web app

Vue 3 + Vite + Tailwind 4 + shadcn-vue/reka-ui + lucide, matching phos's stack.

The phone is where capture happens; this is the review surface — the day's
budget, the history with thumbnails, the weight and macro trends, the profile
and targets, the export, and the agent-reasoning inspector where you can tell a
bad estimate what is wrong and watch the revision it produces.

A meal whose analysis failed is the one row here that is actionable rather than
informational: `RetryFailed` puts the failure reason and a Retry button
together, on the day list, in history and on the meal itself. The reason is half
the affordance — "you exceeded your current quota" is worth another tap and "the
image could not be decoded" never will be. Retry re-runs the agent at the same
revision against the thumbnail the server already stores, so nothing is asked of
a phone whose photo is of food that has since been eaten.

## Running it

```bash
npm install
npm run dev              # http://localhost:5173, proxying /api to the backend
npm run build            # → dist/, copied to /app/static in the container image
npm run generate         # regenerate src/lib/generated/ from android/openapi.json
npm run check:generated  # fail if the committed generated output is stale
npm run check:time       # fail if API instants stop parsing the way the spec says
```

`npm run dev` proxies `/api`, `/healthz` and `/picweight.apk` to
`http://localhost:33100`. Point it elsewhere with `PICWEIGHT_BACKEND`.

The footer version comes from `PICWEIGHT_VERSION`, falling back to the current
git tag, branch or short sha.

## How it is put together

| Path | What lives there |
|---|---|
| `src/lib/api.js` | the only place that talks to the backend; turns `ErrorBody` into readable messages and routes a 401 back to `/login` |
| `src/lib/format.js` | every number and local-day calculation, so rounding is decided once — and the only place an API timestamp is parsed |
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

## Timestamps

Nothing in `src/` calls `new Date(someApiValue)`. Every instant goes through
`parseInstant` in `src/lib/format.js`, and every local day through
`localDateKey` / `dayOffsetMinutes`.

That is not tidiness. The API declares its timestamp fields `format: date-time`
— RFC 3339, offset mandatory — and the backend briefly served them without one.
Android threw and told the user it was offline, which at least was visible. The
browser threw nothing: `new Date("2026-08-01T13:32:33.441")` with no `Z` is
*defined* to parse as local time, so every timestamp this app drew was quietly
wrong by the viewer's offset. For an app whose premise is that the day is yours
and not UTC's (PRD §8), that is a correctness bug wearing no symptoms.

So `parseInstant` reads an offset-less string as UTC (which is what the value
means) and warns once, truncates chrono's nanoseconds rather than relying on
engine-specific leniency, and returns `null` instead of an `Invalid Date` that
would throw out of `toISOString` and blank the page. `npm run check:time` pins
all of it, under a fixed DST-observing zone — under `TZ=UTC` the original bug is
invisible, which is exactly how it reached production.

## Authentication

There is no token in the browser. `GET /api/auth/login` starts
authorization-code + PKCE against the confidential web client and the callback
sets an HttpOnly `picweight_session` cookie; the SPA only ever reads
`GET /api/auth/me` to find out whether it has one.
