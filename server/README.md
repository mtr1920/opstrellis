# OpsTrellis Cloud (server)

The managed backend for OpsTrellis: an OpenAI-compatible speech/LLM gateway with metering, quotas and
provider fallback, the desktop sign-in and usage APIs, Stripe billing, and a React dashboard. One
Node process (Hono) serves the API and the built dashboard. Data lives in Postgres.

```
src/
  index.ts          process entry (HTTP server, periodic jobs)
  app.ts            routes, static dashboard, error handling
  config.ts         environment
  auth.ts           sessions, API tokens, requireAuth, CSRF origin check, rate limiting
  billing.ts        plans, billing periods, included/overage split (pure, unit-tested)
  usage.ts          metering, /api/me summary, user and admin stats
  catalog.ts        model catalog and fallback chain
  stt.ts            STT adapters: openai, groq, deepgram, assemblyai, elevenlabs, google, azure, local (sherpa-onnx)
  llm.ts            chat adapters: openai, groq, openrouter, anthropic (Messages API translation)
  providers.ts      upstream keys (AES-256-GCM in DB, env fallback)
  stripe.ts         Checkout, Portal, webhook handling, Billing Meter reporting
  routes/           gateway (/v1), auth (+ device flow, tokens), api (me, usage, stats, billing), admin, team
  db/               Drizzle schema, migrate, seed
drizzle/            generated SQL migrations (committed)
web/                dashboard (React + Vite), built into dist/web
test/               vitest integration tests (real Postgres, real local model)
scripts/fetch-model.sh   downloads the self-hosted Moonshine model into models/
```

## Run locally

Requires Node 22+ and Postgres 14+.

```sh
cp .env.example .env              # then edit; at least DATABASE_URL, MASTER_KEY, ADMIN_EMAIL and ADMIN_PASSWORD
npm install
npm run fetch-model               # self-hosted STT model (~100 MB) into ./models
npm run db:migrate                # applies migrations and inserts the default catalog
ADMIN_EMAIL=you@example.com ADMIN_PASSWORD=... npm run db:seed   # optional; the server also does this at startup (see below)
npm run build && npm start        # http://localhost:8787
```

`npm run dev` runs the API with reload on :8787 and the dashboard with HMR on :5173 (proxying
`/api` and `/v1`). `npm start`, `dev`, `db:migrate` and `db:seed` read `.env` when present.

### Docker

```sh
docker compose up --build         # Postgres + server on :8787, migrations run on start
```

The image downloads the local model at build time (`--build-arg FETCH_MODEL=0` to skip and mount
`./models` at `/app/models`). The server container restarts unless stopped.

### The first admin

Set `ADMIN_EMAIL` and `ADMIN_PASSWORD` (in `.env`, which docker compose also reads) before exposing
the server. They must pass the signup rules (a valid address; a password of 8 to 200 characters), or
the server refuses to start. At every start, while there is no admin, the server creates that account
as admin before it starts listening; if an account with that email already exists (signup emails are
not verified, so someone else may have registered it), it is promoted, its password is set to
`ADMIN_PASSWORD`, its sessions, API tokens and pending device sign-ins are revoked, and it is taken out
of its team. The password is never logged. While `ADMIN_EMAIL` is set, no signup becomes admin.
Changing `ADMIN_PASSWORD` later does nothing once an admin exists; use `npm run db:seed` to reset it.
`npm run db:seed` applies the same create-or-promote on demand, even when an admin already exists (in
Docker: `docker compose exec -e ADMIN_EMAIL=you@example.com -e ADMIN_PASSWORD=... server node dist/db/seed.js`).

With neither set, the first account created on an empty database becomes admin, and the server logs a
warning at startup while there are no users: whoever reaches a fresh server first owns it.

## Configuration

See `.env.example`. The important ones:

| Variable | Purpose |
|---|---|
| `DATABASE_URL` | Postgres connection string |
| `PUBLIC_URL` | External origin; used for device links, Stripe redirects, secure cookies and CSRF checks |
| `MASTER_KEY` | Encrypts provider keys saved in the admin UI. Keep it stable (saved keys cannot be read with another one). Use 32+ random characters: the server refuses to start with the `.env.example` placeholder and warns about a shorter key (unset disables saving provider keys) |
| `ADMIN_EMAIL`, `ADMIN_PASSWORD` | The admin account, created or promoted at startup while there is no admin, and by `npm run db:seed`; promoting an existing account sets its password to `ADMIN_PASSWORD`, revokes its sessions, API tokens and device sign-ins, and removes it from its team. Invalid values stop the server from starting. With `ADMIN_EMAIL` set no signup becomes admin; with neither set the first signup on an empty database does (see The first admin) |
| `ALLOW_SIGNUP` | Default `true`; accepts `true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off` (any other value stops the server from starting). `false` makes `POST /api/auth/signup` answer `403 signup_disabled`, except for the first account when it would become admin; the dashboard then hides the signup form |
| `MAX_SIGNUPS_PER_DAY` | Default `200`. Accounts created in any rolling 24 hours across all clients; beyond it signup answers `429 signup_limit`. Separately, each client IP (IPv6: /64) may create 20 accounts a day and try 10 signups a minute (`429 rate_limited`). Like `MAX_OVERAGE_MINUTES`, a value that is not a non-negative integer stops the server from starting |
| `MAX_OVERAGE_MINUTES` | Default `3000`. Pro overage minutes allowed per billing period; beyond it the gateway answers `402 overage_cap_reached`. `0` disables overage (Pro stops at its 600 minutes) |
| `TRUST_PROXY` | Number of reverse proxies in front of the server (`1` for one; `true`/`false` mean 1/0; anything else stops the server from starting). The client IP is that many entries from the right of `X-Forwarded-For`. Behind a proxy without it, every client has the proxy's address, so all per-IP limits (login, signup, device start, team join) collapse into one bucket shared by everyone |
| `LOCAL_NUM_THREADS` | Threads for the self-hosted model (it decodes one request at a time, in windows of at most 30 s; at most 4 requests may be running or waiting, beyond that `503 local_model_busy`) |
| `LOCAL_MODEL_DIR` | Folder with self-hosted models; a `local/*` catalog entry's `upstream_model` is a subfolder |
| `OPENAI_API_KEY`, `GROQ_API_KEY`, `DEEPGRAM_API_KEY`, `ASSEMBLYAI_API_KEY`, `ELEVENLABS_API_KEY`, `GOOGLE_API_KEY`, `AZURE_SPEECH_KEY` + `AZURE_SPEECH_REGION`, `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY` | Platform keys, used when none is saved in the admin UI |
| `<PROVIDER>_BASE_URL` | Optional upstream base URL override (e.g. a self-hosted OpenAI-compatible server) |
| `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, `STRIPE_PRICE_PRO`, `STRIPE_PRICE_OVERAGE`, `STRIPE_METER_EVENT` | Billing. Without a secret key the dashboard shows manual billing and admins set plans. With `STRIPE_PRICE_PRO` set, only a subscription containing that price grants Pro, the plan follows the subscription events (not `checkout.session.completed`), and events for other products' subscriptions are ignored. Changing `STRIPE_PRICE_PRO` later drops existing subscribers to Free at their next subscription event |

### Stripe setup

1. Create a Billing Meter with event name `opstrellis_managed_minutes` (sum aggregation).
2. Create a Pro product with a $12/month recurring price (`STRIPE_PRICE_PRO`) and a metered
   $0.02/unit price tied to that meter (`STRIPE_PRICE_OVERAGE`).
3. Add a webhook to `https://<host>/api/stripe/webhook` for `checkout.session.completed`,
   `customer.subscription.created|updated|deleted` and `invoice.payment_failed`.
4. Enable the Customer Portal.

Overage minutes are sent as meter events with the usage event id as the idempotency identifier,
inline after each request and retried every 5 minutes.

## Billing rules

- Free: 15 managed minutes per month; a request that would go past them gets `402 quota_exceeded`.
- Pro: 600 minutes, then $0.02 per started minute of cumulative overage (the sum over a period is
  always `ceil(overage seconds / 60)`), up to `MAX_OVERAGE_MINUTES` (default 3000) overage minutes a
  period; a request past that gets `402 overage_cap_reached`. A past-due subscription stays Pro but gets
  no overage; `unpaid`, `incomplete`, `canceled` and other statuses are Free. An `unpaid` subscription
  stays linked, so Upgrade answers `409 already_subscribed` and the user pays it in the billing portal.
- Stripe event times have one-second resolution: an `incomplete` event (a state that only precedes
  Pro) for the tracked subscription cannot replace Pro unless it is strictly newer than the last event
  applied. `invoice.payment_failed` sets `past_due` without counting as the latest event, so a
  same-second `unpaid` still applies. Another subscription replaces the tracked one only when it is
  itself an active Pro subscription (a failed second checkout leaves the paying one in place).
- The period is the Stripe subscription period when known, else the UTC calendar month.
- BYOK and local usage reported by the desktop is free and only feeds stats.
- Every gateway call records provider cost (from the catalog) and the billed amount.

## Teams

A team shares a library (dictionary entries, snippets and voice actions) that its admins edit on the
dashboard's Team page and that every member's desktop app syncs down read-only through
`GET /api/library`. A user is in at most one team, and joins only with an invite code an admin made.
Plans and billing stay per user.

- Roles: `admin` manages members, invite codes and the library; `member` reads. The last active
  (not disabled) admin cannot be demoted, removed, or leave while others remain. When a team's last
  active admin is deleted or disabled by a server admin, a database trigger makes its longest-standing
  active member admin; a team whose members are all disabled keeps no active admin.
- Invite codes are 128-bit random, URL-safe strings, shown once and stored only as a SHA-256 hash.
  They work for anyone, any number of times, for 7 days or until revoked. Unknown, expired and revoked
  codes get the same `404`, and joining is limited to 10 attempts a minute per signed-in user.
- Revocation: removing a member deletes all of the team's invite codes (any of them would let the
  member straight back in); demoting an admin deletes the codes that admin created; a member who leaves
  takes the codes they created with them.
- Every library write runs in one transaction that locks the team row and increments
  `orgs.library_version`, and the entry stores that version. Deletes leave a tombstone (`deleted`,
  empty `data`), so `GET /api/library?since=N` returns every change after version `N`. Writes are
  limited to 60 a minute per user. Tombstones older than 30 days are purged (every 6 hours), which
  raises the team's `tombstone_floor` to the highest purged version.
- A team is deleted with its library and invites when its last member goes, by leaving or when the
  user is deleted (a database trigger on `org_members`).
- Limits: 2000 live entries per team; live entries of a kind are unique by their key (phrase, trigger
  or name) after Unicode NFKC normalisation and lowercasing (a unique index on `org_library.key`);
  team names 1–80 characters. Text containing NUL or an unpaired surrogate is rejected with `400`.
- Size: the live library may hold at most 6 MB (6,291,456 bytes) of text, measured as the UTF-8 length
  of each entry's JSON-serialised `data`; a create or edit past it gets `409 library_too_large`. This
  keeps a full sync well under the desktop's 16 MB response limit.
- Entries follow the rules desktops apply when syncing (else `400`): no control characters at all in a
  dictionary phrase or replacement, a snippet trigger or an action name; only line breaks (`\n`) and
  tabs in snippet expansions and action prompts; and a snippet expansion must not contain the
  `{clipboard}` placeholder (desktops refuse team snippets that would type the user's clipboard).

| Endpoint | Auth | Who | Result |
|---|---|---|---|
| `GET /api/team` | session or token | anyone | `{team: null}` or `{team: {id, name, role, members, libraryVersion}}` |
| `POST /api/team {name}` | session | not in a team | `201 {team}`; you become admin. `409 already_in_team` |
| `POST /api/team/join {code}` | session | not in a team | `{team}`. `404 invalid_code`, `409 already_in_team`, `429` |
| `POST /api/team/leave` | session | member | `{ok}`. `409 last_admin` if you are the only admin and others remain |
| `GET /api/team/members` | session | member | `{members: [{id, email, name, role}]}` |
| `PATCH /api/team/members/:userId {role}` | session | admin | `{member: {id, email, name, role}}`. `409 last_admin` |
| `DELETE /api/team/members/:userId` | session | admin | `{ok}`. `409 last_admin` |
| `POST /api/team/invites` | session | admin | `201 {id, code, expiresAt}` (the code is shown only here) |
| `GET /api/team/invites` | session | admin | `{invites: [{id, expiresAt, createdAt}]}` (unexpired) |
| `DELETE /api/team/invites/:id` | session | admin | `{ok}` |
| `GET /api/team/library` | session | member | `{version, entries: [{id, kind, data, updatedAt}]}` (live entries) |
| `POST /api/team/library {kind, data}` | session | admin | `201 {id, kind, data, updatedAt}`. `409 duplicate` or `library_full` |
| `PUT /api/team/library/:id {data}` | session | admin | `{id, kind, data, updatedAt}`; the kind cannot change. `409 duplicate` |
| `DELETE /api/team/library/:id` | session | admin | `{ok}`; leaves a tombstone |
| `GET /api/library?since=N` | session or token | anyone | `{team: {id, name} \| null, version, entries: [{id, kind, data, deleted}], more, reset}` (see Library sync); 60 requests a minute per user |

`data` by kind (unknown keys are rejected, strings are trimmed):
`dictionary {phrase: 1–100, replacement: 1–200 | null}`, `snippet {trigger: 1–100, expansion: 1–5000}`,
`action {name: 1–60, prompt: 1–2000, output: "replace" | "copy"}`.

Outside a team, the team routes answer `404 no_team`; members calling admin routes get `403 forbidden`;
ids of another team's members, invites or entries are `404 not_found`.

### Library sync

`GET /api/library?since=N` (`N` a non-negative integer, default 0) answers
`{team, version, entries, more, reset}`:

- A full sync is `since=0`, or any request answered with `reset: true`. It is one response with every
  live entry (at most 2000, no tombstones), `more: false`, and `version` = the team's current version.
  Full syncs are limited to 10 a minute per user (on top of the 60 for the endpoint).
- `reset: true` when `N > 0` but `N` is below the team's tombstone floor (deletes since then were
  purged) or above its current version (e.g. after a database restore).
- Otherwise (`N > 0`) it is an incremental sync: every entry changed after version `N`, tombstones
  (`deleted: true`, `data: {}`) included, oldest first, at most 500 per response. `more: true` means
  there are more, and `version` is then the version of the last entry returned; without `more`,
  `version` is the team's current version.
- Outside a team: `{team: null, version: 0, entries: [], more: false, reset: false}`.

After restoring the database from a backup, clients may hold cursors from writes the backup lost,
which the server cannot always detect (new writes may have passed them). Force every client to start
over by moving each team's version and floor far beyond any cursor issued, before the server serves
again:

```sql
UPDATE orgs SET library_version = library_version + 1000000000, tombstone_floor = library_version + 1000000000;
```

Client rule:

1. If `team` is null, or `team.id` differs from the team the client last synced, discard the response
   (it was computed with the old team's cursor), delete all local team entries, and, if `team` is not
   null, sync again from `since=0`. An answer to a request that was itself sent with `since=0` is
   already that team's full library: apply it instead of discarding it.
2. On `reset: true`, delete all local team entries first.
3. Upsert entries by `id` (they are final states, not a replayable log, so never enforce a local
   unique-on-key constraint), and delete those with `deleted: true`.
4. Store `version` (and `team.id`) as the cursor for the next request, and request again at once while
   `more` is true.

## Tests

```sh
createdb opstrellis_test                 # or set TEST_DATABASE_URL; the schema is rebuilt each run
npm run fetch-model                 # the gateway tests transcribe test_wavs/0.wav for real
npm test                            # builds the dashboard, then runs vitest
```

Live upstream tests (`test/live.test.ts`) run only when the matching key is set, for example
`DEEPGRAM_API_KEY=... NODE_USE_ENV_PROXY=1 npx vitest run test/live.test.ts`.

## HTTP API

All JSON: a non-empty body must be sent as `Content-Type: application/json` (else `415
unsupported_media_type`). Request bodies under `/api` are limited to 64 KB, 1 MB for `/api/usage` and
the Stripe webhook (`413 payload_too_large`). Desktop and scripts authenticate with
`Authorization: Bearer utt_...`; the dashboard uses an httpOnly session cookie. Cookie mutations, and
`/api/auth/{signup,login,logout}`, need a same-origin `Origin` (or `Referer`): when `PUBLIC_URL` is
https the full origin must equal it, otherwise the host must match the `Host` header. Rate limits key
IPv6 clients by their /64. Errors are `{error: code, message?}` under `/api` and
OpenAI-style `{error: {message, type, code}}` under `/v1`.

Gateway (base URL `https://host/v1`, the API token is the "OpenAI key"):

- `POST /v1/audio/transcriptions` multipart `file` (WAV: PCM16 or 32-bit float, mono or stereo, 8–48 kHz,
  ≤25 MB; 16 kHz mono PCM16 preferred), `model` (catalog id or `auto`), `language?`, `prompt?`
  (comma-separated terms become vocabulary hints), `response_format` `json` → `{text}` | `text` |
  `verbose_json` → `{task, language, duration, text, model}`. Header `x-opstrellis-model` names the model
  that answered. The duration is billed from the parsed samples, and upstreams receive a mono PCM16 WAV
  rebuilt from them.
  `402` over quota (`quota_exceeded`, or `overage_cap_reached` for Pro), `403` disabled, `400` bad
  model/audio, `413` too large, `429` more than 3 requests in flight (each user's requests run one at a
  time; the slot and an already-used-up quota are checked before the upload is read, and the upload of
  such an early rejection is then read and dropped, for at most 30 s and 3 at a time per user, so a
  client still sending gets the response), `503
  local_model_busy` when the self-hosted model has too many requests waiting, `502` all upstreams failed.
  The self-hosted model decodes audio longer than 30 s in consecutive 30 s windows (plain cuts, so a
  word spoken across a boundary may be split) and joins their text.
- `POST /v1/chat/completions` OpenAI body with `stream: false`; returns an OpenAI chat completion. Only `model`,
  `messages`, `temperature`, `top_p`, `max_tokens` (capped at 4096 and the remaining token allowance), `stop` and
  `response_format` are forwarded, with `n: 1`.
- `GET /v1/models` → `{object: "list", data: [{id, object, owned_by, kind, streaming}]}`.

Desktop API:

- `POST /api/device/start {device_name?}` → `{device_code, user_code, verification_url, interval, expires_in}`
- `POST /api/device/poll {device_code}` → `200 {token}` | `428 {error: "authorization_pending"}` | `400 {error: "expired_token"}`
- `GET /api/me` → `{user, plan, plan_status, period, usage: {managed_minutes, included_minutes, overage_minutes, remaining_minutes, words, cost_usd}, limits: {included_minutes, overage_per_min_usd, max_overage_minutes, llm_tokens}, preferred_model, preferred_llm}`;
  `PATCH /api/me {name?, preferred_model?, preferred_llm?}`
- `GET /api/catalog` → `{models: [{id, kind, provider, display_name, streaming, enabled, fallback_order, price_per_min_usd | input_per_1m_usd + output_per_1m_usd}], plans}`
- `POST /api/usage {events: [{id, provider, model, mode, kind?, audio_seconds, words, input_tokens?, output_tokens?, app_category?, provider_cost_usd?, created_at}]}` (≤500, idempotent by `id`, unknown fields such as text are rejected, `words` ≤ 1,000,000 and token counts ≤ 2³¹−1; 30 requests a minute per user) → `{accepted, duplicates}`
- `GET /api/stats?tz=Area/City` → totals, WPM, streaks, daily words, app categories, per model
- `GET /api/team`, `GET /api/library?since=N`: the user's team and its shared library (see Teams)

Dashboard-only: `/api/auth/{signup,login,logout}`, `GET /api/auth/signup` → `{open}` (whether the signup
form is shown), `POST /api/auth/password {current, next}` (session only; `403 wrong_password`; 5 tries per
15 minutes; signs out the user's other sessions but not their API tokens), `/api/tokens`,
`/api/device/{:code,approve,deny}` (30 lookups and 10 decisions a minute per user), `/api/billing{,/checkout,/portal}`
(`GET /api/billing` includes `max_overage_minutes`), `/api/team/*` and `POST /api/team` (see Teams), `/api/admin/*` (users, models, providers, stats).

Login is limited to 10 attempts a minute per IP, 10 per 15 minutes per account and IP (IPv6: /64),
and 200 per 15 minutes per account from all addresses together. Guessing from one place is slow, and
failed attempts from one address do not block the owner signing in from another; but the account-wide
ceiling is a trade-off: someone sending 200 wrong logins in 15 minutes from 20 or more address ranges
does lock the account for the rest of that window, in exchange for bounding distributed guessing. An
unknown email costs the same password-hash work as a wrong password. A login is checked against the
stored password hash again, with the user row locked, when its session is written, so a login that
races a password change or a `db:seed` reset gets no session once the change commits.
