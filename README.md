# AI Subtitle & Article Translator

**English** | [简体中文](README.zh-CN.md)

A self-hosted AI translation site: upload a VTT/SRT subtitle file or an article, and DeepSeek
translates it into your target language. Side-by-side sentence view, manual editing, AI
annotations and multi-format export. Rust + Axum backend, Vue 3 SPA frontend, single binary
deployment.

Live demo: <https://tomatocat.art>

> This repository is the **backend** (Rust/Axum, serving only `/api` plus the static bundle).
> The UI lives in a separate repository,
> [**automated-translation-web**](https://github.com/Waterc0lorEyes/automated-translation-web);
> its build output has to be placed in `static/` here (see [Build the frontend](#3-build-the-frontend)).

---

## Contents

- [Features](#features)
- [Tech stack](#tech-stack)
- [Quick start](#quick-start)
- [Configuration](#configuration)
- [Project layout](#project-layout)
- [Design notes](#design-notes)
- [HTTP API](#http-api)
- [Export formats](#export-formats)
- [Internationalization](#internationalization)
- [Testing](#testing)
- [Deployment](#deployment)
- [Production runbook](DEPLOY.md)
- [Operations](#operations)
- [Known limitations](#known-limitations)
- [License](#license)

---

## Features

### Subtitle translation

- `.vtt` and `.srt`, auto-detected (pasting the text directly also works)
- Keeps the original positioning (`align:center line:81%`) and line breaks, so the exported
  subtitles sit exactly where they used to in the player
- Translation runs in the background, the page polls live progress, and you can leave at any time
- Entries that were not translated can be filled in with one click, without redoing finished ones
- Edit entry by entry; clearing an entry falls back to the source text on export

### Article translation

- Upload a TXT file or paste the text; **the AI splits it into sentences, then a second AI pass
  reviews the split** (merges fragments, breaks up run-on sentences, drops empty items, fills gaps)
- Splitting happens in the background: the upload request returns immediately and the page shows
  "splitting" while it polls
- Sentences are then translated the same way as subtitles, and shown side by side
- If splitting fails, the job fails honestly instead of silently falling back to translating the
  whole article in one go

### AI annotations

- Available for both subtitles and articles; give a topic (e.g. slang, cultural background, legal
  terminology) and the model explains the tricky parts
- Annotation language is selectable (Chinese / English / Japanese)
- Annotations are merged additively: regenerating them never wipes the existing ones

### Other

- Four UI locales: Simplified Chinese / Traditional (Taiwan) / Traditional (Hong Kong) / English,
  **auto-detected from the browser language**
- Accounts: sign up, sign in, one DeepSeek API key per user
- History, downloads, dark neon theme, drag-and-drop upload, toast notifications

---

## Tech stack

| Layer | Choice |
|---|---|
| Language | Rust 2021 (1.99.1 at the time of writing) |
| Web | Axum 0.8 + Tower HTTP |
| Routing | Vue Router owns every page; the backend only does the SPA fallback |
| Database | SQLite via sqlx (no compile-time query checking) |
| Passwords | Argon2 + password-hash |
| Sessions | HMAC-SHA256 signed cookie (hmac + sha2) |
| External service | DeepSeek Chat API (reqwest + rustls) |
| Frontend | Vue 3 + TypeScript + Vite 7 + Vue Router (SPA) |

---

## Quick start

### 1. Prerequisites

- A Rust toolchain (`rustup`)
- A DeepSeek API key: <https://platform.deepseek.com/>

### 2. Configure

```bash
cp .env.example .env
```

Edit `.env` and fill in at least:

```ini
DEFAULT_API_KEY=sk-your-key
SESSION_SECRET=$(openssl rand -hex 32)
```

### 3. Build the frontend

The frontend lives in its own repository. Clone it into `frontend/` (that path is already
ignored by `.gitignore`):

```bash
git clone https://github.com/Waterc0lorEyes/automated-translation-web.git frontend
cd frontend
npm ci
STATIC_DIR=../static npm run build   # writes ../static/index.html + assets/
```

### 4. Run

```bash
cargo run
# open http://localhost:3000
```

> **Note:** `static/` is served with relative paths, so the process must be started from the
> project root.

#### Without an API key

```bash
node dev/mock-deepseek.mjs &          # fake model listening on 3199
DEEPSEEK_API_URL=http://127.0.0.1:3199/chat/completions \
DEFAULT_API_KEY=mock-key cargo run
```

Or in a single command (release build + fake model + demo database, no API quota spent):

```bash
cargo build --release
MOCK=1 ./dev/run-demo.sh              # http://0.0.0.0:18080
```

Without `MOCK=1` it uses whatever `.env` says (real key, real database).

#### Frontend dev mode

```bash
cd frontend && npm install && npm run dev   # 5173, /api is proxied to 3000
```

(Requires the frontend repository to be cloned into `frontend/`, see above.)

### 5. Release build

```bash
cargo build --release
# artifact: target/release/automated-translation
```

To deploy, put the **binary + `static/` + `.env`** in the same directory; the SQLite file is
created on first start.

---

## Configuration

Everything is configured through environment variables (`.env`, or the systemd unit):

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | `sqlite://data.db` | SQLite path, relative to the working directory |
| `HOST` | `0.0.0.0` | Listen address |
| `PORT` | `3000` | Listen port |
| `DEFAULT_API_KEY` | empty | Fallback key used when a user has not configured their own |
| `LOG_FILE` | empty | Log file path; empty means stdout (journald) |
| `SESSION_SECRET` | random | Cookie signing key. **When unset, a random one is generated at every start, so all logins are invalidated on restart** |
| `COOKIE_SECURE` | `false` | Set to `true` behind HTTPS, otherwise the cookie is sent without `Secure` |
| `DEEPSEEK_API_URL` | official endpoint | Point this at a proxy, a compatible gateway or the local mock |
| `TRANSLATE_CONCURRENCY` | `3` | Number of in-flight translation requests, 1–8 |

---

## Project layout

```
automated-translation/
├── src/                      # Rust backend
│   ├── main.rs               # startup, router assembly, middleware
│   ├── config.rs             # environment parsing
│   ├── db.rs                 # pool, schema migration, resets interrupted jobs on boot
│   ├── models.rs             # User / Translation / SubtitleEntry / ArticleSegment
│   ├── parser.rs             # VTT/SRT parsing (keeps positioning and line breaks)
│   ├── translator.rs         # DeepSeek client, sentence splitting, translation, export
│   ├── routes.rs             # /api JSON handlers + SPA fallback + Origin guard
│   ├── session.rs            # signed sessions and cookie attributes
│   └── i18n.rs               # four-locale copy table (the frontend dictionary is generated from it)
├── frontend/                 # Vue 3 SPA (separate repo, cloned here; git-ignored)
│   ├── src/api/              # types + fetch adapter (+ optional mock)
│   ├── src/views/            # home / upload / result / history / settings / login / register
│   ├── src/components/       # nav, table, progress bar, drop zone, toasts…
│   ├── src/composables/      # useAuth / usePolling / useToast
│   ├── src/i18n/             # dict.ts is generated from Rust, never edit by hand
│   └── src/styles/           # design tokens + base layer (light frosted glass)
├── static/                   # frontend build output, not committed (see above)
├── dev/mock-deepseek.mjs     # local mock model for running without a key
├── .env.example
├── FRONTEND-CONTRACT.md      # architecture and API contract (read this before changing the API)
└── SKILL.md                  # quick reference for AI coding assistants
```

---

## Design notes

### Data model

```sql
users(id, username UNIQUE, password_hash, api_key, created_at)

translations(id, user_id, filename, source_text, translated_text,
             source_lang, target_lang, status, created_at)
```

`translated_text` has exactly two valid shapes, decided by `translator::parse_content()` from the
JSON structure alone (**the file extension is never consulted**):

| Shape | Structure | Produced by |
|---|---|---|
| Subtitles | bare array `[{index,start,end,text,translation,note,settings}]` | `/subtitle` |
| Article | `{"kind":"article","segments":[{index,text,translation,note}]}` | `/article` |

If neither matches, the page reports "this record uses the old format" and downloads return 410 —
old data is deliberately not supported.

`status` state machine:

```
splitting ──> running ──> done
    │            │
    └────────────┴──────> failed
```

On startup any leftover `splitting`/`running` rows are reset to `failed`, so a record can never be
locked forever by a crashed process.

### Article pipeline

```
POST /article
   │  insert placeholder row segments=[] status=splitting
   ├──────────────> returns 303 /result/{id} immediately
   ▼
[background] AI sentence split (4000-char chunks) → {"sentences":[...]}
   ▼
[background] AI review (120 sentences per batch)  → merge / split / drop / fill
   ▼
write segments, status=running
   ▼
[background] translate in batches (id protocol), persisting after each batch
   ▼
status=done (or failed on error)
```

While `splitting`, the frontend shows a waiting message and polls; as soon as the status changes it
calls `location.reload()`.

### Translation protocol: JSON output + id write-back

Every request sends:

```json
{"items":[{"id":0,"text":"..."},{"id":1,"text":"..."}]}
```

The model replies:

```json
{"pairs":[{"id":0,"translation":"..."}]}
```

Key points:

- The request sets `response_format: {"type":"json_object"}`; if the endpoint rejects it, the field
  is dropped and the request retried once
- `id` is the index of the item in the batch array, and **write-back does no string matching at all**
- Only ids that were part of this batch are accepted; unknown or out-of-range ids are dropped and
  counted
- If the same line appears twice, each id is filled independently
- When the output is cut off by `max_tokens`, the parser scans the complete `{...}` objects it did
  receive and keeps the successful ones

Batching: 100 items first, then halve the batch size after each round if gaps remain
(`100 → 50 → 25 → 12 → 6`), up to 5 rounds. The source text is not echoed back, so 100 items stay
well below the 8192-token ceiling.

### Concurrency and consistency

| Problem | Solution |
|---|---|
| Batches are slow one after another | `buffer_unordered`, 3 by default, tunable via `TRANSLATE_CONCURRENCY` |
| Concurrent writes overwrite each other | every write is read-latest → modify → write-back, serialized by a global lock |
| Duplicate jobs (translate / continue / annotate) | atomic `UPDATE ... WHERE status NOT IN ('running','splitting')` claim; losers are redirected to `?busy=1` |
| Lost updates | only this job's delta is written (`merge_filled` / `merge_edits` / `merge_notes`), never a whole-document overwrite |

> The early whole-document overwrite really did lose every translation when you added annotations
> mid-translation; annotations and translations no longer overwrite each other.

### Sessions and security

- The cookie payload is `{user_id}.{HMAC-SHA256(user_id)}`; a bad signature is treated as signed out
- Cookie attributes: `HttpOnly` + `SameSite=Lax` + `Path=/` + `Max-Age=7d` (+ `Secure` under HTTPS)
- Passwords use Argon2 with default parameters
- All SQL is parameter-bound, templates escape by default, and dynamic frontend content goes through
  `textContent`

---

## HTTP API

Page routes (`/`, `/login`, `/upload/*`, `/result/*`, …) belong to Vue Router; the backend only
falls back to `static/index.html`. The real endpoints are below — see
[FRONTEND-CONTRACT.md](FRONTEND-CONTRACT.md) for the full field list:

| Method | Path | Description |
|---|---|---|
| POST | `/api/auth/register` | Sign up (logs in on success) |
| POST | `/api/auth/login` | Sign in |
| POST | `/api/auth/logout` | Sign out |
| GET | `/api/me` | Current user + masked key |
| GET | `/api/translations` | History list (latest 100, without entries) |
| GET | `/api/translations/{id}` | One record, including entries |
| GET | `/api/translations/{id}/status` | Progress polling: `total/done/status/entries` |
| POST | `/api/translations` | Upload (multipart) and start translating |
| POST | `/api/translations/{id}/edits` | Edit translations (`{"0":"译文","1":""}`, empty string clears) |
| POST | `/api/translations/{id}/annotate` | Generate annotations (`topic`, `note_lang`) |
| POST | `/api/translations/{id}/continue` | Translate the remaining entries |
| GET/PUT | `/api/settings` | Read / save the per-user API key |
| POST | `/api/lang` | Switch language, writes the `lang` cookie |
| GET | `/download/{id}/{format}` | Export (the browser navigates to it directly) |

Every write endpoint validates `Origin`; cross-site requests get a 403.

Record ids are **UUIDv7** externally (e.g. `01a18708-9621-76b7-…`), while the database keeps an
auto-increment primary key; old numeric ids no longer resolve (they return 404).

---

## Export formats

### Subtitles

| format | Content | Extension |
|---|---|---|
| `vtt` | WebVTT, keeps `align`/`line` positioning | `.vtt` |
| `srt` | SRT, comma-separated timestamps | `.srt` |
| `txt` | One translated sentence per line | `.txt` |

### Articles

| format | Content | Extension |
|---|---|---|
| `txt` | Translated sentences, one per line | `.txt` |
| `src` | Source sentences, one per line | `.txt` |
| `csv` | Source / translation / annotation side by side | `.csv` |

The CSV output is Excel-friendly: UTF-8 BOM, CRLF line endings, RFC 4180 escaping for fields that
contain commas, quotes or newlines, and a header row in the current UI language.

Any other format returns 400.

---

## Internationalization

Four locales: `zh` (Simplified) / `zh-TW` (Taiwan) / `zh-HK` (Hong Kong) / `en`.

Priority order on the frontend (`src/i18n/index.ts`):

1. Manual choice (localStorage, plus `POST /api/lang` to set the cookie)
2. Browser `Accept-Language` (`zh-HK`/`zh-MO` → Hong Kong, `zh-TW`/`zh-Hant*` → Taiwan,
   `zh-CN`/`zh` → Simplified, `en*` → English)
3. Simplified Chinese

The backend only reads the `lang` cookie to pick the CSV header language.

Hong Kong and Taiwan use different wording (帳戶/帳號, 用戶/使用者, 註釋/註解, 檔案名稱/檔名).

**Adding a string:**

1. Add a four-tuple to `get_translation()` in `src/i18n.rs`:
   `["简体", "繁體(台)", "繁體(港)", "English"]`
2. `cd frontend && npm run gen:i18n` to regenerate `src/i18n/dict.ts`
3. Use `t('key')` in the component

The frontend dictionary is generated — **do not edit it by hand**. CI can run
`npm run gen:i18n && git diff --exit-code`.

---

## Testing

```bash
cargo test        # 70 unit tests
cargo clippy      # 0 warnings

cd frontend
npm run typecheck # 0 errors
npm test          # 10 smoke tests (happy-dom mounts the whole SPA)
```

What is covered:

- Subtitle parsing/export (positioning, line breaks, SRT commas)
- Session signing (forgery, tampering, wrong key, malformed tokens)
- JSON extraction (object envelope, bare array, code fences, **truncation recovery**, string ids)
- Concurrent persistence (annotations do not clobber translations, late writes do not clobber edits,
  clearing works)
- CSV export (BOM, header, comma/quote/newline escaping)
- Language detection (Traditional/Simplified, Hong Kong, q-value ordering, edge inputs)
- Header generation (non-ASCII names, control characters, CRLF injection)

---

## Deployment

> The full runbook — build, systemd, nginx, HTTPS, backup/rollback, troubleshooting, go-live
> checklist — is in **[DEPLOY.md](DEPLOY.md)**. Below is the short path.

### Server

Ubuntu 24.04 / x86_64 / glibc 2.39, managed by systemd, nginx reverse-proxying to the local port.

### First deployment

```bash
# build locally: frontend first, then backend
cd frontend && npm ci && STATIC_DIR=../static npm run build && cd ..
cargo build --release

# upload (binary + frontend output; keep the server's .env and data.db)
scp target/release/automated-translation user@host:~/automated-translation/
tar czf - static | ssh user@host 'tar xzf - -C ~/automated-translation'
```

On the server:

```bash
cd ~/automated-translation
chmod -R a+r static
```

Example production `.env`:

```ini
DATABASE_URL=sqlite://data.db
HOST=0.0.0.0
PORT=3001
DEFAULT_API_KEY=sk-xxx
LOG_FILE=app.log
SESSION_SECRET=<openssl rand -hex 32>
COOKIE_SECURE=true
```

### systemd unit

`/etc/systemd/system/automated-translation.service`:

```ini
[Unit]
Description=Automated Translation
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=<user>
Group=<user>
WorkingDirectory=/home/<user>/automated-translation
ExecStart=/home/<user>/automated-translation/automated-translation
Restart=always
RestartSec=3
KillSignal=SIGTERM

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now automated-translation
```

### nginx

```nginx
location / {
    proxy_pass http://127.0.0.1:3001;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

Certificates via acme.sh or certbot.

### Updating

```bash
# back up → stop → upload → start
ssh host 'cd ~ && tar czf ~/backups/app-$(date +%F-%H%M).tar.gz automated-translation/ && sudo systemctl stop automated-translation'
scp target/release/automated-translation host:~/automated-translation/automated-translation.new
tar czf - static | ssh host 'tar xzf - -C ~/automated-translation'
ssh host 'cd ~/automated-translation && mv -f automated-translation.new automated-translation && sudo systemctl start automated-translation'
```

### Rollback

```bash
sudo systemctl stop automated-translation
cd ~ && tar xzf ~/backups/app-<timestamp>.tar.gz
sudo systemctl start automated-translation
```

Schema changes happen automatically in code (`CREATE TABLE IF NOT EXISTS` + `ALTER TABLE ADD COLUMN`),
so there is no manual migration — but **old data formats are not supported**, so back up `data.db`
before upgrading.

---

## Operations

```bash
# status / logs
systemctl status automated-translation
journalctl -u automated-translation -f
tail -f ~/automated-translation/app.log      # if LOG_FILE is set

# restart
sudo systemctl restart automated-translation

# back up the database (safe while the service is running)
cp ~/automated-translation/data.db ~/backups/data.db-$(date +%F-%H%M)
```

---

## Known limitations

- **One job per record at a time** (translate / continue / annotate are mutually exclusive); a second
  request is told that a job is already running
- **Clearing an edit deletes the translation**; export falls back to the source text
- If the process crashes during sentence splitting, the record becomes `failed` after restart and has
  to be uploaded again (the source text is still in `source_text`)
- History has no pagination and no delete action
- SQLite is a single file without WAL, which suits a single instance; it is not meant for multiple
  instances sharing one database
- `app.log` is not rotated; for long-running deployments prefer journald or configure logrotate
- Long articles are split in 4000-character chunks, so the splitting stage takes a while for very
  long texts
- The CSV export is sentence-based and does not preserve the paragraph structure of the source

---

## License

[MIT](LICENSE)
