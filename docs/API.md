# flproxy API reference

This document specifies the JSON wire format and REST/WebSocket surface actually implemented by `flproxy-api` (`crates/flproxy-api/src/`), reconciled against the source and the crate's own test suite (`crates/flproxy-api/tests/rest_api.rs`, `tests/ws.rs`). Earlier drafts of this document described a few things `flproxy-core`/`flproxy-api` ended up implementing differently (noted inline below); this revision matches the real server.

All JSON uses `camelCase` field names, with no exceptions — including the fields inside `Action`/`ServerEvent`/`ClientCommand` struct-like variants (`delayMs`, `bytesPerSec`, `contentType`, `flowId`); see [`docs/RULES.md`](RULES.md) for the serde details. For backwards compatibility, rules persisted before this was fixed may still contain the old snake_case spellings on disk, and the server accepts both on read — but it only ever emits camelCase. All timestamps are milliseconds since the Unix epoch unless stated otherwise.

The server is `flproxy-api`'s `axum::Router` (`flproxy_api::router`), served by `flproxy run` on the configured UI port (default `9081`), or standalone with a `NoopReplay`/`StubCert` backend. There is **no authentication** on any of this — see the Security note in the top-level `README.md`.

---

## 1. Flow types

### FlowSummary

Lightweight record used in list views. Several fields are `null` until the flow reaches that stage (e.g. `status`/`durationMs` while `state` is still `pending`/`requesting`/`responding`).

```json
{
  "id": "6f2a9e2c-8b3a-4b1a-9c2a-1a2b3c4d5e6f",
  "seq": 42,
  "state": "complete",
  "startedAt": 1712345678901,
  "durationMs": 123,
  "method": "GET",
  "scheme": "https",
  "host": "api.example.com",
  "port": 443,
  "path": "/v1/users?active=true",
  "url": "https://api.example.com/v1/users?active=true",
  "httpVersion": "HTTP/1.1",
  "status": 200,
  "statusText": "OK",
  "mimeType": "application/json",
  "resourceType": "xhr",
  "requestSize": 0,
  "responseSize": 1834,
  "clientAddr": "192.168.1.42:53211",
  "matchedRules": ["b1e2c3d4-...", "a9f8e7d6-..."],
  "modified": true,
  "error": null,
  "websocket": false,
  "fromCache": false
}
```

`state` is one of: `pending | requesting | responding | complete | error`.

`resourceType` is one of: `document | stylesheet | script | image | font | xhr | json | media | webSocket | other`.

`fromCache` is true only when the response was served locally by a `mockResponse` rule action (never contacted upstream) — the name is a holdover, it has nothing to do with HTTP caching.

### Flow (detail)

`Flow` flattens `FlowSummary` (via `#[serde(flatten)]`) and adds request/response bodies, timing, and (only when `modified: true`) the pre-rule original request/response.

```json
{
  "id": "6f2a9e2c-8b3a-4b1a-9c2a-1a2b3c4d5e6f",
  "seq": 42,
  "state": "complete",
  "startedAt": 1712345678901,
  "durationMs": 123,
  "method": "GET",
  "scheme": "https",
  "host": "api.example.com",
  "port": 443,
  "path": "/v1/users?active=true",
  "url": "https://api.example.com/v1/users?active=true",
  "httpVersion": "HTTP/1.1",
  "status": 200,
  "statusText": "OK",
  "mimeType": "application/json",
  "resourceType": "xhr",
  "requestSize": 0,
  "responseSize": 1834,
  "clientAddr": "192.168.1.42:53211",
  "matchedRules": ["b1e2c3d4-..."],
  "modified": true,
  "error": null,
  "websocket": false,
  "fromCache": false,

  "request": {
    "method": "GET",
    "url": "https://api.example.com/v1/users?active=true",
    "httpVersion": "HTTP/1.1",
    "headers": [
      {"name": "Host", "value": "api.example.com"},
      {"name": "Accept", "value": "application/json"}
    ],
    "body": {"kind": "none", "data": "", "size": 0, "truncated": false, "encoding": null},
    "query": [{"name": "active", "value": "true"}]
  },
  "response": {
    "status": 200,
    "statusText": "OK",
    "httpVersion": "HTTP/1.1",
    "headers": [
      {"name": "Content-Type", "value": "application/json"},
      {"name": "Content-Encoding", "value": "gzip"}
    ],
    "body": {
      "kind": "text",
      "data": "{\"users\":[]}",
      "size": 12,
      "truncated": false,
      "encoding": "gzip"
    }
  },
  "originalRequest": null,
  "originalResponse": {
    "status": 404,
    "statusText": "Not Found",
    "httpVersion": "HTTP/1.1",
    "headers": [{"name": "Content-Type", "value": "text/plain"}],
    "body": {"kind": "text", "data": "not found", "size": 9, "truncated": false, "encoding": null}
  },

  "timings": {
    "blocked": 0.5,
    "dns": -1,
    "connect": 12.0,
    "ssl": 45.0,
    "send": 0.1,
    "wait": 60.0,
    "receive": 5.4
  },
  "wsMessages": [],
  "serverAddr": "93.184.216.34:443",
  "tls": {
    "version": "TLSv1.3",
    "cipherSuite": "TLS13_AES_128_GCM_SHA256",
    "alpn": "h2",
    "sni": "api.example.com",
    "peerCertSubject": null,
    "peerCertIssuer": null,
    "notBefore": null,
    "notAfter": null
  }
}
```

`body.kind` is one of `text | base64 | none | truncated`. `size` is always the full decoded body size, even when `truncated: true`.

`request`/`response` are `null` until that half of the flow has actually been received; `originalRequest`/`originalResponse` stay `null` unless `modified` is true. `tls` is `null` for plain HTTP flows. `peerCert*`/`notBefore`/`notAfter` on `tls` are currently always `null` in practice — flproxy's hand-rolled ClientHello parser only extracts SNI/ALPN, not the peer certificate fields (`connect.rs`'s `build_tls_info`).

### WsMessage

```json
{"direction": "send", "opcode": "text", "timestamp": 1712345678901, "data": "hello", "size": 5}
```

`direction` is `send` (client -> server) or `recv`. `opcode` is `text | binary | close | ping | pong`; `data` is UTF-8 text for `text` frames and base64 for `binary` frames (empty for the rest).

---

## 2. Rule types

### Rule

```json
{
  "id": "b1e2c3d4-1111-2222-3333-444455556666",
  "name": "Redirect staging API",
  "enabled": true,
  "priority": 0,
  "group": "staging",
  "notes": "Route staging traffic to localhost",
  "match": {
    "urlOp": "startsWith",
    "urlValue": "https://api.example.com/",
    "methods": ["GET", "POST"],
    "hostPorts": ["api.example.com:443"],
    "statusCodes": ["4xx", "500-599"],
    "resourceTypes": ["xhr", "json"],
    "requestHeaders": [
      {"name": "X-Env", "op": "equals", "value": "staging"}
    ],
    "responseHeaders": [],
    "requestBody": null,
    "responseBody": {"op": "contains", "value": "error"}
  },
  "actions": [
    {"type": "setRequestHeader", "name": "X-Debug", "value": "1"}
  ]
}
```

`urlOp`: `any | contains | equals | startsWith | endsWith | regex | wildcard`.
`match.requestHeaders[].op` / `responseHeaders[].op`: `exists | absent | equals | contains | regex`.
`requestBody.op` / `responseBody.op`: `contains | regex | equals`.

Only `urlOp`/`urlValue`, `methods`, `hostPorts`, `resourceTypes`, `requestHeaders`, `requestBody` are evaluated in the request phase. `statusCodes`, `responseHeaders`, `responseBody` are only evaluated in the response phase (a full match, request conditions included, is required). See `docs/RULES.md` for the full semantics, evaluation order, and worked recipes.

### Action — one example per variant

```json
{"type": "redirect", "to": "https://staging.example.com/$1"}
```
```json
{"type": "rewriteUrl", "find": "/v1/", "replace": "/v2/", "regex": false}
```
```json
{"type": "setQueryParam", "name": "debug", "value": "1"}
```
```json
{"type": "removeQueryParam", "name": "session"}
```
```json
{"type": "setRequestHeader", "name": "Authorization", "value": "Bearer xyz"}
```
```json
{"type": "removeRequestHeader", "name": "If-None-Match"}
```
```json
{"type": "setResponseHeader", "name": "Cache-Control", "value": "no-store"}
```
```json
{"type": "removeResponseHeader", "name": "Set-Cookie"}
```
```json
{"type": "setRequestBody", "body": "{\"forced\":true}", "encoding": "text", "contentType": "application/json"}
```
```json
{"type": "setResponseBody", "body": "eyJvayI6dHJ1ZX0=", "encoding": "base64", "contentType": "application/json"}
```
```json
{"type": "replaceInRequestBody", "find": "foo", "replace": "bar", "regex": false}
```
```json
{"type": "replaceInResponseBody", "find": "\\d{4}-\\d{2}-\\d{2}", "replace": "REDACTED", "regex": true}
```
```json
{"type": "jsonPatchRequest", "ops": [{"op": "set", "path": "user.id", "value": 42}]}
```
```json
{"type": "jsonPatchResponse", "ops": [{"op": "remove", "path": "data.items[0].secret"}]}
```
```json
{
  "type": "mockResponse",
  "status": 200,
  "headers": [{"name": "Content-Type", "value": "application/json"}],
  "body": "{\"mocked\":true}",
  "encoding": "text",
  "delayMs": 250
}
```
```json
{"type": "setStatus", "status": 500}
```
```json
{"type": "block", "reason": "blocked by policy"}
```
```json
{"type": "delay", "ms": 500}
```
```json
{"type": "throttle", "bytesPerSec": 51200}
```
```json
{"type": "setMethod", "method": "POST"}
```

**Field-naming note:** the `Action` enum is tagged with `#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]`, so the `"type"` discriminant (`mockResponse`, `setRequestHeader`, ...) *and* every field inside each variant are camelCase, including `delayMs` (`mockResponse`), `bytesPerSec` (`throttle`), and `contentType` (`setRequestBody`/`setResponseBody`). An earlier revision of this document recorded those three fields as snake_case on the wire (`delay_ms`/`bytes_per_sec`/`content_type`) and POSTing the camelCase spelling as failing with `422 Unprocessable Entity` — that was a genuine bug (`rename_all` on an enum only renames variant names, not struct-variant fields) which broke the web UI's rule editor (it always sent camelCase). It's now fixed: the server emits and expects camelCase for these fields. The old snake_case spellings are still accepted on deserialize (via `#[serde(alias = "...")]`) purely so rules already saved to `rules.json` before the fix keep loading; new rules should use camelCase.

`JsonOp.op` is one of `set | remove | merge | append`; `path` is a dot/bracket path such as `"data.items[0].name"`.
`PayloadEncoding` (`encoding` fields above) is `text | base64`.

---

## 3. Settings

```json
{
  "proxyPort": 9080,
  "uiPort": 9081,
  "bindAddr": "0.0.0.0",
  "maxFlows": 10000,
  "maxBodyBytes": 5242880,
  "interceptHttps": true,
  "passthroughHosts": ["*.bank.com"],
  "captureIncludeHosts": [],
  "captureExcludeHosts": ["*.ads.example.com"],
  "autoSystemProxy": false,
  "captureWebsockets": true,
  "theme": "dark",
  "upstreamProxy": null,
  "paused": false
}
```

See the settings table in the top-level `README.md` for defaults and descriptions of each field.

---

## 4. WebSocket messages

Path: `WS /api/ws`.

On connect, the server sends a `state` snapshot (the same shape as `GET /api/state`) followed by a `flows` batch of up to the 2000 most recent flows, then a live stream of events.

**Coalescing:** `flow`/`flows` events (i.e. per-flow create/update notifications) are buffered and flushed as at most one `flows` message per **50ms** window (deduped by flow id, last write wins within the window, sorted by `seq`) — this is what keeps a fast-capturing proxy from flooding a slow UI client. Every other event type is sent immediately, after first flushing any buffered flow updates (to preserve relative ordering). The server also pings every 30 seconds and drops the connection after 2 consecutive missed pongs.

**Lag/resync:** if a client falls behind the server's internal broadcast channel (capacity 4096) enough that the channel drops messages, the client receives a `notice` event (`{"type":"notice","level":"warning","message":"dropped <n> events"}`, where `<n>` is the number of events the broadcast channel reports as lost) immediately followed by a fresh `flows` batch of the most recent 2000 flows — the same resync payload sent on initial connect — so the client's flow list can catch back up without a full page reload.

### ServerEvent (server -> client), one example per variant

```json
{"type": "flow", "flow": { "...": "FlowSummary" }}
```
```json
{"type": "flows", "flows": [{ "...": "FlowSummary" }]}
```
```json
{"type": "flowDetail", "flow": { "...": "Flow" }}
```
```json
{"type": "wsMessage", "flowId": "6f2a9e2c-...", "message": {"direction": "recv", "opcode": "text", "timestamp": 1712345678901, "data": "pong", "size": 4}}
```
```json
{"type": "cleared"}
```
```json
{"type": "state", "state": { "...": "same shape as GET /api/state" }}
```
```json
{"type": "rulesChanged"}
```
```json
{"type": "settingsChanged", "settings": { "...": "Settings" }}
```
```json
{"type": "notice", "level": "warning", "message": "dropped 3 events"}
```

Note: `flproxy-api` never actually constructs a `flowDetail` event itself today (no route/handler emits `ServerEvent::FlowDetail`); it exists in the wire protocol and is handled the same as any other event by the coalescing logic, but nothing currently sends one.

### ClientCommand (client -> server), one example per variant

```json
{"type": "pause", "paused": true}
```
```json
{"type": "clear"}
```
```json
{"type": "subscribe", "filter": "host:api.example.com"}
```
```json
{"type": "ping"}
```

`pause` persists the new `paused` value to `settings.json` and broadcasts `settingsChanged` to all clients (not just the sender). `clear` clears the flow store and broadcasts `cleared`. `subscribe` is accepted but currently a no-op (no server-side filtering is implemented). `ping` gets a `{"type":"notice","level":"info","message":"pong"}` reply on the same socket. A malformed inbound frame is logged and ignored rather than closing the connection.

---

## 5. REST surface

All error responses share the shape `{"error": "<category>", "detail": "<message>"}`, where `category` is one of `bad_request` (400) | `not_found` (404) | `payload_too_large` (413) | `internal` (500) | `not_implemented` (501) | `bad_gateway` (502). Request bodies over 512 MiB are rejected globally (`413`); in practice only `POST /api/har/import` is expected to approach that. CORS is enabled only for the Vite dev-server origins (`http://localhost:5173`, `http://127.0.0.1:5173`) with any method/header — there's no wildcard `*` origin.

```
GET    /api/state
                     -> 200 { version, proxyPort, uiPort, capturing, paused,
                              flowCount, caFingerprint, uptimeSecs,
                              systemProxy: { enabled, platform, supported } }
                     `capturing` is `!paused` (kept as a separate field for
                     UI convenience). `platform` is one of "macos" /
                     "windows" / "linux" / "unknown"; `supported` is
                     whether this platform's system-proxy integration is
                     implemented at all (true only for macos/windows/linux).

GET    /api/flows?limit=&afterSeq=&q=&methods=&statusClass=&resourceTypes=&host=&onlyModified=
                     -> 200 { "flows": FlowSummary[] }
                     `limit` defaults to 2000, capped at 50000. `methods`
                     and `resourceTypes` are comma-separated. `statusClass`
                     is a single digit (e.g. `4` for 4xx). `onlyModified`
                     accepts "true"/"1"/"yes" (case-insensitive) as true.

GET    /api/flows/:id
                     -> 200 Flow
                     -> 404 if `:id` isn't a valid UUID, or is valid but
                        unknown

DELETE /api/flows
                     -> 204, clears the flow store and broadcasts `cleared`
                        over the WebSocket

POST   /api/flows/:id/replay
        body: optional edited RequestRecord as raw JSON (empty body ->
              replay the flow's own original request)
                     -> 200 { "id": "<new-flow-id>" }
                     -> 404 if `:id` doesn't exist
                     -> 501 if the configured ReplayHook can't replay (e.g.
                        `flproxy-api` running standalone with no proxy
                        backend attached; not reachable via `flproxy run`,
                        which always attaches a real backend)

GET    /api/rules
                     -> 200 { "rules": [Rule, ...] }   (persisted order)
                     Wrapped in an object (not a bare `Rule[]` array), for
                     symmetry with `GET /api/rules/export`; the web UI's
                     `listRules()` (`ui/src/lib/api.ts`) expects this shape.

POST   /api/rules
        body: Rule (server-assigns a UUID `id` if the given `id` is empty
              or the field is omitted from the JSON body entirely — the web
              UI's "new rule from template" flow omits it rather than
              sending `"id": ""`)
                     -> 201 Rule

PUT    /api/rules/:id
        body: Rule  (wholesale replacement)
                     -> 200 Rule
                     -> 404 if `:id` doesn't exist

DELETE /api/rules/:id
                     -> 204
                     -> 404 if `:id` doesn't exist

POST   /api/rules/:id/toggle
                     -> 200 Rule (with `enabled` flipped)
                     -> 404 if `:id` doesn't exist

POST   /api/rules/reorder
        body: {"ids": ["rule-id-1", "rule-id-2", ...]}
                     -> 204
                     rules not named in `ids` keep their relative order,
                     appended after the listed ones

POST   /api/rules/import
        body: {"rules": [Rule, ...], "replace": true}
                     -> 200 { "imported": <n> }
                     `replace` defaults to true if omitted. `true` replaces
                     the whole rule set; `false` merges by `id` (existing
                     rules kept, matching ids overwritten in place, new ids
                     appended). Broadcasts `rulesChanged`.

GET    /api/rules/export
                     -> 200 {"rules": [Rule, ...]}

GET    /api/settings
                     -> 200 Settings

PUT    /api/settings
        body: a *partial* Settings object (any subset of fields)
                     -> 200 Settings merged with "restartRequired": <bool>
                     `restartRequired` is true iff `proxyPort`, `uiPort`,
                     or `bindAddr` changed. The merged settings are always
                     persisted to `settings.json` and broadcast as
                     `settingsChanged`, regardless of `restartRequired`.

POST   /api/system-proxy
        body: {"enabled": true}
                     -> 200 { "enabled": <bool> }, configures the OS system
                        proxy to point at `127.0.0.1:<proxyPort>` (enabled)
                        or disables it, then echoes back the applied value
                        (not a bare `204` — the web UI's `setSystemProxy()`
                        in `ui/src/lib/api.ts` expects the body back)
                     -> 502 if the underlying OS command fails
                     Enabling snapshots the OS proxy configuration exactly
                     as it was beforehand (written to
                     `<data-dir>/sysproxy-state.json`), so a later disable
                     — via this same endpoint, `flproxy proxy off`, or
                     `flproxy run` shutting down — restores that prior
                     configuration rather than just turning the proxy off.
                     Disabling without ever having enabled it through one
                     of those tracked paths still just turns the proxy
                     off (there's no snapshot to restore).

GET    /api/setup
                     -> 200 {
                          "proxyHost": "192.168.1.10",
                          "proxyPort": 9080,
                          "lanAddresses": ["192.168.1.10", "10.0.0.5"],
                          "certUrl": "http://192.168.1.10:9081/cert/flproxy-ca.crt",
                          "caFingerprint": "SHA256:AB:CD:...",
                          "qrSvg": "<svg>...</svg>"
                        }
                     `proxyHost`/first of `lanAddresses` is best-effort:
                     private (RFC1918) addresses on a commonly-primary
                     interface (en0/wlan0/eth0) are preferred; falls back
                     to "127.0.0.1" if no LAN address is found. `qrSvg` is
                     an inline SVG QR code encoding `certUrl`. Note:
                     `certUrl` points at the `.crt` (DER) route, not `.pem`
                     as an earlier draft of this document showed.

GET    /api/har?ids=a,b
                     -> 200 HAR 1.2 document (export_har), served as a file
                        download (Content-Disposition: attachment). Omit
                        `ids` to export every captured flow.

POST   /api/har/import
        body: HAR 1.2 document
                     -> 200 { "imported": <n> }
                     -> 400 if the document doesn't parse as HAR
                     Imported flows get freshly assigned `seq` values (not
                     the HAR's own entry order) and are broadcast as a
                     `flows` event.

GET    /cert/flproxy-ca.pem
                     -> 200, the MITM root CA certificate, PEM-encoded,
                        `Content-Type: application/x-pem-file`

GET    /cert/flproxy-ca.crt
                     -> 200, the CA certificate, DER-encoded (despite the
                        `.crt` extension, the bytes are identical to
                        `.der`), `Content-Type: application/x-x509-ca-cert`
                        — the MIME type mobile OSes look for when
                        installing a certificate profile

GET    /cert/flproxy-ca.der
                     -> 200, same DER bytes as `.crt`,
                        `Content-Type: application/x-x509-ca-cert`

WS     /api/ws
                     -> upgrades to a WebSocket carrying ServerEvent/
                        ClientCommand messages as documented in section 4

GET    /*
                     -> any path not matched above falls back to serving
                        the built web UI (SPA-style: unknown paths get
                        `index.html`), or a minimal built-in placeholder
                        page if no UI build is available on disk/embedded.
                        An unmatched path under /api/* or /cert/* instead
                        gets a JSON 404 (`ApiError::NotFound`), not this
                        SPA fallback.
```
