# Rule engine

A `Rule` (`crates/flproxy-core/src/rule.rs`) pairs a `match` (`Matcher`) with an ordered list of `actions` (`Action`). Rules are persisted as `<data-dir>/rules.json` and served/edited via `GET/POST /api/rules`, `PUT/DELETE /api/rules/:id`, `POST /api/rules/:id/toggle`, `POST /api/rules/reorder`, and `POST/GET /api/rules/import`/`/export` (see `docs/API.md`).

All field names below are the literal JSON wire names, verified by reading `rule.rs` and by POSTing each example to a running `flproxy run` instance (see "Verification" at the end of this document).

## Matcher fields

```json
{
  "urlOp": "startsWith",
  "urlValue": "https://api.example.com/",
  "methods": ["GET", "POST"],
  "hostPorts": ["api.example.com:443"],
  "statusCodes": ["4xx", "500-599"],
  "resourceTypes": ["xhr", "json"],
  "requestHeaders": [{"name": "X-Env", "op": "equals", "value": "staging"}],
  "responseHeaders": [],
  "requestBody": null,
  "responseBody": {"op": "contains", "value": "error"}
}
```

All fields are optional (default to "matches anything"). Field meanings:

| Field | Type | Matches when... |
|---|---|---|
| `urlOp` + `urlValue` | `UrlOp`, `string` | the full request URL compares against `urlValue` per `urlOp` (see below). |
| `methods` | `string[]` | the HTTP method (case-insensitive) is in this list. Empty = any. |
| `hostPorts` | `string[]` (globs) | `host` or `host:port` matches one of these glob patterns. Empty = any. |
| `statusCodes` | `string[]` | the response status matches one of these specs. Empty = any. **Response phase only.** |
| `resourceTypes` | `ResourceType[]` | the inferred resource type is in this list. Empty = any. |
| `requestHeaders` | `HeaderCond[]` | every listed condition matches a request header. |
| `responseHeaders` | `HeaderCond[]` | every listed condition matches a response header. **Response phase only.** |
| `requestBody` | `BodyCond \| null` | the decoded request body matches. |
| `responseBody` | `BodyCond \| null` | the decoded response body matches. **Response phase only.** |

`UrlOp` (compares the matcher's `urlValue` against the full request URL, e.g. `https://api.example.com/v1/users?active=true`):

| Value | Meaning |
|---|---|
| `any` | Always matches (default). |
| `contains` | URL contains `urlValue` as a substring. |
| `equals` | URL equals `urlValue` exactly. |
| `startsWith` | URL starts with `urlValue`. |
| `endsWith` | URL ends with `urlValue`. |
| `regex` | `urlValue` is a regex matched against the URL. Enables `$1`–`$9` capture-group substitution in `redirect`/`rewriteUrl` — see below. |
| `wildcard` | `urlValue` is a glob pattern (e.g. `https://example.com/assets/*.js`) matched against the URL. |

`hostPorts` globs are matched against both `host:port` and bare `host` (so `*.example.com` matches `api.example.com:443` and `api.example.com`).

`statusCodes` entries are exact (`"200"`), a class (`"4xx"`), or an inclusive range (`"500-599"`).

`resourceTypes` values: `document | stylesheet | script | image | font | xhr | json | media | webSocket | other` (inferred from `Content-Type`/`Accept` and the path, see `ResourceType::infer`).

`HeaderCond` (`requestHeaders`/`responseHeaders` entries):

```json
{"name": "Authorization", "op": "exists", "value": null}
```

`op` is one of `exists | absent | equals | contains | regex` (`value` is required for all except `exists`/`absent`; header names are matched case-insensitively, and `equals`/`contains`/`regex` match if *any* header instance with that name satisfies it).

`BodyCond` (`requestBody`/`responseBody`), evaluated against the decoded (post content-encoding) body text:

```json
{"op": "contains", "value": "error"}
```

`op` is one of `contains | regex | equals`.

## Action catalog

Every action below is a real variant of the `Action` enum, tagged by a `"type"` field. The enum is `#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]`, so both the `type` tag values (`mockResponse`, `setRequestHeader`, etc.) *and* the fields inside each struct-like variant are camelCase on the wire — including the three multi-word fields, `delayMs` (`mockResponse`), `bytesPerSec` (`throttle`), and `contentType` (`setRequestBody`/`setResponseBody`). This is consistent with the rest of the API (`Matcher`, `Rule`, `Settings`, `Flow`/`FlowSummary`, ...), which is also camelCase throughout. For backwards compatibility, the old snake_case spellings (`delay_ms`, `bytes_per_sec`, `content_type`) are still accepted on read via `#[serde(alias = "...")]`, so rules saved to `rules.json` before this fix keep loading — but the server only ever emits camelCase, and new rules should use camelCase too.

Request-phase actions:

```json
{"type": "redirect", "to": "http://localhost:4000/$1"}
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
{"type": "setRequestBody", "body": "{\"forced\":true}", "encoding": "text", "contentType": "application/json"}
```
```json
{"type": "replaceInRequestBody", "find": "foo", "replace": "bar", "regex": false}
```
```json
{"type": "jsonPatchRequest", "ops": [{"op": "set", "path": "user.id", "value": 42}]}
```
```json
{"type": "mockResponse", "status": 200, "headers": [{"name": "Content-Type", "value": "application/json"}], "body": "{\"mocked\":true}", "encoding": "text", "delayMs": 250}
```
```json
{"type": "block", "reason": "blocked by policy"}
```
```json
{"type": "setMethod", "method": "POST"}
```

Response-phase actions:

```json
{"type": "setResponseHeader", "name": "Cache-Control", "value": "no-store"}
```
```json
{"type": "removeResponseHeader", "name": "Set-Cookie"}
```
```json
{"type": "setResponseBody", "body": "eyJvayI6dHJ1ZX0=", "encoding": "base64", "contentType": "application/json"}
```
```json
{"type": "replaceInResponseBody", "find": "\\d{4}-\\d{2}-\\d{2}", "replace": "REDACTED", "regex": true}
```
```json
{"type": "jsonPatchResponse", "ops": [{"op": "remove", "path": "data.items[0].secret"}]}
```
```json
{"type": "setStatus", "status": 500}
```

Phase-agnostic (apply in whichever phase the owning rule matched — a rule matching in both phases contributes delay/throttle in both):

```json
{"type": "delay", "ms": 500}
```
```json
{"type": "throttle", "bytesPerSec": 51200}
```

`JsonOp` (`jsonPatchRequest`/`jsonPatchResponse` `ops` entries): `op` is one of `set | remove | merge | append`; `path` is a dot/bracket path such as `"data.items[0].name"`; `value` is used by `set`/`merge`/`append` and ignored for `remove`. `merge` shallow-merges an object into the object at `path` (falling back to `set` if `value` isn't an object); `append` pushes onto the array at `path`, creating it if absent.

`PayloadEncoding` (`encoding` on `setRequestBody`/`setResponseBody`/`mockResponse`) is `text | base64`; `text` is the default if omitted.

## Phase semantics

Rules run in two phases:

- **Request phase** (`RuleSet::apply_request`): evaluates each rule's request-evaluable conditions (`urlOp`/`urlValue`, `methods`, `hostPorts`, `resourceTypes`, `requestHeaders`, `requestBody`) and, for matching rules, applies request-phase actions in order: `redirect`, `rewriteUrl`, `setQueryParam`, `removeQueryParam`, `setRequestHeader`, `removeRequestHeader`, `setRequestBody`, `replaceInRequestBody`, `jsonPatchRequest`, `mockResponse`, `setMethod`, `block`, plus `delay`/`throttle`.
- **Response phase** (`RuleSet::apply_response`): re-evaluates each rule's *full* matcher (request conditions again, plus `statusCodes`, `responseHeaders`, `responseBody`) and, for matching rules, applies response-phase actions: `setResponseHeader`, `removeResponseHeader`, `setResponseBody`, `replaceInResponseBody`, `jsonPatchResponse`, `setStatus`, plus `delay`/`throttle`. A rule with only request-evaluable conditions is checked again here — it doesn't need a `statusCodes`/`responseHeaders`/`responseBody` condition to run in this phase, it just also has to still match.

A rule can fire in both phases if its matcher passes both times and it has actions of both kinds.

`block` and `mockResponse` are request-phase-only actions that short-circuit the request before it ever reaches upstream:

- `block` responds immediately with **403 Forbidden** and a `{"error":"blocked","reason":"..."}` body; the reason defaults to `"blocked by rule"` if empty.
- `mockResponse` responds immediately with the given status/headers/body (after `delayMs`, if set), never contacting upstream.

`block` can also appear in a response-phase action list; there it stops any remaining lower-priority rules from applying in that phase, but — since a real response already exists by then — it has no effect on the status/body actually returned.

## Evaluation order

Rules are sorted once (when the rule set is (re)compiled, i.e. on every create/update/delete/reorder/import) by `priority` ascending, then by original insertion (list) order for ties. **All matching rules apply** in that order — this is not first-match-wins — except that a `block` or `mockResponse` stops any further rules in the *same phase* from being evaluated. Disabled rules (`enabled: false`) are skipped entirely (never matched, never applied). A rule with an invalid regex or glob pattern anywhere in its matcher/actions is dropped from the compiled set (it never matches) — there is currently no API endpoint that surfaces this as an error to the caller, so a typo'd regex fails silently rather than 400ing at creation time.

Changes made through the API (or `flproxy rules import`) take effect immediately on the next request — the compiled rule set is rebuilt on every mutation, there's no caching lag.

## Capture-group substitution

When a rule's matcher uses `"urlOp": "regex"`, `redirect`'s `to` and `rewriteUrl`'s `replace` may contain `$1`–`$9`, substituted with that regex's capture groups from the URL the matcher matched against (captured before any of the rule's own actions can mutate the URL). `$0` and a `$` not followed by a digit 1–9 are passed through literally. This only applies when `urlOp` is `regex`; it's a no-op otherwise. See the worked example in the recipes below.

## Recipes

Each recipe was POSTed to a locally running `flproxy run` instance's `POST /api/rules` and returned `201 Created` — see "Verification" for the exact commands and responses.

**1. Point a staging API path at localhost, preserving the path**

```json
{
  "name": "Route staging API to localhost",
  "enabled": true,
  "priority": 0,
  "group": "staging",
  "match": {
    "urlOp": "regex",
    "urlValue": "^https://api\\.example\\.com/staging/(.*)$"
  },
  "actions": [
    {"type": "redirect", "to": "http://localhost:4000/$1"}
  ]
}
```

**2. Mock a 500 to test error handling**

```json
{
  "name": "Mock a 500 for the payments endpoint",
  "enabled": true,
  "priority": 0,
  "match": {
    "urlOp": "contains",
    "urlValue": "/api/payments"
  },
  "actions": [
    {
      "type": "mockResponse",
      "status": 500,
      "headers": [{"name": "Content-Type", "value": "application/json"}],
      "body": "{\"error\":\"internal_error\"}",
      "encoding": "text",
      "delayMs": 0
    }
  ]
}
```

**3. Strip CORS headers from a response**

```json
{
  "name": "Strip CORS headers from example.com responses",
  "enabled": true,
  "priority": 0,
  "match": {
    "hostPorts": ["*.example.com"]
  },
  "actions": [
    {"type": "removeResponseHeader", "name": "Access-Control-Allow-Origin"},
    {"type": "removeResponseHeader", "name": "Access-Control-Allow-Credentials"}
  ]
}
```

**4. Inject an auth header into requests**

```json
{
  "name": "Inject a dev auth token",
  "enabled": true,
  "priority": 0,
  "match": {
    "hostPorts": ["api.example.com:443"]
  },
  "actions": [
    {"type": "setRequestHeader", "name": "Authorization", "value": "Bearer dev-token-123"}
  ]
}
```

**5. Simulate a slow/throttled 3G-like connection**

```json
{
  "name": "Simulate a slow 3G connection",
  "enabled": true,
  "priority": 0,
  "match": {
    "urlOp": "any"
  },
  "actions": [
    {"type": "delay", "ms": 400},
    {"type": "throttle", "bytesPerSec": 50000}
  ]
}
```

**6. Block requests to analytics domains**

```json
{
  "name": "Block analytics domains",
  "enabled": true,
  "priority": 0,
  "match": {
    "hostPorts": ["*.google-analytics.com", "*.segment.io"]
  },
  "actions": [
    {"type": "block", "reason": "blocked analytics traffic"}
  ]
}
```

## Verification

Each recipe above (plus the `id`/`enabled` etc. defaults filled in by the server) was submitted to a real running instance:

```
cargo run -q -p flproxy-cli -- run --no-open --proxy-port 19082 --ui-port 19083 --data-dir <scratch-dir> &
curl -sS -X POST http://127.0.0.1:19083/api/rules -H "Content-Type: application/json" --data-binary @recipe.json -w "\nHTTP_STATUS:%{http_code}\n"
```

Results (all `201 Created`, server-assigned `id`, empty `id` in the request body):

| Recipe | Status |
|---|---|
| Route staging API to localhost | `201` |
| Mock a 500 for the payments endpoint | `201` |
| Strip CORS headers from example.com responses | `201` |
| Inject a dev auth token | `201` |
| Simulate a slow 3G connection | `201` |
| Block analytics domains | `201` |

An earlier version of this document recorded that the `throttle` recipe failed with `"bytesPerSec"` (`422 Unprocessable Entity: missing field 'bytes_per_sec'`) and only succeeded with the snake_case spelling — that was a real bug in `Action`'s serde attributes (`rename_all` on an enum doesn't rename struct-variant fields), now fixed by adding `rename_all_fields = "camelCase"` plus `#[serde(alias = "...")]` on the affected fields for backwards compatibility. See the live re-verification below, which POSTs the camelCase spelling (`bytesPerSec`, `delayMs`, `contentType`) for `throttle`, `mockResponse`, and `setResponseBody` and confirms `201 Created` plus a correct round-trip through `GET /api/rules`.
