//! Traffic records: the core data model describing a captured HTTP(S)/WS flow.

use serde::{Deserialize, Serialize};

/// Unique identifier for a [`Flow`].
pub type FlowId = uuid::Uuid;

/// Lifecycle state of a captured flow.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FlowState {
    /// The flow has been created but the request has not started streaming.
    Pending,
    /// The request is being sent to the upstream server.
    Requesting,
    /// The response is being received from the upstream server.
    Responding,
    /// The flow finished successfully.
    Complete,
    /// The flow ended with an error (connection failure, timeout, ...).
    Error,
}

/// Classification of the kind of resource a flow represents, used for
/// filtering and icon selection in the UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ResourceType {
    /// A top-level HTML document.
    Document,
    /// A CSS stylesheet.
    Stylesheet,
    /// A JavaScript resource.
    Script,
    /// An image resource.
    Image,
    /// A web font.
    Font,
    /// An `XMLHttpRequest`/`fetch` request.
    Xhr,
    /// A JSON payload.
    Json,
    /// Audio or video media.
    Media,
    /// A WebSocket connection.
    WebSocket,
    /// Anything that doesn't fit the other categories.
    Other,
}

impl ResourceType {
    /// Infers a [`ResourceType`] from an optional MIME type and the request path.
    ///
    /// The MIME type (when present) is authoritative; the path extension is
    /// used as a fallback heuristic when the MIME type is missing or generic.
    pub fn infer(mime: Option<&str>, path: &str) -> ResourceType {
        let path_no_query = path.split(['?', '#']).next().unwrap_or(path);
        let ext = path_no_query
            .rsplit('.')
            .next()
            .filter(|e| !e.contains('/'))
            .unwrap_or("")
            .to_ascii_lowercase();

        if let Some(mime) = mime {
            let mime = mime
                .split(';')
                .next()
                .unwrap_or(mime)
                .trim()
                .to_ascii_lowercase();
            match mime.as_str() {
                "text/html" | "application/xhtml+xml" => return ResourceType::Document,
                "text/css" => return ResourceType::Stylesheet,
                "application/javascript"
                | "text/javascript"
                | "application/x-javascript"
                | "application/ecmascript" => return ResourceType::Script,
                "application/json" | "application/ld+json" => return ResourceType::Json,
                _ => {}
            }
            if mime.starts_with("image/") {
                return ResourceType::Image;
            }
            if mime.starts_with("font/")
                || mime == "application/font-woff"
                || mime == "application/x-font-ttf"
                || mime == "application/vnd.ms-fontobject"
            {
                return ResourceType::Font;
            }
            if mime.starts_with("audio/") || mime.starts_with("video/") {
                return ResourceType::Media;
            }
        }

        match ext.as_str() {
            "html" | "htm" => ResourceType::Document,
            "css" => ResourceType::Stylesheet,
            "js" | "mjs" | "cjs" => ResourceType::Script,
            "json" => ResourceType::Json,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "avif" => {
                ResourceType::Image
            }
            "woff" | "woff2" | "ttf" | "otf" | "eot" => ResourceType::Font,
            "mp3" | "mp4" | "wav" | "ogg" | "webm" | "m4a" | "mov" | "avi" => ResourceType::Media,
            _ => ResourceType::Other,
        }
    }
}

/// A single HTTP header (or query parameter) name/value pair.
///
/// Headers are kept as an ordered list (rather than a map) so repeated
/// header names and original ordering are preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderPair {
    /// Header (or query parameter) name.
    pub name: String,
    /// Header (or query parameter) value.
    pub value: String,
}

impl HeaderPair {
    /// Creates a new [`HeaderPair`] from any two `Into<String>` values.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Discriminates how [`BodyPayload::data`] should be interpreted.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BodyKind {
    /// `data` is UTF-8 text, stored verbatim.
    Text,
    /// `data` is binary content, base64-encoded.
    Base64,
    /// There is no body.
    #[default]
    None,
    /// The body was too large and has been truncated; `data` holds a
    /// (possibly text or base64) prefix.
    Truncated,
}

/// A captured (and possibly decoded/truncated) HTTP body, ready for JSON
/// transport to the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyPayload {
    /// How to interpret [`Self::data`].
    pub kind: BodyKind,
    /// UTF-8 text, or base64-encoded binary, depending on [`Self::kind`].
    pub data: String,
    /// Original (decoded, pre-truncation) body size in bytes.
    pub size: u64,
    /// Whether the stored `data` is a truncated prefix of the real body.
    pub truncated: bool,
    /// The original `Content-Encoding` of the body on the wire, if any
    /// (e.g. `"gzip"`), kept for informational purposes.
    pub encoding: Option<String>,
}

/// A captured HTTP request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    /// HTTP method, e.g. `"GET"`.
    pub method: String,
    /// Full request URL.
    pub url: String,
    /// HTTP version string, e.g. `"HTTP/1.1"`.
    pub http_version: String,
    /// Request headers, in wire order.
    pub headers: Vec<HeaderPair>,
    /// Request body.
    pub body: BodyPayload,
    /// Parsed query string parameters.
    pub query: Vec<HeaderPair>,
}

/// A captured HTTP response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseRecord {
    /// HTTP status code.
    pub status: u16,
    /// HTTP status reason phrase.
    pub status_text: String,
    /// HTTP version string, e.g. `"HTTP/1.1"`.
    pub http_version: String,
    /// Response headers, in wire order.
    pub headers: Vec<HeaderPair>,
    /// Response body.
    pub body: BodyPayload,
}

/// HAR-style timing breakdown for a flow, all values in milliseconds.
///
/// Following the HAR 1.2 convention, a value of `-1` means "not applicable".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    /// Time spent queued/blocked before the request could be sent.
    pub blocked: f64,
    /// DNS resolution time.
    pub dns: f64,
    /// TCP connection establishment time.
    pub connect: f64,
    /// TLS handshake time.
    pub ssl: f64,
    /// Time spent sending the request.
    pub send: f64,
    /// Time spent waiting for the first response byte.
    pub wait: f64,
    /// Time spent receiving the response body.
    pub receive: f64,
}

/// Direction of a captured WebSocket frame relative to the proxy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WsDirection {
    /// Client to server.
    Send,
    /// Server to client.
    Recv,
}

/// A single captured WebSocket message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsMessage {
    /// Direction the message travelled.
    pub direction: WsDirection,
    /// Frame opcode: `"text"`, `"binary"`, `"close"`, `"ping"`, or `"pong"`.
    pub opcode: String,
    /// Capture time, milliseconds since the Unix epoch.
    pub timestamp: i64,
    /// Message payload: UTF-8 text for text frames, base64 for binary frames.
    pub data: String,
    /// Original payload size in bytes.
    pub size: u64,
}

/// TLS session information captured for HTTPS/WSS flows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsInfo {
    /// Negotiated TLS protocol version, e.g. `"TLSv1.3"`.
    pub version: Option<String>,
    /// Negotiated cipher suite.
    pub cipher_suite: Option<String>,
    /// Negotiated ALPN protocol, e.g. `"h2"`.
    pub alpn: Option<String>,
    /// Server Name Indication sent by the client.
    pub sni: Option<String>,
    /// Subject of the peer (server) certificate.
    pub peer_cert_subject: Option<String>,
    /// Issuer of the peer (server) certificate.
    pub peer_cert_issuer: Option<String>,
    /// Certificate validity start, milliseconds since the Unix epoch.
    pub not_before: Option<i64>,
    /// Certificate validity end, milliseconds since the Unix epoch.
    pub not_after: Option<i64>,
}

/// Lightweight summary of a [`Flow`], suitable for list views.
///
/// This is intentionally cheap to clone/serialize: it excludes bodies and
/// other potentially large fields, which only live on the full [`Flow`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSummary {
    /// Unique flow identifier.
    pub id: FlowId,
    /// Monotonically increasing sequence number, used for incremental polling.
    pub seq: u64,
    /// Current lifecycle state.
    pub state: FlowState,
    /// Capture start time, milliseconds since the Unix epoch.
    pub started_at: i64,
    /// Total flow duration in milliseconds, once complete.
    pub duration_ms: Option<u64>,
    /// HTTP method.
    pub method: String,
    /// URL scheme: `"http"`, `"https"`, `"ws"`, or `"wss"`.
    pub scheme: String,
    /// Target host.
    pub host: String,
    /// Target port.
    pub port: u16,
    /// Path plus query string.
    pub path: String,
    /// Full request URL.
    pub url: String,
    /// HTTP version string.
    pub http_version: String,
    /// Response status code, once available.
    pub status: Option<u16>,
    /// Response status reason phrase, once available.
    pub status_text: Option<String>,
    /// Response MIME type, once available.
    pub mime_type: Option<String>,
    /// Inferred resource type.
    pub resource_type: ResourceType,
    /// Request body size in bytes.
    pub request_size: u64,
    /// Response body size in bytes.
    pub response_size: u64,
    /// Address of the client that issued the request.
    pub client_addr: String,
    /// Display name of the local application that issued the request
    /// (e.g. `"Google Chrome"`, `"Safari"`, `"curl"`), when it could be
    /// resolved. `None` for remote/LAN clients, replayed flows, or when
    /// resolution otherwise failed.
    pub app: Option<String>,
    /// IDs of rules that matched this flow.
    pub matched_rules: Vec<String>,
    /// Whether any rule modified this flow's request or response.
    pub modified: bool,
    /// Error message, if the flow ended in [`FlowState::Error`].
    pub error: Option<String>,
    /// Whether this flow is a WebSocket connection.
    pub websocket: bool,
    /// Whether the response was served locally by a `mockResponse` action.
    pub from_cache: bool,
}

/// A fully captured traffic record: summary plus request/response bodies,
/// timings, and (for modified flows) the original pre-rule snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flow {
    /// Lightweight summary fields, flattened into this struct's JSON.
    #[serde(flatten)]
    pub summary: FlowSummary,
    /// The (possibly rule-modified) request that was sent upstream.
    pub request: Option<RequestRecord>,
    /// The (possibly rule-modified) response received from upstream.
    pub response: Option<ResponseRecord>,
    /// The original request as received from the client, before any rule
    /// modified it. Only populated when [`FlowSummary::modified`] is true.
    pub original_request: Option<RequestRecord>,
    /// The original response as received from upstream, before any rule
    /// modified it. Only populated when [`FlowSummary::modified`] is true.
    pub original_response: Option<ResponseRecord>,
    /// Timing breakdown for this flow.
    pub timings: Timings,
    /// Captured WebSocket messages, in chronological order.
    pub ws_messages: Vec<WsMessage>,
    /// Address of the upstream server that was connected to.
    pub server_addr: Option<String>,
    /// TLS session info, for HTTPS/WSS flows.
    pub tls: Option<TlsInfo>,
}

impl Flow {
    /// Creates a new [`Flow`] in [`FlowState::Pending`] from an incoming
    /// request, assigning it the given `id`/`seq` and capture metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new_request(
        id: FlowId,
        seq: u64,
        started_at: i64,
        method: impl Into<String>,
        scheme: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        url: impl Into<String>,
        http_version: impl Into<String>,
        client_addr: impl Into<String>,
        request: RequestRecord,
    ) -> Self {
        let method = method.into();
        let scheme = scheme.into();
        let websocket = scheme == "ws" || scheme == "wss";
        let request_size = request.body.size;
        let summary = FlowSummary {
            id,
            seq,
            state: FlowState::Pending,
            started_at,
            duration_ms: None,
            method,
            scheme,
            host: host.into(),
            port,
            path: path.into(),
            url: url.into(),
            http_version: http_version.into(),
            status: None,
            status_text: None,
            mime_type: None,
            resource_type: ResourceType::Other,
            request_size,
            response_size: 0,
            client_addr: client_addr.into(),
            app: None,
            matched_rules: Vec::new(),
            modified: false,
            error: None,
            websocket,
            from_cache: false,
        };
        Flow {
            summary,
            request: Some(request),
            response: None,
            original_request: None,
            original_response: None,
            timings: Timings::default(),
            ws_messages: Vec::new(),
            server_addr: None,
            tls: None,
        }
    }

    /// Attaches a response to this flow and marks it complete, computing
    /// `duration_ms` from `started_at` and `finished_at`.
    pub fn mark_complete(&mut self, response: ResponseRecord, finished_at: i64) {
        self.summary.status = Some(response.status);
        self.summary.status_text = Some(response.status_text.clone());
        self.summary.mime_type = response
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-type"))
            .map(|h| {
                h.value
                    .split(';')
                    .next()
                    .unwrap_or(&h.value)
                    .trim()
                    .to_string()
            });
        self.summary.response_size = response.body.size;
        self.summary.resource_type =
            ResourceType::infer(self.summary.mime_type.as_deref(), &self.summary.path);
        self.summary.duration_ms = Some((finished_at - self.summary.started_at).max(0) as u64);
        self.summary.state = FlowState::Complete;
        self.response = Some(response);
    }

    /// Marks this flow as errored with the given message.
    pub fn mark_error(&mut self, message: impl Into<String>, finished_at: i64) {
        self.summary.state = FlowState::Error;
        self.summary.error = Some(message.into());
        self.summary.duration_ms = Some((finished_at - self.summary.started_at).max(0) as u64);
    }

    /// Returns a cloned [`FlowSummary`] for this flow.
    pub fn summary(&self) -> FlowSummary {
        self.summary.clone()
    }
}
