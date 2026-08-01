# picweight

AI-assisted calorie & БЖУ tracker. Photograph what you eat and drink; a
server-side estimation agent returns calories and macros; every logged meal
immediately tells you what budget you have left today.

Self-hosted, single container, deployed to Kubernetes via Helm. Rust backend,
Vue 3 web app, Android app — all in one image.

> Manual calorie tracking fails for one reason: friction per meal. The bet is
> that *photo → structured macros* makes daily logging survivable. See
> [`docs/PRD.md`](docs/PRD.md) for the full design and
> [`ARCHITECTURE.md`](ARCHITECTURE.md) for how it is put together.

## Features

- **Photograph a meal, get macros** — a bounded agentic loop (rig 0.41 on
  OpenAI) identifies the dish, checks your own history, estimates per-item grams
  and macros, critiques itself once, and emits structured items with a reasoning
  note each.
- **Recall from your own confirmed meals** — the primary accuracy mechanism.
  Confirm a dish once and the next order starts from your corrected numbers.
- **Correction by conversation** — "too much rice, about half that". The agent
  session is persisted and *resumed*, not restarted, so the correction costs one
  short turn rather than a second full loop. Every revision is retained.
- **Multi-dish sittings** — N photos in one `group_id` run N concurrent loops,
  land as N independently correctable meals, and fire **one** notification.
- **Targets that are arithmetic, not a language model** — Mifflin-St Jeor BMR ×
  activity factor + goal delta, with protein and fat floors. Deterministic and
  auditable.
- **Feedback the moment you log, never on a schedule** — one self-contained
  notification per meal: what you logged, where you stand, what is left.
- **Barcodes** — Open Food Facts for packaged goods, the one place a real
  database still earns its keep.
- **Thumbnails only** — originals are deleted after analysis; one 768px JPEG per
  meal, content-addressed. Two years at 5 meals/day is roughly 350MB.
- **Generic OIDC** — any compliant IdP, primary target Zitadel. A confidential
  web client and a public/native client for the phone.
- **One image** — backend, SPA and APK. The web UI links the APK it shipped with.

## Quick start

### Docker

```bash
docker pull ghcr.io/diverofdark/picweight:latest
```

```yaml
services:
  picweight:
    image: ghcr.io/diverofdark/picweight:latest
    ports:
      - "33100:33100"
    volumes:
      - ./data:/app/data
    environment:
      - RUST_LOG=info
      - PICWEIGHT_DATA_PATH=/app/data
      - PICWEIGHT_OPENAI_API_KEY=sk-...
      - PICWEIGHT_OIDC_ISSUER=https://auth.example.com
      - PICWEIGHT_OIDC_CLIENT_ID=picweight
      - PICWEIGHT_OIDC_CLIENT_SECRET=...
      - PICWEIGHT_OIDC_MOBILE_CLIENT_ID=picweight-android
      - PICWEIGHT_OIDC_REDIRECT_URI=https://picweight.example.com/api/auth/callback
    restart: unless-stopped
```

Open <http://localhost:33100>. The APK is at `/picweight.apk`; the OpenAPI
explorer is at `/api/docs`.

The repo's [`docker-compose.yml`](docker-compose.yml) builds from source
instead — including the APK, so the first build is slow.

### Kubernetes (Helm)

```bash
helm upgrade --install picweight ./helm/picweight \
  --namespace picweight --create-namespace \
  --set ingress.enabled=true \
  --set ingress.host=picweight.example.com \
  --set oidc.issuer=https://auth.example.com
```

**The chart never creates a Secret and assumes no secrets operator.** It
references two Secrets by name — `<release>-oidc` and `<release>-openai` by
default, overridable with `oidc.existingSecret` / `openai.existingSecret`.
Create them however you already do:

```bash
kubectl -n picweight create secret generic picweight-oidc \
  --from-literal=client_id=picweight \
  --from-literal=client_secret=... \
  --from-literal=mobile_client_id=picweight-android

kubectl -n picweight create secret generic picweight-openai \
  --from-literal=api_key=sk-...
```

…or drop a SealedSecret, ExternalSecret, SOPS Secret or anything else into
`additionalObjects` in values.yaml. Each entry is rendered through `tpl`, so it
can reference the release-derived names:

```yaml
additionalObjects:
  - apiVersion: bitnami.com/v1alpha1
    kind: SealedSecret
    metadata:
      name: '{{ include "picweight.fullname" $ }}-openai'
    spec:
      encryptedData:
        api_key: AgBy3i4OJSWK...
```

Worked examples for both backends ship in
[`helm/picweight/values.yaml`](helm/picweight/values.yaml) and in
[`helm/picweight/ci/`](helm/picweight/ci/), which CI renders on every push.

Deployed by ArgoCD, pin the image to the commit being synced:

```yaml
image:
  tag: sha-$ARGOCD_APP_REVISION_SHORT
```

## Backups

Backups are deliberately not an app feature — the PVC holds everything
(`picweight.db` plus `thumbs/`), so `rclone` covers it:

```bash
rclone sync /var/lib/picweight-data remote:backups/picweight --backup-dir remote:backups/picweight-old
```

In-cluster, run it against the same PVC from a CronJob, or snapshot the volume.
Take SQLite consistently — either stop the pod, or copy with
`sqlite3 picweight.db ".backup /tmp/picweight.db"` first; the database runs in
WAL mode, so a naive file copy can miss committed pages.

## Environment variables

Everything is read once at startup. A missing required variable crash-loops with
a message naming it, rather than 500-ing on the first request.

### Core

| Variable | Default | Description |
|---|---|---|
| `PICWEIGHT_PORT` | `33100` | HTTP port (phos holds 33000) |
| `PICWEIGHT_DATA_PATH` | `./data` | SQLite database + `thumbs/`. This is the thing to back up |
| `PICWEIGHT_STATIC_DIR` | `./static` | Built SPA + `picweight.apk`; set to `/app/static` in the image |
| `PICWEIGHT_DATABASE_URL` | `<data_path>/picweight.db` | Override the SQLite file location |
| `RUST_LOG` / `PICWEIGHT_RUST_LOG` | `info` | `tracing-subscriber` filter, e.g. `info,picweight_backend::agent=debug` |

### OpenAI

The key lives **only** in the backend; clients never talk to OpenAI. Spend is
capped provider-side, so there is no in-app budgeting — but quota exhaustion
fails loudly: the meal goes to `failed` with a visible reason.

| Variable | Default | Description |
|---|---|---|
| `PICWEIGHT_OPENAI_API_KEY` | *(required)* | Running without a key is an explicit non-goal |
| `PICWEIGHT_OPENAI_MODEL` | `gpt-4.1` | Vision model handed to rig |
| `PICWEIGHT_OPENAI_BASE_URL` | `https://api.openai.com/v1` | Point at LiteLLM / vLLM / Azure, or a mock in tests |
| `PICWEIGHT_WEB_SEARCH_ENABLED` | `false` | Registers the agent's `web_search` tool (chain-restaurant nutrition) |

### OIDC / SSO

Two clients, as in phos: a confidential **web** client and an optional
public/native **mobile** client (PKCE, no secret) for the Android app.

| Variable | Default | Description |
|---|---|---|
| `PICWEIGHT_OIDC_ISSUER` | *(required)* | Issuer URL; discovery does the rest |
| `PICWEIGHT_OIDC_CLIENT_ID` | *(required)* | Confidential web client id |
| `PICWEIGHT_OIDC_CLIENT_SECRET` | *(required)* | Confidential web client secret |
| `PICWEIGHT_OIDC_MOBILE_CLIENT_ID` | *(unset)* | Public/native client id used by the Android app |
| `PICWEIGHT_OIDC_REDIRECT_URI` | `http://localhost:<port>/api/auth/callback` | Must match the client registration |
| `PICWEIGHT_OIDC_SCOPES` | `openid profile email` | Space-separated |
| `PICWEIGHT_JWT_SECRET` | *(auto-generated)* | Signs session JWTs; persisted to `<data_path>/.picweight_jwt_secret` |
| `PICWEIGHT_JWT_TTL` | `1209600` (14 days) | Session lifetime in seconds |

## Development

### Prerequisites

Rust (1.96+), Node 22+, JDK 17 for the Android app. No system ML libraries:
SQLite is compiled into the binary (`libsqlite3-sys` with `bundled`), and there
is no ONNX or ffmpeg. On Debian/Ubuntu, `pkg-config` and `libssl-dev` is the
whole list.

### Backend

```bash
cd backend
cargo run                     # dev server on :33100
cargo test                    # unit + integration (temp SQLite, mock IdP, mock LLM)
cargo clippy --all-targets
cargo run -- openapi ../android/openapi.json   # regenerate the API contract
```

`cargo run -- openapi` needs no configuration at all — no database, no OIDC, no
API key — because the Docker build runs it in a stage that has none of those.
`cargo test` fails if the committed `android/openapi.json` is stale.

### Frontend

```bash
cd frontend
npm install
npm run dev      # Vite with HMR, proxying /api to :33100
npm run build    # → dist/
```

### Android

```bash
cd android
./gradlew assembleDebug
```

The Retrofit client is generated from `android/openapi.json` by
`org.openapi.generator`, so the API contract cannot drift by hand-editing.

### Helm

```bash
helm lint helm/picweight
helm template picweight helm/picweight --values helm/picweight/ci/sealedsecret-values.yaml
```

## Verification

`cargo test` covers Mifflin-St Jeor against published reference values, the
macro split, local-day bucketing across timezone boundaries, and the §13
integration suite: ingest → loop → confirm, idempotent replay of a `client_uuid`,
three photos in one group producing three loops and exactly one notification, a
correction writing revision 2 without repeating tool calls, and user B getting a
404 on every one of user A's endpoints. These run **inside the Docker build**, so
a red test fails the image.

CI additionally lints and renders the Helm chart against three secrets backends
and asserts no chart template ever emits a Secret.

## API

Documented with `utoipa`, browsable at `/api/docs`, exported to
`android/openapi.json`. Full route list in [`docs/PRD.md`](docs/PRD.md) §9.

## License

MIT
