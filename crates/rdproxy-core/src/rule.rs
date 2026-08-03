//! Rule model and matching/transformation engine.
//!
//! A [`Rule`] pairs a [`Matcher`] with a list of [`Action`]s. A [`RuleSet`]
//! compiles a collection of rules (pre-parsing regexes/globs so evaluation
//! is cheap) and applies them to requests and responses in two phases: see
//! [`RuleSet::apply_request`] and [`RuleSet::apply_response`].

use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::flow::{HeaderPair, ResourceType};

fn default_true() -> bool {
    true
}

fn default_200() -> u16 {
    200
}

/// How a [`Matcher`]'s `url_value` should be compared against a request URL.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UrlOp {
    /// Always matches, regardless of `url_value`.
    #[default]
    Any,
    /// The URL contains `url_value` as a substring.
    Contains,
    /// The URL is exactly equal to `url_value`.
    Equals,
    /// The URL starts with `url_value`.
    StartsWith,
    /// The URL ends with `url_value`.
    EndsWith,
    /// `url_value` is a regular expression matched against the URL.
    Regex,
    /// `url_value` is a glob pattern matched against the URL.
    Wildcard,
}

/// Comparison operator for a [`HeaderCond`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HeaderOp {
    /// A header with this name is present (any value).
    Exists,
    /// No header with this name is present.
    Absent,
    /// Some header with this name has a value exactly equal to `value`.
    Equals,
    /// Some header with this name has a value containing `value`.
    Contains,
    /// Some header with this name has a value matching the regex `value`.
    Regex,
}

/// A single header condition within a [`Matcher`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderCond {
    /// Header name, matched case-insensitively.
    pub name: String,
    /// Comparison operator.
    pub op: HeaderOp,
    /// Comparison value, required for all operators except `exists`/`absent`.
    pub value: Option<String>,
}

/// Comparison operator for a [`BodyCond`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BodyCondOp {
    /// The body contains `value` as a substring.
    Contains,
    /// `value` is a regex matched against the body.
    Regex,
    /// The body is exactly equal to `value`.
    Equals,
}

/// A body condition within a [`Matcher`], evaluated against the decoded
/// (post content-encoding) body text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyCond {
    /// Comparison operator.
    pub op: BodyCondOp,
    /// Comparison value.
    pub value: String,
}

/// The set of conditions a [`Rule`] must satisfy to apply.
///
/// Request-evaluable fields (`url_op`/`url_value`, `methods`, `host_ports`,
/// `resource_types`, `request_headers`, `request_body`) are checked in the
/// request phase. `status_codes`, `response_headers`, and `response_body`
/// are only checked in the response phase; see [`RuleSet::apply_request`]
/// and [`RuleSet::apply_response`] for the exact semantics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Matcher {
    /// Operator used to compare `url_value` against the request URL.
    #[serde(default)]
    pub url_op: UrlOp,
    /// Value compared against the request URL, per `url_op`.
    #[serde(default)]
    pub url_value: String,
    /// HTTP methods this rule applies to (case-insensitive). Empty = any.
    #[serde(default)]
    pub methods: Vec<String>,
    /// Glob patterns matched against `host` and `host:port`. Empty = any.
    #[serde(default)]
    pub host_ports: Vec<String>,
    /// Status code specs: exact (`"200"`), class (`"4xx"`), or range
    /// (`"500-599"`). Empty = any. Only checked in the response phase.
    #[serde(default)]
    pub status_codes: Vec<String>,
    /// Resource types this rule applies to. Empty = any.
    #[serde(default)]
    pub resource_types: Vec<ResourceType>,
    /// Request header conditions (all must match).
    #[serde(default)]
    pub request_headers: Vec<HeaderCond>,
    /// Response header conditions (all must match). Only checked in the
    /// response phase.
    #[serde(default)]
    pub response_headers: Vec<HeaderCond>,
    /// Optional request body condition.
    #[serde(default)]
    pub request_body: Option<BodyCond>,
    /// Optional response body condition. Only checked in the response phase.
    #[serde(default)]
    pub response_body: Option<BodyCond>,
}

/// How a payload string embedded in a [`Rule`] JSON document should be
/// interpreted before use.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PayloadEncoding {
    /// The payload string is used verbatim as UTF-8 text.
    #[default]
    Text,
    /// The payload string is base64 and must be decoded first.
    Base64,
}

/// The kind of mutation a [`JsonOp`] performs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum JsonOpKind {
    /// Sets the value at `path`, creating intermediate objects/arrays.
    Set,
    /// Removes the value at `path`, if present.
    Remove,
    /// Shallow-merges an object `value` into the object at `path`.
    Merge,
    /// Appends `value` to the array at `path`, creating it if absent.
    Append,
}

/// A single JSON Patch-like operation used by [`Action::JsonPatchRequest`]
/// and [`Action::JsonPatchResponse`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonOp {
    /// The mutation to perform.
    pub op: JsonOpKind,
    /// Dot/bracket path into the JSON document, e.g. `"data.items[0].name"`.
    pub path: String,
    /// Value used by `set`/`merge`/`append` (ignored for `remove`).
    pub value: Option<Value>,
}

/// A single rule transformation, applied when its owning [`Rule`]'s
/// [`Matcher`] matches.
///
/// Variants are split into request-phase and response-phase actions; see
/// the phase-classification doc comments on [`RuleSet::apply_request`] and
/// [`RuleSet::apply_response`] for exactly which variants run in which
/// phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Action {
    /// Redirects the request to `to`. May contain `$1`..`$9`, substituted
    /// from the matcher's URL regex capture groups (only meaningful when
    /// `Matcher::url_op` is [`UrlOp::Regex`]).
    Redirect {
        /// Destination URL, possibly containing `$1`..`$9` placeholders.
        to: String,
    },
    /// Rewrites the request URL via literal or regex find/replace.
    RewriteUrl {
        /// Substring or regex pattern to find.
        find: String,
        /// Replacement text. May contain `$1`..`$9` from the matcher's URL
        /// regex captures (see [`Action::Redirect`]).
        replace: String,
        /// When true, `find` is treated as a regex.
        #[serde(default)]
        regex: bool,
    },
    /// Sets (replacing any existing same-name value) a query parameter.
    SetQueryParam {
        /// Query parameter name.
        name: String,
        /// Query parameter value.
        value: String,
    },
    /// Removes a query parameter by name.
    RemoveQueryParam {
        /// Query parameter name.
        name: String,
    },
    /// Sets a request header, replacing any existing same-name headers.
    SetRequestHeader {
        /// Header name.
        name: String,
        /// Header value.
        value: String,
    },
    /// Removes all request headers with the given name.
    RemoveRequestHeader {
        /// Header name.
        name: String,
    },
    /// Sets a response header, replacing any existing same-name headers.
    SetResponseHeader {
        /// Header name.
        name: String,
        /// Header value.
        value: String,
    },
    /// Removes all response headers with the given name.
    RemoveResponseHeader {
        /// Header name.
        name: String,
    },
    /// Replaces the request body.
    SetRequestBody {
        /// New body content.
        body: String,
        /// How `body` is encoded.
        #[serde(default)]
        encoding: PayloadEncoding,
        /// Optional `Content-Type` header to set alongside the new body.
        #[serde(default, alias = "content_type")]
        content_type: Option<String>,
    },
    /// Replaces the response body.
    SetResponseBody {
        /// New body content.
        body: String,
        /// How `body` is encoded.
        #[serde(default)]
        encoding: PayloadEncoding,
        /// Optional `Content-Type` header to set alongside the new body.
        #[serde(default, alias = "content_type")]
        content_type: Option<String>,
    },
    /// Finds and replaces text within the decoded request body.
    ReplaceInRequestBody {
        /// Substring or regex pattern to find.
        find: String,
        /// Replacement text.
        replace: String,
        /// When true, `find` is treated as a regex.
        #[serde(default)]
        regex: bool,
    },
    /// Finds and replaces text within the decoded response body.
    ReplaceInResponseBody {
        /// Substring or regex pattern to find.
        find: String,
        /// Replacement text.
        replace: String,
        /// When true, `find` is treated as a regex.
        #[serde(default)]
        regex: bool,
    },
    /// Applies JSON patch operations to the decoded request body.
    JsonPatchRequest {
        /// Operations to apply, in order.
        ops: Vec<JsonOp>,
    },
    /// Applies JSON patch operations to the decoded response body.
    JsonPatchResponse {
        /// Operations to apply, in order.
        ops: Vec<JsonOp>,
    },
    /// Short-circuits the request phase, responding locally without
    /// contacting the upstream server.
    MockResponse {
        /// Mocked response status code.
        #[serde(default = "default_200")]
        status: u16,
        /// Mocked response headers.
        #[serde(default)]
        headers: Vec<HeaderPair>,
        /// Mocked response body.
        #[serde(default)]
        body: String,
        /// How `body` is encoded.
        #[serde(default)]
        encoding: PayloadEncoding,
        /// Artificial delay before responding, in milliseconds.
        #[serde(default, alias = "delay_ms")]
        delay_ms: u64,
    },
    /// Overrides the response status code.
    SetStatus {
        /// New status code.
        status: u16,
    },
    /// Blocks the request, short-circuiting the request phase.
    Block {
        /// Human-readable reason surfaced to the caller.
        #[serde(default)]
        reason: String,
    },
    /// Adds an artificial delay. Applies in whichever phase the owning rule
    /// matches in (see [`RuleSet::apply_request`]/[`RuleSet::apply_response`]);
    /// a rule that matches in both phases contributes delay in both.
    Delay {
        /// Delay in milliseconds.
        ms: u64,
    },
    /// Caps transfer speed. Applies in whichever phase the owning rule
    /// matches in, same as [`Action::Delay`]. When multiple `Throttle`
    /// actions apply within one phase, the most restrictive (smallest)
    /// value wins.
    Throttle {
        /// Maximum bytes per second.
        #[serde(alias = "bytes_per_sec")]
        bytes_per_sec: u64,
    },
    /// Overrides the HTTP method.
    SetMethod {
        /// New HTTP method.
        method: String,
    },
}

/// A named, orderable rule combining a [`Matcher`] with a list of
/// [`Action`]s to apply when it matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// Unique identifier (a UUID string). Defaults to an empty string when
    /// omitted from incoming JSON entirely (not just when present-but-empty)
    /// — `POST /api/rules`'s handler treats an empty id as "server, please
    /// assign one", and the web UI's "new rule from template" flow
    /// (`ui/src/pages/rules/templates.ts`) omits the field altogether rather
    /// than sending `"id": ""`.
    #[serde(default)]
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether this rule is active. Disabled rules are skipped entirely.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Evaluation order key; lower values run first.
    #[serde(default)]
    pub priority: i32,
    /// Optional UI grouping label.
    #[serde(default)]
    pub group: Option<String>,
    /// Optional free-form notes.
    #[serde(default)]
    pub notes: Option<String>,
    /// Matching conditions.
    #[serde(rename = "match")]
    pub matcher: Matcher,
    /// Actions to apply when `matcher` matches, in order.
    pub actions: Vec<Action>,
}

/// A rule that failed to compile (e.g. an invalid regex or glob pattern).
///
/// The offending rule is skipped entirely — it never matches or applies —
/// but compilation of the rest of the [`RuleSet`] proceeds normally.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleError {
    /// The id of the rule that failed to compile.
    pub rule_id: String,
    /// Human-readable description of the failure.
    pub message: String,
}

/// Parses a glob `pattern`, returning a human-readable error string on
/// failure. Shared by [`RuleSet`] (for `hostPorts`/wildcard URL matching)
/// and `settings.rs` (for passthrough/capture host globs).
pub(crate) fn build_glob(pattern: &str) -> Result<GlobMatcher, String> {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| e.to_string())
}

/// A parsed status-code condition entry.
#[derive(Debug, Clone, Copy)]
enum StatusMatcher {
    Exact(u16),
    Class(u8),
    Range(u16, u16),
}

impl StatusMatcher {
    fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if let Some(rest) = spec.strip_suffix("xx").or_else(|| spec.strip_suffix("XX")) {
            let class: u8 = rest.trim().parse().ok()?;
            return Some(StatusMatcher::Class(class));
        }
        if let Some((lo, hi)) = spec.split_once('-') {
            let lo: u16 = lo.trim().parse().ok()?;
            let hi: u16 = hi.trim().parse().ok()?;
            return Some(StatusMatcher::Range(lo, hi));
        }
        spec.parse().ok().map(StatusMatcher::Exact)
    }

    fn matches(&self, status: u16) -> bool {
        match self {
            StatusMatcher::Exact(v) => *v == status,
            StatusMatcher::Class(d) => (status / 100) as u8 == *d,
            StatusMatcher::Range(lo, hi) => status >= *lo && status <= *hi,
        }
    }
}

/// Precompiled regex/glob state for one [`Rule`], aligned by index with the
/// `rules`/`compiled` vectors inside [`RuleSet`].
struct CompiledEntry {
    url_regex: Option<Regex>,
    url_glob: Option<GlobMatcher>,
    host_globs: Vec<GlobMatcher>,
    status_matchers: Vec<StatusMatcher>,
    /// Aligned with `Matcher::request_headers`; `Some` only for `regex` conditions.
    req_header_regex: Vec<Option<Regex>>,
    /// Aligned with `Matcher::response_headers`; `Some` only for `regex` conditions.
    resp_header_regex: Vec<Option<Regex>>,
    req_body_regex: Option<Regex>,
    resp_body_regex: Option<Regex>,
    /// Aligned with `Rule::actions`; `Some` only for actions with `regex: true`.
    action_regex: Vec<Option<Regex>>,
}

fn compile_optional_regex(pattern: &str, errors: &mut Vec<String>) -> Option<Regex> {
    match Regex::new(pattern) {
        Ok(re) => Some(re),
        Err(e) => {
            errors.push(format!("invalid regex '{pattern}': {e}"));
            None
        }
    }
}

fn compile_header_regexes(conds: &[HeaderCond], errors: &mut Vec<String>) -> Vec<Option<Regex>> {
    conds
        .iter()
        .map(|c| {
            if c.op != HeaderOp::Regex {
                return None;
            }
            match &c.value {
                Some(v) => compile_optional_regex(v, errors),
                None => {
                    errors.push(format!(
                        "header condition '{}' uses regex op without a value",
                        c.name
                    ));
                    None
                }
            }
        })
        .collect()
}

fn compile_body_regex(cond: Option<&BodyCond>, errors: &mut Vec<String>) -> Option<Regex> {
    let cond = cond?;
    if cond.op == BodyCondOp::Regex {
        compile_optional_regex(&cond.value, errors)
    } else {
        None
    }
}

fn compile_rule(rule: &Rule) -> Result<CompiledEntry, Vec<String>> {
    let mut errors = Vec::new();
    let m = &rule.matcher;

    let url_regex = if m.url_op == UrlOp::Regex {
        match Regex::new(&m.url_value) {
            Ok(re) => Some(re),
            Err(e) => {
                errors.push(format!("invalid url regex: {e}"));
                None
            }
        }
    } else {
        None
    };

    let url_glob = if m.url_op == UrlOp::Wildcard {
        match build_glob(&m.url_value) {
            Ok(g) => Some(g),
            Err(e) => {
                errors.push(format!("invalid url glob: {e}"));
                None
            }
        }
    } else {
        None
    };

    let mut host_globs = Vec::with_capacity(m.host_ports.len());
    for pattern in &m.host_ports {
        match build_glob(pattern) {
            Ok(g) => host_globs.push(g),
            Err(e) => errors.push(format!("invalid host glob '{pattern}': {e}")),
        }
    }

    let status_matchers = m
        .status_codes
        .iter()
        .filter_map(|s| StatusMatcher::parse(s))
        .collect();

    let req_header_regex = compile_header_regexes(&m.request_headers, &mut errors);
    let resp_header_regex = compile_header_regexes(&m.response_headers, &mut errors);
    let req_body_regex = compile_body_regex(m.request_body.as_ref(), &mut errors);
    let resp_body_regex = compile_body_regex(m.response_body.as_ref(), &mut errors);

    let mut action_regex = Vec::with_capacity(rule.actions.len());
    for action in &rule.actions {
        let compiled = match action {
            Action::RewriteUrl { find, regex, .. } if *regex => {
                Some(compile_optional_regex(find, &mut errors))
            }
            Action::ReplaceInRequestBody { find, regex, .. } if *regex => {
                Some(compile_optional_regex(find, &mut errors))
            }
            Action::ReplaceInResponseBody { find, regex, .. } if *regex => {
                Some(compile_optional_regex(find, &mut errors))
            }
            _ => None,
        };
        action_regex.push(compiled.flatten());
    }

    if errors.is_empty() {
        Ok(CompiledEntry {
            url_regex,
            url_glob,
            host_globs,
            status_matchers,
            req_header_regex,
            resp_header_regex,
            req_body_regex,
            resp_body_regex,
            action_regex,
        })
    } else {
        Err(errors)
    }
}

fn default_port_for_scheme(scheme: &str) -> u16 {
    match scheme {
        "https" | "wss" => 443,
        "http" | "ws" => 80,
        _ => 0,
    }
}

fn matches_url(
    m: &Matcher,
    url_regex: Option<&Regex>,
    url_glob: Option<&GlobMatcher>,
    url: &str,
) -> bool {
    match m.url_op {
        UrlOp::Any => true,
        UrlOp::Contains => url.contains(m.url_value.as_str()),
        UrlOp::Equals => url == m.url_value,
        UrlOp::StartsWith => url.starts_with(m.url_value.as_str()),
        UrlOp::EndsWith => url.ends_with(m.url_value.as_str()),
        UrlOp::Regex => url_regex.map(|re| re.is_match(url)).unwrap_or(false),
        UrlOp::Wildcard => url_glob.map(|g| g.is_match(url)).unwrap_or(false),
    }
}

fn matches_host_port(m: &Matcher, host_globs: &[GlobMatcher], url: &url::Url) -> bool {
    if m.host_ports.is_empty() {
        return true;
    }
    let host = url.host_str().unwrap_or("");
    let port = url
        .port()
        .unwrap_or_else(|| default_port_for_scheme(url.scheme()));
    let host_port = format!("{host}:{port}");
    host_globs
        .iter()
        .any(|g| g.is_match(&host_port) || g.is_match(host))
}

fn header_lookup<'a>(headers: &'a [HeaderPair], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
        .collect()
}

fn header_cond_matches(cond: &HeaderCond, regex: Option<&Regex>, headers: &[HeaderPair]) -> bool {
    let values = header_lookup(headers, &cond.name);
    match cond.op {
        HeaderOp::Exists => !values.is_empty(),
        HeaderOp::Absent => values.is_empty(),
        HeaderOp::Equals => match &cond.value {
            Some(expected) => values.iter().any(|v| v == expected),
            None => false,
        },
        HeaderOp::Contains => match &cond.value {
            Some(expected) => values.iter().any(|v| v.contains(expected.as_str())),
            None => false,
        },
        HeaderOp::Regex => match regex {
            Some(re) => values.iter().any(|v| re.is_match(v)),
            None => false,
        },
    }
}

fn matches_all_header_conds(
    conds: &[HeaderCond],
    regexes: &[Option<Regex>],
    headers: &[HeaderPair],
) -> bool {
    conds
        .iter()
        .zip(regexes.iter())
        .all(|(c, r)| header_cond_matches(c, r.as_ref(), headers))
}

fn body_cond_matches(cond: Option<&BodyCond>, regex: Option<&Regex>, body: Option<&[u8]>) -> bool {
    let Some(cond) = cond else { return true };
    let owned;
    let text: &str = match body {
        Some(b) => {
            owned = String::from_utf8_lossy(b);
            owned.as_ref()
        }
        None => "",
    };
    match cond.op {
        BodyCondOp::Contains => text.contains(cond.value.as_str()),
        BodyCondOp::Equals => text == cond.value,
        BodyCondOp::Regex => regex.map(|re| re.is_match(text)).unwrap_or(false),
    }
}

#[allow(clippy::too_many_arguments)]
fn matches_request(
    rule: &Rule,
    compiled: &CompiledEntry,
    method: &str,
    url: &url::Url,
    headers: &[HeaderPair],
    body: Option<&[u8]>,
    resource_type: ResourceType,
) -> bool {
    let m = &rule.matcher;
    matches_url(
        m,
        compiled.url_regex.as_ref(),
        compiled.url_glob.as_ref(),
        url.as_str(),
    ) && (m.methods.is_empty() || m.methods.iter().any(|x| x.eq_ignore_ascii_case(method)))
        && matches_host_port(m, &compiled.host_globs, url)
        && (m.resource_types.is_empty() || m.resource_types.contains(&resource_type))
        && matches_all_header_conds(&m.request_headers, &compiled.req_header_regex, headers)
        && body_cond_matches(
            m.request_body.as_ref(),
            compiled.req_body_regex.as_ref(),
            body,
        )
}

#[allow(clippy::too_many_arguments)]
fn matches_response(
    rule: &Rule,
    compiled: &CompiledEntry,
    method: &str,
    url: &url::Url,
    headers: &[HeaderPair],
    body: Option<&[u8]>,
    resource_type: ResourceType,
    status: u16,
    resp_headers: &[HeaderPair],
    resp_body: Option<&[u8]>,
) -> bool {
    matches_request(rule, compiled, method, url, headers, body, resource_type)
        && (rule.matcher.status_codes.is_empty()
            || compiled.status_matchers.iter().any(|sm| sm.matches(status)))
        && matches_all_header_conds(
            &rule.matcher.response_headers,
            &compiled.resp_header_regex,
            resp_headers,
        )
        && body_cond_matches(
            rule.matcher.response_body.as_ref(),
            compiled.resp_body_regex.as_ref(),
            resp_body,
        )
}

/// Substitutes `$1`..`$9` in `template` with capture groups from `captures`.
/// Any other text (including a lone `$0` or `$` not followed by a digit
/// 1-9) is passed through unchanged.
fn substitute_captures(template: &str, captures: Option<&regex::Captures>) -> String {
    let Some(caps) = captures else {
        return template.to_string();
    };
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&next) = chars.peek() {
                if next.is_ascii_digit() && next != '0' {
                    chars.next();
                    let idx = next.to_digit(10).unwrap_or(0) as usize;
                    if let Some(m) = caps.get(idx) {
                        result.push_str(m.as_str());
                    }
                    continue;
                }
            }
        }
        result.push(c);
    }
    result
}

fn apply_find_replace(
    text: &str,
    find: &str,
    replace: &str,
    is_regex: bool,
    compiled: Option<&Regex>,
) -> String {
    if is_regex {
        match compiled {
            Some(re) => re.replace_all(text, replace).into_owned(),
            None => text.to_string(),
        }
    } else {
        text.replace(find, replace)
    }
}

fn decode_payload_encoding(body: &str, encoding: PayloadEncoding) -> Vec<u8> {
    match encoding {
        PayloadEncoding::Text => body.as_bytes().to_vec(),
        PayloadEncoding::Base64 => BASE64.decode(body).unwrap_or_default(),
    }
}

fn set_header(headers: &mut Vec<HeaderPair>, name: &str, value: &str) {
    headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
    headers.push(HeaderPair::new(name, value));
}

fn remove_header(headers: &mut Vec<HeaderPair>, name: &str) -> bool {
    let before = headers.len();
    headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
    headers.len() != before
}

fn set_query_param(url: &mut url::Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != name)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.push((name.to_string(), value.to_string()));
    url.query_pairs_mut().clear().extend_pairs(&pairs);
}

fn remove_query_param(url: &mut url::Url, name: &str) -> bool {
    let original: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let filtered: Vec<(String, String)> = original
        .iter()
        .filter(|(k, _)| k != name)
        .cloned()
        .collect();
    if filtered.len() == original.len() {
        return false;
    }
    if filtered.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(&filtered);
    }
    true
}

/// One segment of a parsed dot/bracket JSON path (e.g. `"a.b[0]"` parses to
/// `[Key("a"), Key("b"), Index(0)]`).
enum PathSegment {
    /// An object key.
    Key(String),
    /// An array index.
    Index(usize),
}

fn parse_path(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !current.is_empty() {
                    segments.push(PathSegment::Key(std::mem::take(&mut current)));
                }
            }
            '[' => {
                if !current.is_empty() {
                    segments.push(PathSegment::Key(std::mem::take(&mut current)));
                }
                let mut idx = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    idx.push(c2);
                }
                if let Ok(n) = idx.parse::<usize>() {
                    segments.push(PathSegment::Index(n));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        segments.push(PathSegment::Key(current));
    }
    segments
}

fn json_get_mut<'a>(root: &'a mut Value, segments: &[PathSegment]) -> Option<&'a mut Value> {
    let mut cur = root;
    for seg in segments {
        cur = match (seg, cur) {
            (PathSegment::Key(k), Value::Object(map)) => map.get_mut(k)?,
            (PathSegment::Index(i), Value::Array(arr)) => arr.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Sets `value` at `path`, creating intermediate objects/arrays as needed
/// (arrays are padded with `null` up to the required index).
fn json_set(current: &mut Value, segments: &[PathSegment], value: Value) {
    match segments.split_first() {
        None => *current = value,
        Some((PathSegment::Key(key), rest)) => {
            if !current.is_object() {
                *current = Value::Object(serde_json::Map::new());
            }
            if let Value::Object(map) = current {
                let entry = map.entry(key.clone()).or_insert(Value::Null);
                json_set(entry, rest, value);
            }
        }
        Some((PathSegment::Index(idx), rest)) => {
            if !current.is_array() {
                *current = Value::Array(Vec::new());
            }
            if let Value::Array(arr) = current {
                while arr.len() <= *idx {
                    arr.push(Value::Null);
                }
                json_set(&mut arr[*idx], rest, value);
            }
        }
    }
}

/// Removes the value at `path`, if present. A no-op if the path (or any
/// intermediate segment) doesn't exist.
fn json_remove(current: &mut Value, segments: &[PathSegment]) {
    let Some((last, init)) = segments.split_last() else {
        return;
    };
    let Some(parent) = json_get_mut(current, init) else {
        return;
    };
    match (last, parent) {
        (PathSegment::Key(k), Value::Object(map)) => {
            map.remove(k);
        }
        (PathSegment::Index(i), Value::Array(arr)) => {
            if *i < arr.len() {
                arr.remove(*i);
            }
        }
        _ => {}
    }
}

/// Shallow-merges `value` (which must be an object) into the object at
/// `path`; if `value` isn't an object, behaves like [`json_set`]. If the
/// existing value at `path` isn't an object, it is replaced.
fn json_merge(root: &mut Value, segments: &[PathSegment], value: Value) {
    let Value::Object(new_map) = value else {
        json_set(root, segments, value);
        return;
    };
    match json_get_mut(root, segments) {
        Some(Value::Object(existing)) => {
            for (k, v) in new_map {
                existing.insert(k, v);
            }
        }
        Some(existing) => *existing = Value::Object(new_map),
        None => json_set(root, segments, Value::Object(new_map)),
    }
}

/// Pushes `value` onto the array at `path`, creating the array if the path
/// is absent. If a non-array value already exists at `path`, it is
/// replaced with a new single-element array.
fn json_append(root: &mut Value, segments: &[PathSegment], value: Value) {
    match json_get_mut(root, segments) {
        Some(Value::Array(arr)) => arr.push(value),
        Some(existing) => *existing = Value::Array(vec![value]),
        None => json_set(root, segments, Value::Array(vec![value])),
    }
}

/// Parses `bytes` as JSON, applies `ops` in order, and re-serializes.
/// Returns `None` (leaving the body untouched) if `bytes` isn't valid JSON.
fn apply_json_ops(bytes: &[u8], ops: &[JsonOp]) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    for op in ops {
        let segments = parse_path(&op.path);
        match op.op {
            JsonOpKind::Set => json_set(
                &mut value,
                &segments,
                op.value.clone().unwrap_or(Value::Null),
            ),
            JsonOpKind::Remove => json_remove(&mut value, &segments),
            JsonOpKind::Merge => json_merge(
                &mut value,
                &segments,
                op.value.clone().unwrap_or(Value::Null),
            ),
            JsonOpKind::Append => json_append(
                &mut value,
                &segments,
                op.value.clone().unwrap_or(Value::Null),
            ),
        }
    }
    serde_json::to_vec(&value).ok()
}

/// Request-side context passed to [`RuleSet::apply_request`].
pub struct RequestCtx<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// Full request URL.
    pub url: &'a url::Url,
    /// Request headers.
    pub headers: &'a [HeaderPair],
    /// Decoded request body, if any.
    pub body: Option<&'a [u8]>,
    /// Inferred resource type.
    pub resource_type: ResourceType,
}

/// Request-plus-response context passed to [`RuleSet::apply_response`].
pub struct ResponseCtx<'a> {
    /// HTTP method of the originating request.
    pub method: &'a str,
    /// Full URL of the originating request.
    pub url: &'a url::Url,
    /// Headers of the originating request.
    pub headers: &'a [HeaderPair],
    /// Decoded body of the originating request, if any.
    pub body: Option<&'a [u8]>,
    /// Inferred resource type.
    pub resource_type: ResourceType,
    /// Response status code.
    pub status: u16,
    /// Response headers.
    pub resp_headers: &'a [HeaderPair],
    /// Decoded response body, if any.
    pub resp_body: Option<&'a [u8]>,
}

/// A response synthesized locally by a [`Action::MockResponse`], short-circuiting
/// the request phase before any upstream connection is made.
#[derive(Debug, Clone)]
pub struct MockedResponse {
    /// Status code to respond with.
    pub status: u16,
    /// Headers to respond with.
    pub headers: Vec<HeaderPair>,
    /// Decoded response body.
    pub body: Vec<u8>,
    /// Artificial delay before responding, in milliseconds.
    pub delay_ms: u64,
}

/// The result of running a [`RuleSet`] over a request.
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    /// Possibly rule-modified HTTP method.
    pub method: String,
    /// Possibly rule-modified URL.
    pub url: String,
    /// Possibly rule-modified headers.
    pub headers: Vec<HeaderPair>,
    /// Possibly rule-modified decoded body.
    pub body: Option<Vec<u8>>,
    /// IDs of rules whose matcher matched in the request phase.
    pub matched: Vec<String>,
    /// Whether any rule modified the request.
    pub modified: bool,
    /// Set if a [`Action::Block`] fired; the request should not be sent upstream.
    pub blocked: Option<String>,
    /// Set if a [`Action::MockResponse`] fired; the request should not be sent upstream.
    pub mocked: Option<MockedResponse>,
    /// Total artificial delay to apply before sending the request, in milliseconds.
    pub delay_ms: u64,
    /// Most restrictive throttle to apply while sending the request, if any.
    pub throttle_bps: Option<u64>,
}

/// The result of running a [`RuleSet`] over a response.
#[derive(Debug, Clone)]
pub struct ResponseOutcome {
    /// Possibly rule-modified status code.
    pub status: u16,
    /// Possibly rule-modified headers.
    pub headers: Vec<HeaderPair>,
    /// Possibly rule-modified decoded body.
    pub body: Option<Vec<u8>>,
    /// IDs of rules whose matcher matched in the response phase.
    pub matched: Vec<String>,
    /// Whether any rule modified the response.
    pub modified: bool,
    /// Total artificial delay to apply before returning the response, in milliseconds.
    pub delay_ms: u64,
    /// Most restrictive throttle to apply while returning the response, if any.
    pub throttle_bps: Option<u64>,
}

/// A compiled, sorted collection of [`Rule`]s ready for matching.
///
/// Construct with [`RuleSet::new`]; rules are sorted by `priority` (lower
/// first) then original insertion order, disabled rules are dropped, and
/// rules with invalid regex/glob patterns are dropped and reported via
/// [`RuleSet::errors`] rather than causing a panic.
pub struct RuleSet {
    rules: Vec<Rule>,
    compiled: Vec<CompiledEntry>,
    errors: Vec<RuleError>,
}

impl RuleSet {
    /// Compiles `rules` into a [`RuleSet`]. Disabled rules are skipped;
    /// rules with invalid regex/glob patterns are skipped and reported via
    /// [`RuleSet::errors`].
    pub fn new(rules: Vec<Rule>) -> Self {
        let mut indexed: Vec<(usize, Rule)> = rules.into_iter().enumerate().collect();
        indexed.sort_by(|(ia, a), (ib, b)| a.priority.cmp(&b.priority).then(ia.cmp(ib)));

        let mut out_rules = Vec::new();
        let mut out_compiled = Vec::new();
        let mut errors = Vec::new();
        for (_, rule) in indexed {
            if !rule.enabled {
                continue;
            }
            match compile_rule(&rule) {
                Ok(entry) => {
                    out_rules.push(rule);
                    out_compiled.push(entry);
                }
                Err(msgs) => {
                    for message in msgs {
                        errors.push(RuleError {
                            rule_id: rule.id.clone(),
                            message,
                        });
                    }
                }
            }
        }
        RuleSet {
            rules: out_rules,
            compiled: out_compiled,
            errors,
        }
    }

    /// Returns the compiled, sorted, enabled rules (rules that failed to
    /// compile are excluded; see [`RuleSet::errors`]).
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Returns compilation errors for rules that were skipped.
    pub fn errors(&self) -> &[RuleError] {
        &self.errors
    }

    /// Runs the request phase: evaluates each rule's request-evaluable
    /// conditions (URL, method, host/port, resource type, request headers,
    /// request body) and applies its request-phase actions in order.
    ///
    /// All matching rules apply (not first-match-wins), except that a
    /// [`Action::Block`] or [`Action::MockResponse`] short-circuits any
    /// remaining (lower-priority) rules in this phase.
    pub fn apply_request(&self, ctx: RequestCtx) -> RequestOutcome {
        let mut method = ctx.method.to_string();
        let mut url = ctx.url.clone();
        let mut headers = ctx.headers.to_vec();
        let mut body: Option<Vec<u8>> = ctx.body.map(|b| b.to_vec());

        let mut matched = Vec::new();
        let mut modified = false;
        let mut blocked = None;
        let mut mocked = None;
        let mut delay_ms: u64 = 0;
        let mut throttle_bps: Option<u64> = None;

        for (rule, compiled) in self.rules.iter().zip(self.compiled.iter()) {
            if !matches_request(
                rule,
                compiled,
                &method,
                &url,
                &headers,
                body.as_deref(),
                ctx.resource_type,
            ) {
                continue;
            }
            matched.push(rule.id.clone());

            // Snapshot the URL text now, before this rule's own actions can
            // mutate `url`, so capture-group substitution always reflects
            // the state the matcher actually matched against.
            let match_url_str = url.as_str().to_string();
            let matcher_caps = if rule.matcher.url_op == UrlOp::Regex {
                compiled
                    .url_regex
                    .as_ref()
                    .and_then(|re| re.captures(match_url_str.as_str()))
            } else {
                None
            };

            let mut rule_modified = false;
            for (action, action_regex) in rule.actions.iter().zip(compiled.action_regex.iter()) {
                match action {
                    Action::Redirect { to } => {
                        let target = substitute_captures(to, matcher_caps.as_ref());
                        if let Ok(parsed) = url::Url::parse(&target) {
                            url = parsed;
                            rule_modified = true;
                        }
                    }
                    Action::RewriteUrl {
                        find,
                        replace,
                        regex,
                    } => {
                        let replacement = substitute_captures(replace, matcher_caps.as_ref());
                        let current = url.as_str().to_string();
                        let updated = apply_find_replace(
                            &current,
                            find,
                            &replacement,
                            *regex,
                            action_regex.as_ref(),
                        );
                        if updated != current {
                            if let Ok(parsed) = url::Url::parse(&updated) {
                                url = parsed;
                                rule_modified = true;
                            }
                        }
                    }
                    Action::SetQueryParam { name, value } => {
                        set_query_param(&mut url, name, value);
                        rule_modified = true;
                    }
                    Action::RemoveQueryParam { name } => {
                        if remove_query_param(&mut url, name) {
                            rule_modified = true;
                        }
                    }
                    Action::SetRequestHeader { name, value } => {
                        set_header(&mut headers, name, value);
                        rule_modified = true;
                    }
                    Action::RemoveRequestHeader { name } => {
                        if remove_header(&mut headers, name) {
                            rule_modified = true;
                        }
                    }
                    Action::SetRequestBody {
                        body: new_body,
                        encoding,
                        content_type,
                    } => {
                        body = Some(decode_payload_encoding(new_body, *encoding));
                        if let Some(ct) = content_type {
                            set_header(&mut headers, "Content-Type", ct);
                        }
                        rule_modified = true;
                    }
                    Action::ReplaceInRequestBody {
                        find,
                        replace,
                        regex,
                    } => {
                        if let Some(bytes) = &body {
                            let text = String::from_utf8_lossy(bytes);
                            let replaced = apply_find_replace(
                                &text,
                                find,
                                replace,
                                *regex,
                                action_regex.as_ref(),
                            );
                            if replaced != text {
                                body = Some(replaced.into_bytes());
                                rule_modified = true;
                            }
                        }
                    }
                    Action::JsonPatchRequest { ops } => {
                        if let Some(bytes) = &body {
                            if let Some(new_bytes) = apply_json_ops(bytes, ops) {
                                body = Some(new_bytes);
                                rule_modified = true;
                            }
                        }
                    }
                    Action::MockResponse {
                        status,
                        headers: mock_headers,
                        body: mock_body,
                        encoding,
                        delay_ms: mock_delay,
                    } => {
                        mocked = Some(MockedResponse {
                            status: *status,
                            headers: mock_headers.clone(),
                            body: decode_payload_encoding(mock_body, *encoding),
                            delay_ms: *mock_delay,
                        });
                        rule_modified = true;
                    }
                    Action::SetMethod { method: new_method } => {
                        method = new_method.clone();
                        rule_modified = true;
                    }
                    Action::Block { reason } => {
                        blocked = Some(if reason.is_empty() {
                            "blocked by rule".to_string()
                        } else {
                            reason.clone()
                        });
                        rule_modified = true;
                    }
                    Action::Delay { ms } => {
                        delay_ms = delay_ms.saturating_add(*ms);
                    }
                    Action::Throttle { bytes_per_sec } => {
                        throttle_bps = Some(match throttle_bps {
                            Some(existing) => existing.min(*bytes_per_sec),
                            None => *bytes_per_sec,
                        });
                    }
                    // Response-phase-only actions do not apply here.
                    Action::SetResponseHeader { .. }
                    | Action::RemoveResponseHeader { .. }
                    | Action::SetResponseBody { .. }
                    | Action::ReplaceInResponseBody { .. }
                    | Action::JsonPatchResponse { .. }
                    | Action::SetStatus { .. } => {}
                }
            }

            if rule_modified {
                modified = true;
            }
            if blocked.is_some() || mocked.is_some() {
                break;
            }
        }

        RequestOutcome {
            method,
            url: url.to_string(),
            headers,
            body,
            matched,
            modified,
            blocked,
            mocked,
            delay_ms,
            throttle_bps,
        }
    }

    /// Runs the response phase: re-evaluates each rule's full matcher
    /// (request conditions *and* status/response-header/response-body
    /// conditions) and applies its response-phase actions in order.
    ///
    /// All matching rules apply (not first-match-wins), except that a
    /// [`Action::Block`] short-circuits any remaining (lower-priority)
    /// rules in this phase. `Block` has no dedicated field on
    /// [`ResponseOutcome`] (a response already exists by this phase), so it
    /// only affects short-circuiting here, not the returned fields.
    pub fn apply_response(&self, ctx: ResponseCtx) -> ResponseOutcome {
        let mut status = ctx.status;
        let mut headers = ctx.resp_headers.to_vec();
        let mut body: Option<Vec<u8>> = ctx.resp_body.map(|b| b.to_vec());

        let mut matched = Vec::new();
        let mut modified = false;
        let mut delay_ms: u64 = 0;
        let mut throttle_bps: Option<u64> = None;
        let mut short_circuit = false;

        for (rule, compiled) in self.rules.iter().zip(self.compiled.iter()) {
            if !matches_response(
                rule,
                compiled,
                ctx.method,
                ctx.url,
                ctx.headers,
                ctx.body,
                ctx.resource_type,
                status,
                &headers,
                body.as_deref(),
            ) {
                continue;
            }
            matched.push(rule.id.clone());

            let mut rule_modified = false;
            for (action, action_regex) in rule.actions.iter().zip(compiled.action_regex.iter()) {
                match action {
                    Action::SetResponseHeader { name, value } => {
                        set_header(&mut headers, name, value);
                        rule_modified = true;
                    }
                    Action::RemoveResponseHeader { name } => {
                        if remove_header(&mut headers, name) {
                            rule_modified = true;
                        }
                    }
                    Action::SetResponseBody {
                        body: new_body,
                        encoding,
                        content_type,
                    } => {
                        body = Some(decode_payload_encoding(new_body, *encoding));
                        if let Some(ct) = content_type {
                            set_header(&mut headers, "Content-Type", ct);
                        }
                        rule_modified = true;
                    }
                    Action::ReplaceInResponseBody {
                        find,
                        replace,
                        regex,
                    } => {
                        if let Some(bytes) = &body {
                            let text = String::from_utf8_lossy(bytes);
                            let replaced = apply_find_replace(
                                &text,
                                find,
                                replace,
                                *regex,
                                action_regex.as_ref(),
                            );
                            if replaced != text {
                                body = Some(replaced.into_bytes());
                                rule_modified = true;
                            }
                        }
                    }
                    Action::JsonPatchResponse { ops } => {
                        if let Some(bytes) = &body {
                            if let Some(new_bytes) = apply_json_ops(bytes, ops) {
                                body = Some(new_bytes);
                                rule_modified = true;
                            }
                        }
                    }
                    Action::SetStatus { status: new_status } => {
                        status = *new_status;
                        rule_modified = true;
                    }
                    Action::Delay { ms } => {
                        delay_ms = delay_ms.saturating_add(*ms);
                    }
                    Action::Throttle { bytes_per_sec } => {
                        throttle_bps = Some(match throttle_bps {
                            Some(existing) => existing.min(*bytes_per_sec),
                            None => *bytes_per_sec,
                        });
                    }
                    Action::Block { .. } => {
                        short_circuit = true;
                    }
                    // Request-phase-only actions do not apply here.
                    Action::Redirect { .. }
                    | Action::RewriteUrl { .. }
                    | Action::SetQueryParam { .. }
                    | Action::RemoveQueryParam { .. }
                    | Action::SetRequestHeader { .. }
                    | Action::RemoveRequestHeader { .. }
                    | Action::SetRequestBody { .. }
                    | Action::ReplaceInRequestBody { .. }
                    | Action::JsonPatchRequest { .. }
                    | Action::MockResponse { .. }
                    | Action::SetMethod { .. } => {}
                }
            }

            if rule_modified {
                modified = true;
            }
            if short_circuit {
                break;
            }
        }

        ResponseOutcome {
            status,
            headers,
            body,
            matched,
            modified,
            delay_ms,
            throttle_bps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn matcher_url(op: UrlOp, value: &str) -> Matcher {
        Matcher {
            url_op: op,
            url_value: value.to_string(),
            ..Matcher::default()
        }
    }

    fn make_rule(id: &str, priority: i32, matcher: Matcher, actions: Vec<Action>) -> Rule {
        Rule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            priority,
            group: None,
            notes: None,
            matcher,
            actions,
        }
    }

    fn req_ctx(url: &Url) -> RequestCtx<'_> {
        RequestCtx {
            method: "GET",
            url,
            headers: &[],
            body: None,
            resource_type: ResourceType::Other,
        }
    }

    #[test]
    fn url_op_contains() {
        let url = Url::parse("http://example.com/api/v1/users").unwrap();
        let rule = make_rule("r1", 0, matcher_url(UrlOp::Contains, "/api/"), vec![]);
        let set = RuleSet::new(vec![rule]);
        assert_eq!(set.apply_request(req_ctx(&url)).matched, vec!["r1"]);

        let miss = Url::parse("http://example.com/other").unwrap();
        assert!(set.apply_request(req_ctx(&miss)).matched.is_empty());
    }

    #[test]
    fn url_op_equals() {
        let url = Url::parse("http://example.com/exact").unwrap();
        let rule = make_rule(
            "r1",
            0,
            matcher_url(UrlOp::Equals, "http://example.com/exact"),
            vec![],
        );
        let set = RuleSet::new(vec![rule]);
        assert_eq!(set.apply_request(req_ctx(&url)).matched, vec!["r1"]);
    }

    #[test]
    fn url_op_starts_ends_with() {
        let url = Url::parse("http://example.com/prefix/suffix.json").unwrap();
        let starts = make_rule(
            "s",
            0,
            matcher_url(UrlOp::StartsWith, "http://example.com/prefix"),
            vec![],
        );
        let ends = make_rule("e", 0, matcher_url(UrlOp::EndsWith, ".json"), vec![]);
        let set = RuleSet::new(vec![starts, ends]);
        let matched = set.apply_request(req_ctx(&url)).matched;
        assert_eq!(matched, vec!["s", "e"]);
    }

    #[test]
    fn url_op_regex() {
        let url = Url::parse("http://example.com/users/42").unwrap();
        let rule = make_rule("r1", 0, matcher_url(UrlOp::Regex, r"/users/\d+$"), vec![]);
        let set = RuleSet::new(vec![rule]);
        assert_eq!(set.apply_request(req_ctx(&url)).matched, vec!["r1"]);

        let miss = Url::parse("http://example.com/users/abc").unwrap();
        assert!(set.apply_request(req_ctx(&miss)).matched.is_empty());
    }

    #[test]
    fn url_op_wildcard() {
        let url = Url::parse("http://example.com/assets/app.js").unwrap();
        let rule = make_rule(
            "r1",
            0,
            matcher_url(UrlOp::Wildcard, "http://example.com/assets/*.js"),
            vec![],
        );
        let set = RuleSet::new(vec![rule]);
        assert_eq!(set.apply_request(req_ctx(&url)).matched, vec!["r1"]);
    }

    #[test]
    fn wildcard_host_ports() {
        let matcher = Matcher {
            host_ports: vec!["*.example.com".to_string()],
            ..Matcher::default()
        };
        let rule = make_rule("r1", 0, matcher, vec![]);
        let set = RuleSet::new(vec![rule]);

        let sub = Url::parse("https://api.example.com/x").unwrap();
        assert_eq!(set.apply_request(req_ctx(&sub)).matched, vec!["r1"]);

        let bare = Url::parse("https://example.com/x").unwrap();
        assert!(set.apply_request(req_ctx(&bare)).matched.is_empty());

        let other = Url::parse("https://api.other.com/x").unwrap();
        assert!(set.apply_request(req_ctx(&other)).matched.is_empty());
    }

    fn resp_ctx<'a>(url: &'a Url, status: u16) -> ResponseCtx<'a> {
        ResponseCtx {
            method: "GET",
            url,
            headers: &[],
            body: None,
            resource_type: ResourceType::Other,
            status,
            resp_headers: &[],
            resp_body: None,
        }
    }

    #[test]
    fn status_class_and_range() {
        let url = Url::parse("http://example.com/").unwrap();
        let class_matcher = Matcher {
            status_codes: vec!["4xx".to_string()],
            ..Matcher::default()
        };
        let class_rule = make_rule("class", 0, class_matcher, vec![]);

        let range_matcher = Matcher {
            status_codes: vec!["500-599".to_string()],
            ..Matcher::default()
        };
        let range_rule = make_rule("range", 0, range_matcher, vec![]);

        let set = RuleSet::new(vec![class_rule, range_rule]);

        let out_404 = set.apply_response(resp_ctx(&url, 404));
        assert_eq!(out_404.matched, vec!["class"]);

        let out_503 = set.apply_response(resp_ctx(&url, 503));
        assert_eq!(out_503.matched, vec!["range"]);

        let out_200 = set.apply_response(resp_ctx(&url, 200));
        assert!(out_200.matched.is_empty());
    }

    #[test]
    fn redirect_capture_group_substitution() {
        let url = Url::parse("http://old.example.com/foo/bar").unwrap();
        let matcher = matcher_url(UrlOp::Regex, r"^http://old\.example\.com/(.+)$");
        let rule = make_rule(
            "r1",
            0,
            matcher,
            vec![Action::Redirect {
                to: "http://new.example.com/$1".to_string(),
            }],
        );
        let set = RuleSet::new(vec![rule]);
        let out = set.apply_request(req_ctx(&url));
        assert_eq!(out.url, "http://new.example.com/foo/bar");
        assert!(out.modified);
    }

    #[test]
    fn json_patch_set_remove_merge_append() {
        let body = br#"{"a":1,"b":{"x":1},"list":[1,2]}"#;
        let ops = vec![
            JsonOp {
                op: JsonOpKind::Set,
                path: "a".to_string(),
                value: Some(Value::from(99)),
            },
            JsonOp {
                op: JsonOpKind::Remove,
                path: "b.x".to_string(),
                value: None,
            },
            JsonOp {
                op: JsonOpKind::Merge,
                path: "b".to_string(),
                value: Some(serde_json::json!({"y": 2})),
            },
            JsonOp {
                op: JsonOpKind::Append,
                path: "list".to_string(),
                value: Some(Value::from(3)),
            },
            JsonOp {
                op: JsonOpKind::Set,
                path: "created.nested[0].name".to_string(),
                value: Some(Value::from("hi")),
            },
        ];
        let out = apply_json_ops(body, &ops).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["a"], 99);
        assert!(v["b"].get("x").is_none());
        assert_eq!(v["b"]["y"], 2);
        assert_eq!(v["list"], serde_json::json!([1, 2, 3]));
        assert_eq!(v["created"]["nested"][0]["name"], "hi");
    }

    #[test]
    fn json_patch_invalid_body_is_noop() {
        assert!(apply_json_ops(b"not json", &[]).is_none());
    }

    #[test]
    fn block_short_circuits_request_phase() {
        let url = Url::parse("http://example.com/x").unwrap();
        let blocker = make_rule(
            "blocker",
            0,
            Matcher::default(),
            vec![Action::Block {
                reason: "nope".to_string(),
            }],
        );
        let follower = make_rule(
            "follower",
            1,
            Matcher::default(),
            vec![Action::SetRequestHeader {
                name: "X-Test".to_string(),
                value: "1".to_string(),
            }],
        );
        let set = RuleSet::new(vec![blocker, follower]);
        let out = set.apply_request(req_ctx(&url));
        assert_eq!(out.matched, vec!["blocker"]);
        assert_eq!(out.blocked.as_deref(), Some("nope"));
        assert!(!out.headers.iter().any(|h| h.name == "X-Test"));
    }

    #[test]
    fn mock_short_circuits_request_phase() {
        let url = Url::parse("http://example.com/x").unwrap();
        let mocker = make_rule(
            "mocker",
            0,
            Matcher::default(),
            vec![Action::MockResponse {
                status: 201,
                headers: vec![],
                body: "hi".to_string(),
                encoding: PayloadEncoding::Text,
                delay_ms: 0,
            }],
        );
        let follower = make_rule(
            "follower",
            1,
            Matcher::default(),
            vec![Action::SetRequestHeader {
                name: "X-Test".to_string(),
                value: "1".to_string(),
            }],
        );
        let set = RuleSet::new(vec![mocker, follower]);
        let out = set.apply_request(req_ctx(&url));
        assert_eq!(out.matched, vec!["mocker"]);
        let mocked = out.mocked.expect("expected mocked response");
        assert_eq!(mocked.status, 201);
        assert_eq!(mocked.body, b"hi");
        assert!(!out.headers.iter().any(|h| h.name == "X-Test"));
    }

    #[test]
    fn body_replace_literal_and_regex() {
        let url = Url::parse("http://example.com/x").unwrap();
        let literal_rule = make_rule(
            "lit",
            0,
            Matcher::default(),
            vec![Action::ReplaceInRequestBody {
                find: "foo".to_string(),
                replace: "bar".to_string(),
                regex: false,
            }],
        );
        let set = RuleSet::new(vec![literal_rule]);
        let ctx = RequestCtx {
            method: "POST",
            url: &url,
            headers: &[],
            body: Some(b"foo baz foo"),
            resource_type: ResourceType::Other,
        };
        let out = set.apply_request(ctx);
        assert_eq!(out.body.as_deref(), Some(&b"bar baz bar"[..]));

        let regex_rule = make_rule(
            "re",
            0,
            Matcher::default(),
            vec![Action::ReplaceInRequestBody {
                find: r"\d+".to_string(),
                replace: "NUM".to_string(),
                regex: true,
            }],
        );
        let set2 = RuleSet::new(vec![regex_rule]);
        let ctx2 = RequestCtx {
            method: "POST",
            url: &url,
            headers: &[],
            body: Some(b"value 123 and 456"),
            resource_type: ResourceType::Other,
        };
        let out2 = set2.apply_request(ctx2);
        assert_eq!(out2.body.as_deref(), Some(&b"value NUM and NUM"[..]));
    }

    #[test]
    fn priority_ordering_then_insertion_order() {
        let url = Url::parse("http://example.com/x").unwrap();
        let a = make_rule("a", 5, Matcher::default(), vec![]);
        let b = make_rule("b", 1, Matcher::default(), vec![]);
        let c = make_rule("c", 1, Matcher::default(), vec![]);
        let d = make_rule("d", -1, Matcher::default(), vec![]);
        // Insertion order: a, b, c, d. Expected evaluation order by
        // (priority asc, insertion order): d(-1), b(1), c(1), a(5).
        let set = RuleSet::new(vec![a, b, c, d]);
        let out = set.apply_request(req_ctx(&url));
        assert_eq!(out.matched, vec!["d", "b", "c", "a"]);
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let url = Url::parse("http://example.com/x").unwrap();
        let mut rule = make_rule("r1", 0, Matcher::default(), vec![]);
        rule.enabled = false;
        let set = RuleSet::new(vec![rule]);
        assert!(set.rules().is_empty());
        assert!(set.apply_request(req_ctx(&url)).matched.is_empty());
    }

    #[test]
    fn bad_regex_does_not_panic_and_is_reported() {
        let url = Url::parse("http://example.com/x").unwrap();
        let rule = make_rule("bad", 0, matcher_url(UrlOp::Regex, "(unclosed"), vec![]);
        let set = RuleSet::new(vec![rule]);
        assert!(set.rules().is_empty());
        assert_eq!(set.errors().len(), 1);
        assert_eq!(set.errors()[0].rule_id, "bad");
        // Must not panic, and must simply never match.
        assert!(set.apply_request(req_ctx(&url)).matched.is_empty());
    }

    #[test]
    fn bad_glob_does_not_panic_and_is_reported() {
        let matcher = Matcher {
            host_ports: vec!["[".to_string()],
            ..Matcher::default()
        };
        let rule = make_rule("badglob", 0, matcher, vec![]);
        let set = RuleSet::new(vec![rule]);
        assert!(set.rules().is_empty());
        assert!(!set.errors().is_empty());
    }

    #[test]
    fn set_header_replaces_all_same_name() {
        let url = Url::parse("http://example.com/x").unwrap();
        let rule = make_rule(
            "r1",
            0,
            Matcher::default(),
            vec![Action::SetRequestHeader {
                name: "X-Foo".to_string(),
                value: "new".to_string(),
            }],
        );
        let set = RuleSet::new(vec![rule]);
        let headers = vec![
            HeaderPair::new("X-Foo", "old1"),
            HeaderPair::new("x-foo", "old2"),
            HeaderPair::new("X-Bar", "keep"),
        ];
        let ctx = RequestCtx {
            method: "GET",
            url: &url,
            headers: &headers,
            body: None,
            resource_type: ResourceType::Other,
        };
        let out = set.apply_request(ctx);
        let foo_values: Vec<&str> = out
            .headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("X-Foo"))
            .map(|h| h.value.as_str())
            .collect();
        assert_eq!(foo_values, vec!["new"]);
        assert!(out.headers.iter().any(|h| h.name == "X-Bar"));
    }
}

/// Regression guard for the "`#[serde(tag = \"type\")]` on an enum renames
/// variant names, not struct-variant field names" bug: every [`Action`]
/// variant must serialize with exactly the camelCase keys the UI's
/// `ui/src/lib/types.ts` (`Action` union) declares, must round-trip through
/// serialize/deserialize, and must still accept the old (buggy) snake_case
/// field spelling so rules already saved to `~/.rdproxy/rules.json` keep
/// loading.
#[cfg(test)]
mod action_wire_format_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(v: &Value) -> BTreeSet<String> {
        v.as_object()
            .expect("action must serialize to a JSON object")
            .keys()
            .cloned()
            .collect()
    }

    fn key_set(extra: &[&str]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = extra.iter().map(|s| s.to_string()).collect();
        set.insert("type".to_string());
        set
    }

    /// Serializes `action`, asserts its JSON keys are exactly `"type"` +
    /// `expected_keys`, then round-trips it through deserialize and
    /// compares for equality with the original.
    fn check_wire_format(action: Action, expected_keys: &[&str]) {
        let value = serde_json::to_value(&action).unwrap();
        assert_eq!(
            keys(&value),
            key_set(expected_keys),
            "unexpected wire keys for {value}"
        );

        let round_tripped: Action = serde_json::from_value(value).unwrap();
        assert_eq!(round_tripped, action);
    }

    #[test]
    fn redirect_wire_format() {
        check_wire_format(
            Action::Redirect {
                to: "http://x".to_string(),
            },
            &["to"],
        );
    }

    #[test]
    fn rewrite_url_wire_format() {
        check_wire_format(
            Action::RewriteUrl {
                find: "a".to_string(),
                replace: "b".to_string(),
                regex: true,
            },
            &["find", "replace", "regex"],
        );
    }

    #[test]
    fn set_query_param_wire_format() {
        check_wire_format(
            Action::SetQueryParam {
                name: "n".to_string(),
                value: "v".to_string(),
            },
            &["name", "value"],
        );
    }

    #[test]
    fn remove_query_param_wire_format() {
        check_wire_format(
            Action::RemoveQueryParam {
                name: "n".to_string(),
            },
            &["name"],
        );
    }

    #[test]
    fn set_request_header_wire_format() {
        check_wire_format(
            Action::SetRequestHeader {
                name: "n".to_string(),
                value: "v".to_string(),
            },
            &["name", "value"],
        );
    }

    #[test]
    fn remove_request_header_wire_format() {
        check_wire_format(
            Action::RemoveRequestHeader {
                name: "n".to_string(),
            },
            &["name"],
        );
    }

    #[test]
    fn set_response_header_wire_format() {
        check_wire_format(
            Action::SetResponseHeader {
                name: "n".to_string(),
                value: "v".to_string(),
            },
            &["name", "value"],
        );
    }

    #[test]
    fn remove_response_header_wire_format() {
        check_wire_format(
            Action::RemoveResponseHeader {
                name: "n".to_string(),
            },
            &["name"],
        );
    }

    #[test]
    fn set_request_body_wire_format() {
        check_wire_format(
            Action::SetRequestBody {
                body: "b".to_string(),
                encoding: PayloadEncoding::Text,
                content_type: Some("application/json".to_string()),
            },
            &["body", "encoding", "contentType"],
        );
    }

    #[test]
    fn set_request_body_serializes_content_type_as_camel_case() {
        let value = serde_json::to_value(Action::SetRequestBody {
            body: "b".to_string(),
            encoding: PayloadEncoding::Text,
            content_type: Some("text/plain".to_string()),
        })
        .unwrap();
        assert_eq!(value["contentType"], "text/plain");
        assert!(value.get("content_type").is_none());
    }

    #[test]
    fn set_request_body_accepts_legacy_snake_case_content_type() {
        let json = serde_json::json!({
            "type": "setRequestBody",
            "body": "b",
            "encoding": "text",
            "content_type": "text/plain",
        });
        let action: Action = serde_json::from_value(json).unwrap();
        assert_eq!(
            action,
            Action::SetRequestBody {
                body: "b".to_string(),
                encoding: PayloadEncoding::Text,
                content_type: Some("text/plain".to_string()),
            }
        );
    }

    #[test]
    fn set_response_body_wire_format() {
        check_wire_format(
            Action::SetResponseBody {
                body: "b".to_string(),
                encoding: PayloadEncoding::Base64,
                content_type: Some("application/json".to_string()),
            },
            &["body", "encoding", "contentType"],
        );
    }

    #[test]
    fn set_response_body_serializes_content_type_as_camel_case() {
        let value = serde_json::to_value(Action::SetResponseBody {
            body: "b".to_string(),
            encoding: PayloadEncoding::Text,
            content_type: Some("text/plain".to_string()),
        })
        .unwrap();
        assert_eq!(value["contentType"], "text/plain");
        assert!(value.get("content_type").is_none());
    }

    #[test]
    fn set_response_body_accepts_legacy_snake_case_content_type() {
        let json = serde_json::json!({
            "type": "setResponseBody",
            "body": "b",
            "encoding": "text",
            "content_type": "text/plain",
        });
        let action: Action = serde_json::from_value(json).unwrap();
        assert_eq!(
            action,
            Action::SetResponseBody {
                body: "b".to_string(),
                encoding: PayloadEncoding::Text,
                content_type: Some("text/plain".to_string()),
            }
        );
    }

    #[test]
    fn replace_in_request_body_wire_format() {
        check_wire_format(
            Action::ReplaceInRequestBody {
                find: "a".to_string(),
                replace: "b".to_string(),
                regex: false,
            },
            &["find", "replace", "regex"],
        );
    }

    #[test]
    fn replace_in_response_body_wire_format() {
        check_wire_format(
            Action::ReplaceInResponseBody {
                find: "a".to_string(),
                replace: "b".to_string(),
                regex: false,
            },
            &["find", "replace", "regex"],
        );
    }

    #[test]
    fn json_patch_request_wire_format() {
        check_wire_format(
            Action::JsonPatchRequest {
                ops: vec![JsonOp {
                    op: JsonOpKind::Set,
                    path: "a".to_string(),
                    value: Some(Value::from(1)),
                }],
            },
            &["ops"],
        );
    }

    #[test]
    fn json_patch_response_wire_format() {
        check_wire_format(
            Action::JsonPatchResponse {
                ops: vec![JsonOp {
                    op: JsonOpKind::Remove,
                    path: "a".to_string(),
                    value: None,
                }],
            },
            &["ops"],
        );
    }

    #[test]
    fn mock_response_wire_format() {
        check_wire_format(
            Action::MockResponse {
                status: 201,
                headers: vec![HeaderPair::new("X-Test", "1")],
                body: "hi".to_string(),
                encoding: PayloadEncoding::Text,
                delay_ms: 250,
            },
            &["status", "headers", "body", "encoding", "delayMs"],
        );
    }

    #[test]
    fn mock_response_serializes_delay_ms_as_camel_case() {
        let action = Action::MockResponse {
            status: 200,
            headers: vec![],
            body: String::new(),
            encoding: PayloadEncoding::Text,
            delay_ms: 500,
        };
        let value = serde_json::to_value(&action).unwrap();
        assert_eq!(value["delayMs"], 500);
        assert!(value.get("delay_ms").is_none());
    }

    #[test]
    fn mock_response_accepts_legacy_snake_case_delay_ms() {
        let json = serde_json::json!({
            "type": "mockResponse",
            "status": 200,
            "headers": [],
            "body": "",
            "encoding": "text",
            "delay_ms": 750,
        });
        let action: Action = serde_json::from_value(json).unwrap();
        assert_eq!(
            action,
            Action::MockResponse {
                status: 200,
                headers: vec![],
                body: String::new(),
                encoding: PayloadEncoding::Text,
                delay_ms: 750,
            }
        );
    }

    #[test]
    fn set_status_wire_format() {
        check_wire_format(Action::SetStatus { status: 404 }, &["status"]);
    }

    #[test]
    fn block_wire_format() {
        check_wire_format(
            Action::Block {
                reason: "nope".to_string(),
            },
            &["reason"],
        );
    }

    #[test]
    fn delay_wire_format() {
        check_wire_format(Action::Delay { ms: 10 }, &["ms"]);
    }

    #[test]
    fn throttle_wire_format() {
        check_wire_format(
            Action::Throttle {
                bytes_per_sec: 1024,
            },
            &["bytesPerSec"],
        );
    }

    #[test]
    fn throttle_serializes_bytes_per_sec_as_camel_case() {
        let value = serde_json::to_value(Action::Throttle {
            bytes_per_sec: 4096,
        })
        .unwrap();
        assert_eq!(value["bytesPerSec"], 4096);
        assert!(value.get("bytes_per_sec").is_none());
    }

    #[test]
    fn throttle_accepts_legacy_snake_case_bytes_per_sec() {
        let json = serde_json::json!({"type": "throttle", "bytes_per_sec": 2048});
        let action: Action = serde_json::from_value(json).unwrap();
        assert_eq!(
            action,
            Action::Throttle {
                bytes_per_sec: 2048
            }
        );
    }

    #[test]
    fn set_method_wire_format() {
        check_wire_format(
            Action::SetMethod {
                method: "POST".to_string(),
            },
            &["method"],
        );
    }
}
