# ATLAS HTTP API

The local API of ATLAS. One Rust process (`atlas`, with `atlas-web` in-process) serves the JSON
API, the SSE chat stream and the static UI from `public/` on `http://127.0.0.1:8787` — loopback
only, no auth: `boot::bind_address` refuses any other host before a socket exists.

Everything is JSON in, JSON out. There is no pagination token, no API key, no external service
involved. The table in `rust/atlas-web/src/routes/mod.rs` is the only source of truth, and
`rust/atlas-web/tests/routes.rs` drives all 49 documented rows through it (status and top-level
keys) — `atlas-e2e` drives the same rows over a real socket and fails if the two tables differ.

```
cargo test --offline -p atlas-web --test routes    # the contract, in-process
cargo run -p atlas-e2e -- run                      # the same contract, over a socket
```

## Conventions

| Topic | Rule |
| --- | --- |
| Content type | `application/json; charset=utf-8` for every `/api/*` answer |
| Encoding | compact JSON, UTF-8, no pretty printing |
| Caching | `Cache-Control: no-store` on every `/api/*` response |
| Headers | the seven documented security headers on every response, including errors and static files (`Content-Security-Policy`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cross-Origin-Resource-Policy: same-origin`, `Cross-Origin-Opener-Policy: same-origin`, `X-Frame-Options: DENY`, `Permissions-Policy`) |
| Identity | only a loopback authority may address the server (`127.0.0.1`, `localhost`, `::1`); anything else is `403 host not allowed` |
| State-changing calls | `POST`/`PUT`/`PATCH`/`DELETE` must come from `http://<loopback>:<this port>`, or from no browser at all (no `Origin`, no `Referer`) — otherwise `403 cross-origin request refused` |
| Methods | `GET`, `HEAD`, `POST`, `DELETE`. Anything else is `405 method not allowed`; a known path with the wrong method is `404 not found` |
| Errors | `{"error": "human readable message"}` — redacted, capped at 300 characters, `<`/`>` rendered as `‹`/`›`, never HTML, never a traceback |
| Timestamps | ISO-8601 UTC, `2026-11-21T09:42:40Z` |
| Query numbers | parsed as integers and clamped into the documented range |
| Query text | control characters dropped, whitespace collapsed, `%` and `\` removed, `_` replaced by a space in `LIKE` patterns; over-long input is a `400`, never silently truncated |
| Ids | positive integers; anything else is a `400` that names the parameter |

Error status codes:

| Code | Meaning |
| --- | --- |
| `400` | a parameter or body is unusable (not a number, too long, wrong characters, not a JSON object), **or** one of the seven headers the gate decides from is repeated, unreadable or empty (`host`, `origin`, `referer`, `sec-fetch-site`, `content-length`, `transfer-encoding`, `content-type` — a repeated `Host` is refused rather than resolved), **or** the request target is absolute-form (a proxy shape: `GET http://evil.example/api/status`) |
| `403` | the request did not come from this origin (`host not allowed`, `cross-origin request refused`) — a state-changing request whose `Origin`/`Referer` is not this loopback port, or whose `Sec-Fetch-Site` says it came from another site |
| `404` | no such endpoint, article, thread, entity or category — or a static path the policy refused |
| `405` | method not supported at all (`PUT`, `PATCH`, `OPTIONS`) — decided before the path is looked at, so a method this server does not have is a `405` even when the request line is a shape only that method uses |
| `411` | a chunked request body |
| `413` | request body larger than 1 MiB |
| `414` | query string longer than 2 048 **bytes** |
| `415` | a body with a `Content-Type` that is neither `application/json` nor `text/plain` |
| `429` | a rate-limited endpoint already has a request in flight or ten in the last minute — with `Retry-After` |
| `431` | more than 64 headers, or more than 32 KiB of headers |
| `500` | unexpected server-side failure. The payload is always `{"error": "internal error"}` (or the owner crate's reason); the detail goes to `data/app.log` only |

Examples:

```console
$ curl -s "http://127.0.0.1:8787/api/articles?hours=abc"
{"error":"hours must be an integer"}
$ curl -s "http://127.0.0.1:8787/api/article/9999"
{"error":"article not found"}
$ curl -s -X POST http://127.0.0.1:8787/api/topics -H 'Content-Type: application/x-www-form-urlencoded' -d '{}'
{"error":"request body must be application/json"}
```

## Parameter ranges

| Name | Default | Range | Used by |
| --- | --- | --- | --- |
| `hours` | `window_hours` (72) | 1-720 | articles, search, graph/articles, map, category |
| `hours` (alerts) | 48 | 1-720 | alerts |
| `hours` (briefing body) | 24 | 1-168 | `POST /api/briefing` |
| `limit` | endpoint specific | see below | list endpoints |
| `offset` | 0 | 0-100000 | articles |
| `days` | 7 | 1-90 | timeline |
| `depth` | 2 | 1-4 | graph |
| `n` | 12 | 1-120 | read |
| `min_matches` | 1 | 1-50 | topics |
| `threshold` | 5 | 1-100 | monitors |

Values outside the range are clamped, not rejected: `limit=99999` returns at most 200 rows.
Non-numeric values are a `400` that names the parameter.

`limit` maxima per endpoint: `/api/articles` 200 (default 60), `/api/search` 100 (25),
`/api/threads` 100 (40), `/api/graph` 200 (120), `/api/graph/articles` 200 (120),
`/api/entities` 60 (30), `/api/alerts` 200 (50), `/api/tags` 80 (30).

Text parameters are capped; over-long input is a `400` (`"<name> is too long (max N characters)"`).
Query and path values: `q` 200; `place`, `entity`, `category`, `name` (entity/category lookups)
and `topic` 80; `tag` 60; `iso` 12 (must look like `USA` or `EN-GB`); `from` / `to` 32 (must look
like `2026-11-21` or `2026-11-21T00:00:00Z`); `monitor` (alert filter) 80. JSON body values:
`name` 80, `keywords` 800, `exclude` 400 and `topic` 120 for topics and research; `name` 120 and
`query` 500 for monitors.

Every value is bound as a SQL parameter; no user string is ever concatenated into SQL.

---

# Read endpoints

## GET /api/status

One call for the dashboard: counts, collection state, model, browser, freshness window, and what
the process verified about itself.

Every key is always present: `articles`, `stored_articles`, `filtered_articles`, `topics`,
`active_topics`, `entities`, `edges`, `threads`, `links`, `categories`, `alerts_unseen`,
`monitors`, `last_collect`, `collect_note`, `collect_base`, `fulltext`, `briefing_at`, `ki_up`,
`model`, `browser`, `chat`, `interval_min`, `window_hours`, `max_age_hours`, `rust`, `native`,
`guard`, `security`.

| Key | Shape |
| --- | --- |
| `fulltext` | `{total, done, pending, pct, reader}` — `reader` is the backfill's own progress |
| `browser` | `{executable, running, headless, …}` — `atlas_browser::Browser::status()` |
| `chat` | `{max_steps, total_token_budget, model}` — the numbers the agent loop enforces |
| `rust` | `{available: true, path: <this executable>, source: "rust", …}` |
| `native` | `{required: [9 names], present, missing, dir, ok, boot: {checked, ok, reason}}` |
| `guard` | `{source: "rust", available: true, bridge: "atlas-guard", …}` |
| `security` | `{boot: {checked, ok, surface, downloads, at}, http: "rust", loopback, downloads}` |

`ki_up` is `false` and `model` is `null` when the local model does not answer; `articles` is the
count inside the freshness window, `stored_articles` is the whole table.

```console
$ curl -s http://127.0.0.1:8787/api/status
{"articles":103,"stored_articles":1370,"filtered_articles":1267,"topics":1,"active_topics":1,"entities":812,"edges":1904,"threads":37,"links":2210,"categories":9,"alerts_unseen":0,"monitors":1,"last_collect":"2026-11-21T09:40:02Z","collect_note":"","collect_base":false,"fulltext":{"total":104,"done":15,"pending":89,"pct":14,"reader":{}},"briefing_at":null,"ki_up":true,"model":"local","browser":{"executable":"/Applications/Google Chrome.app/…","running":false,"headless":true},"chat":{"max_steps":10,"total_token_budget":24000,"model":"local"},"interval_min":60,"window_hours":72,"max_age_hours":72,"rust":{"available":true,"path":"…/atlas-web","source":"rust"},"native":{"ok":true,"missing":[]},"guard":{"source":"rust"},"security":{"boot":{"checked":true,"ok":true,"downloads":{"ok":true}},"http":"rust","loopback":true,"downloads":true}}
```

## GET /api/health

Cheap liveness probe: five keys, one `SELECT 1`.

```json
{"ok": true, "time": "2026-11-21T09:51:34Z", "uptime_s": 412, "articles": 103, "model_up": true}
```

`ok` is `false` only when the database does not answer; `model_up` is `false` when the model probe
fails (a health answer is still a `200`).

## GET /api/security

What the process verified about itself. Five keys: `protocol` (`"atlas-gate/1"`), `http`, `text`,
`machine`, `boot`.

- `http` — the gate in force: `{gate, source: "rust", reason, note, decisions, rust_decisions,
  native, headers: [the seven names], limits: {body_bytes, headers, header_bytes, query_bytes,
  static_bytes}, rate_limit: {endpoints, per_min, max_running}, rate: {windows, running}}`.
- `text` — `{source: "rust", available: true, reason: "no bridge: atlas-web has no text
  subprocess", …}`: the text rules are linked, not a child process.
- `machine` — the configuration report: `{bind_host, loopback_only, ports: {http, llama, cdp},
  shell, env_allowlist: [...], data_mode, file_mode, permissions: {posix, entries: [{path, …}],
  ok}, warnings: [...]}`. `permissions.entries` are the paths the app hardened at boot.
- `boot` — `{checked, ok, surface, downloads, at}`, or `{checked: false, ok, reason}` when the
  audit could not run. `surface` is the listener verdict; `downloads` is the no-download audit.

## GET /api/stats

The archive at a glance. Arguments are ignored (a bad one is not a `400`).

`{articles, entities, edges, threads, connections, sources, categories, topics: {topics, active,
relevant_articles, stored_articles, filtered_articles}, last_collect, model, top_entities:
[{name, type, count}]}` — `top_entities` is the ten most-mentioned entities.

## GET /api/articles

The article list with every UI filter. All parameters optional: `hours`, `limit`, `offset`, `q`,
`iso`, `category`, `thread`, `topic` (id or name), `place`, `tag`, `from`, `to`.

Response: `{"total": int, "articles": [row]}` where `total` counts every row matching the filter,
not just the page. A row is `{id, title, summary, published, iso, url, place, geo_src, source,
thread_id, tags, topics}` (`tags` / `topics` are ` · `-joined strings, empty when none).

```console
$ curl -s "http://127.0.0.1:8787/api/articles?hours=24&limit=2&iso=USA"
{"total":7,"articles":[{"id":413,"title":"…","summary":"…","published":"2026-11-21T08:10:00Z","iso":"USA","url":"https://…","place":"Washington","geo_src":"text","source":"Reuters","thread_id":12,"tags":"","topics":"Semiconductors"}]}
```

## GET /api/article/{id}

One article: every row column plus `source`, `source_url`, `country`, `entities`, `categories`,
`topics` (`[{id, name, score, matched}]`), `related`, `tags`, `words`, `thread` (`{id, title,
size}` or `null`), and `content` capped at 9 000 characters.

```console
$ curl -s http://127.0.0.1:8787/api/article/413
{"id":413,"title":"…","entities":[{"name":"TSMC","type":"ORG","category":"Organization"}],…,"thread":{"id":12,"title":"…","size":6}}
$ curl -s http://127.0.0.1:8787/api/article/9999
{"error":"article not found"}
$ curl -s http://127.0.0.1:8787/api/article/abc
{"error":"article id must be a number"}
```

## GET /api/search?q=&limit=&hours=&iso=

Full-text search over the archive, newest first (never by FTS rank — a rank sort aborts on a
half-rebuilt index).

`q` is required. `limit` 1-100 (default 25), `hours` 1-720 (default `window_hours`), `iso`
optional country filter.

Response: `{"q": str, "count": int, "articles": [{id, title, summary, published, iso, url}]}`

```console
$ curl -s "http://127.0.0.1:8787/api/search?q=semiconductor&limit=3"
{"q":"semiconductor","count":3,"articles":[{"id":412,"title":"…","summary":"…","published":"2026-11-21T08:10:00Z","iso":"TWN","url":"https://…"}]}
$ curl -s "http://127.0.0.1:8787/api/search"
{"error":"q is required"}
```

## GET /api/threads?limit=

`{"threads": [{id, title, size, updated, sources, isos}]}` — `sources` and `isos` are
comma-joined strings. `limit` 1-100, default 40.

## GET /api/thread/{id}

`{"articles": [{id, title, published, iso, summary, source, url}]}` — may be empty.

## GET /api/sources

`{"sources": [{id, name, url, rss, country, iso, lat, lon, lang, active, last_ok, last_error, n,
kind, topic_id}]}` — the active rows, ordered by kind and name.

## GET /api/briefing

`{"briefing": {at, hours, articles, text}|null, "state": str}` — the cached digest. `briefing` is
`null` until one exists or when the cached one is older than 180 minutes; `state` is the last
value the digest job wrote (empty string when it never ran).

## GET /api/timeline?days=&entity=

`{"count": int, "articles": [{id, title, published, iso, lat, lon, source, thread_id}]}`, newest
first and oldest last, thinned evenly to at most 400 markers over the window. `days` 1-90, default
7. With `entity`, only that entity's articles are drawn.

## GET /api/graph?entity=&depth=&limit=

Entity graph around one entity. `entity=__top__` returns the most mentioned entities; an unknown
entity returns `{"nodes": [], "edges": [], "center": null}` rather than an error. `depth` 1-4
(default 2), `limit` 1-200 (default 120).

`{"nodes": [{id, name, type, count, category}], "edges": [{source, target, weight, label}],
"center": {entity}|null}`

```console
$ curl -s "http://127.0.0.1:8787/api/graph?entity=TSMC&depth=2&limit=40"
{"nodes":[{"id":88,"name":"TSMC","type":"ORG","count":21,"category":"Organization"}],"edges":[],"center":{…}}
```

## GET /api/graph/articles?topic=&hours=&limit=

Article-to-article link graph of one window (`topic` by id or name, `hours`, `limit` 1-200).

`{"nodes": [{id, title, published, iso, topics, label, type: "ARTICLE", count: 1}], "edges":
[{source, target, weight, label, why, score}], "center": null, "mode": "articles"}`

## GET /api/edge?source=&target=

Why two entities are linked. Both ids are required (`400 source and target required`).

`{"a": {entity}|null, "b": {entity}|null, "weight": num, "label": str, "articles": [{id, title,
published, place, source}]}`

## GET /api/related/{id}

`{"related": [{id, title, published, iso, url, shared, score, why, source}]}` — the link graph
around one article (at most 12), from `atlas-graph`.

## GET /api/entity?name=

`{"entity": {id, name, type, count, …}, "category": str|null, "articles": [{id, title, published,
iso, source}], "neighbors": [{id, name, type, count, weight, label}], "aliases": [str]}`

The name is matched exactly, then as an alias, then as a substring. Unknown:
`404 {"error": "entity not found: …"}`.

## GET /api/entity/{id}

The same profile addressed by id; a missing id is `404 {"error": "entity not found: <id>"}`.

## GET /api/entities?q=&limit=

`{"entities": [{id, name, type, count, category_id, category}]}`. Without `q` only entities seen
in the current window are listed. `limit` 1-60, default 30.

## GET /api/categories

`{"categories": [{id, name, kind, description, created, n_entities, n_articles}]}`

## GET /api/category?name=&hours=

`{"category": {id, name, kind, description, created}, "entities": [{name, type, count}],
"articles": [{id, title, published, iso, source}]}` — at most 20 of each inside `hours`.
Unknown name: `404 {"error": "category not found: …"}`.

## GET /api/map?hours=

The world view: `{"hours", "places": [{place, iso, prec, lat, lon, count, titles: [{id, title,
published}], country}], "by_iso": {"USA": 12}, "countries": [{iso, name, count}], "totals":
{reports, places}}`. Nothing is geocoded here: a place is what the collector stored, and an
article whose country is unknown keeps its ISO code as its label.

## GET /api/topics

`{"topics": [{id, name, keywords, exclude, min_matches, active, created, hits, last_hit,
n_articles, n_24h, n_feeds, keyword_list}], "summary": {topics, active, relevant_articles,
stored_articles, filtered_articles}}`

## GET /api/topics/suggestions

`{"suggestions": [{name, keywords}]}` — the curated starters (14 in `BUILTIN_SUGGESTIONS`) that
are not topics yet, at most 12.

## GET /api/monitors

`{"monitors": [{id, name, query, threshold, active, created, last_run, hits, n_alerts,
n_unseen}]}`

## GET /api/alerts?monitor=&hours=&limit=

`{"alerts": [{id, monitor_id, kind, article_id, url, title, relevance, summary, analysis,
entities, seen, created, monitor_name, monitor_query}]}` — `entities` is always an array (a
broken stored value answers `[]`). `monitor` matches the monitor's name or query (`LIKE`),
`hours` 1-720 (default 48), `limit` 1-200 (default 50).

## GET /api/tags?limit=

`{"tags": [{tag, n}]}` — the tag cloud over the current window, most used first. `limit` 1-80,
default 30. The tags are the ones the local model wrote through `tag_article`.

## GET /api/research

State of the research run: `{"state": "idle"|"running", "started": iso|null, "result": …}`.
`result` is `null` while nothing has finished, otherwise `{ok, text, stored, steps, error}`. Poll
it while `state == "running"`.

```console
$ curl -s http://127.0.0.1:8787/api/research
{"state":"idle","started":"2026-11-21T09:25:29Z","result":null}
```

---

# Write endpoints

All take a JSON object body; an empty body is treated as `{}`. A body that is not JSON is
`400 {"error": "request body must be valid JSON"}`, a JSON value that is not an object is
`400 {"error": "request body must be a JSON object"}`, a body over 1 MiB is `413`, a chunked body
is `411`, and a `Content-Type` other than `application/json` or `text/plain` is `415`.

| Endpoint | Body | Response |
| --- | --- | --- |
| `POST /api/collect` | - | `{"started": true, "at": iso}` (+ the pass's own state, `busy` when one was already running) — one research pass per active topic |
| `POST /api/analyze/{id}` | - | `{"started": true, "article": id, "done": true, "analysis": {…}}` — one model call, answered when it is done; a missing article is `404` |
| `POST /api/read` | `{n?}` (1-120, default 12) | `{"started": true, "queued": int, "pending": int, "reader": {…}}` — bounded and resumable |
| `POST /api/research` | `{topic?}` (max 120 characters) | `{"state": "running"}` (plus `"busy": true` when a run already holds the claim) |
| `POST /api/briefing` | `{hours?}` (1-168, default 24) | `{"started": true, "hours": int, "briefing": {…}}` — one model call, answered when the digest is written |
| `POST /api/monitors/run` | - | `{"started": true}` (+ the check's own report) — one check per active monitor |
| `POST /api/topics/{id}/rescan` | - | `{"started": true, "job": {…}}` — re-matches the archive against the topics |
| `POST /api/topics` | `{name, keywords, min_matches?, exclude?}` | `{"id": int, "ok": true}` (+ `collect` or `warning`) |
| `POST /api/topics/{id}` | any of `{name, keywords, exclude, min_matches, active}` | `{"ok": true}` |
| `POST /api/topics/{id}/toggle` | - | `{"ok": true, "active": 0\|1}`; an unknown id is `404 {"error":"not found"}` |
| `DELETE /api/topics/{id}` | - | `{"ok": true}` |
| `POST /api/monitors` | `{name, query, threshold?}` | `{"id": int}` |
| `POST /api/monitors/{id}/toggle` | - | `{"ok": true}` |
| `DELETE /api/monitors/{id}` | - | `{"ok": true}` — the monitor, its alerts and its seen-marks |
| `POST /api/alerts/seen` | - | `{"ok": true}` — marks every alert read |
| `POST /api/settings/collect-base` | `{enabled: bool}` | `{"ok": true, "enabled": bool}` (+ `collect` when it was turned on, or `warning` when the pass could not start) |

Topic and monitor refusals use the original wording: `400 topic name required`,
`400 at least one keyword required`, `400 a topic named '…' already exists`.

The long jobs (`collect`, `read`, `research`, `monitors/run`, `topics/{id}/rescan`) answer when
the work has been started or has already finished a bounded slice; poll `/api/status`,
`/api/research` or `GET /api/briefing` for the result. `analyze` and `briefing` are one model call
each and answer with their result. `/api/chat`, `/api/collect`, `/api/read` and `/api/research`
are rate-limited: 10 per minute and one running at a time, per endpoint, with `Retry-After` on a
`429`.

```console
$ curl -s -X POST http://127.0.0.1:8787/api/topics \
    -H 'Content-Type: application/json' \
    -d '{"name":"Energy","keywords":"grid, LNG, pipeline","min_matches":2}'
{"id":2,"ok":true}
```

## POST /api/chat (SSE)

Streams the assistant answer as Server-Sent Events, token by token. Request body:
`{"messages": [{"role": "user", "content": "…"}]}`. The stream is `text/event-stream;
charset=utf-8`, `Cache-Control: no-store`, one JSON object per `data:` line, nothing buffered.

| frame | meaning |
| --- | --- |
| `{"type": "start"}` | the turn began; the status line is already 200 |
| `{"type": "delta", "text": "…", "content": "…"}` | one token, as the model produced it — the fragment is under **both** names |
| `{"type": "tool_start", "name": …, "args": {…}, "step": 1}` | the model called one ATLAS tool |
| `{"type": "tool", "name": …, "args": {…}, "result": {…}, "error": …, "repeated": bool, "step": 1}` | what that tool answered |
| `{"type": "final", "content": "…"}` | the answer (the agent loop's own `done` event) |
| `{"type": "error", "message": "…"}` | the run failed |
| `{"type": "done"}` | the server's terminator; always the last frame |

A `delta` frame carries the fragment under both key names clients read: `text` and `content`. The
payload is one object either way, so a reader that knows one field never sees a frame it cannot
paint. The frame type is `delta` only — the panel paints `delta` and `token` the same way, so
emitting both would paint one token twice.

Frames are sent as they arrive. The step and token budgets of the loop are published on
`/api/status` (`chat.max_steps`, `chat.total_token_budget`) and a client that disconnects cancels
the run. Only `user` and `assistant` messages with non-empty content are sent to the model (last
20, each capped at 4 000 characters).

```console
$ curl -sN -X POST http://127.0.0.1:8787/api/chat \
    -H 'Content-Type: application/json' \
    -d '{"messages":[{"role":"user","content":"What is moving today?"}]}'
data: {"type":"start"}

data: {"type":"delta","text":"Semiconductors","content":"Semiconductors"}

data: {"type":"tool_start","name":"stats","args":{},"step":2}

data: {"type":"final","content":"Semiconductors moved."}

data: {"type":"done"}
```

A body that is not a JSON object is a normal JSON `400` before the stream starts, and so is
`messages` that is neither a list nor falsy (`{"error": "messages must be a list"}`).

---

# Static files

`GET /` serves `public/index.html`. Every other path is resolved inside `public/` by
`static_files::static_target`: control bytes, backslashes, `..`, dotfiles, spaces, a trailing dot,
more than 16 path segments, an extension outside the table and any symlink on the way are refused
— as a JSON `404`, and before the handler runs. Only `GET` and `HEAD` reach a file; a `POST` to
`/index.html` is a `404`. `HEAD` answers the headers and no body.

The ten content types: `.html`, `.js`, `.css`, `.json`, `.geojson`, `.svg`, `.png`, `.ico`,
`.wasm`, `.woff2`. The frontend is `public/ui/` (wasm), so `/ui/loader.js`, `/ui/atlas_ui.js`,
`/ui/atlas_ui_bg.wasm` and `/ui/tiles/**` are all served from there; `/css/*`, `/style.css` and
`/vendor/*` come from the shell's own directory. A static file is capped at 8 MiB.

```console
$ curl -s -o /dev/null -w '%{http_code} %{content_type}\n' http://127.0.0.1:8787/
200 text/html; charset=utf-8
$ curl -s -o /dev/null -w '%{http_code} %{content_type}\n' http://127.0.0.1:8787/ui/loader.js
200 text/javascript; charset=utf-8
$ curl -s http://127.0.0.1:8787/nope.js
{"error":"not found"}
```

# Logging

`data/app.log` receives one line per request that failed or was slow — `METHOD PATH STATUS ms`,
nothing else, no body, no query string — and never a traceback:

```console
2026-11-21T09:50:54Z GET /api/article/999999999 404 0ms
2026-11-21T09:50:54Z GET /api/articles?q=… 500 2140ms
```

The line is built by `logline::prepare`: redacted (`atlas_guard::redact`), control bytes dropped (a
`\n` in a path cannot start a second line), collapsed to one line and capped at 400 characters. A
line that is nothing but a secret is not written at all. `data/app.log` has exactly one writer,
and `atlas-guard/tests/audit_logs.rs` fails the build if a second one appears.
