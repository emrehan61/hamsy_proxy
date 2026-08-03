// Right-pane tabbed flow inspector.

import type { Component } from "solid-js";
import { For, Match, Show, Switch, createMemo, createSignal } from "solid-js";
import type { BodyPayload, Flow, FlowSummary, HeaderPair, Timings } from "../lib/types";
import { formatBytes, formatDateShort, formatDuration, formatTimestamp, methodColor, statusColor } from "../lib/format";
import Tabs, { type TabItem } from "./Tabs";
import TimingsBar from "./TimingsBar";
import HeadersTable from "./HeadersTable";
import BodyViewer from "./BodyViewer";
import JsonTree from "./JsonTree";
import Icon from "./Icon";

export interface FlowDetailProps {
  flow: Flow | undefined;
  /**
   * Optional lighter-weight fallback while the full `Flow` is still
   * loading. Not used below — the loading/empty state (`flow === undefined`
   * shows "Select a flow to inspect it") covers that case simply enough
   * that threading a second, partial data shape through every tab wasn't
   * worth the complexity. Kept in the prop signature per spec so callers
   * that already have a `FlowSummary` handy can pass it without a type
   * error, in case a later phase does want to use it.
   */
  flowSummary?: FlowSummary;
}

const TIMING_PHASES: { key: keyof Timings; label: string }[] = [
  { key: "blocked", label: "Blocked" },
  { key: "dns", label: "DNS" },
  { key: "connect", label: "Connect" },
  { key: "ssl", label: "SSL" },
  { key: "send", label: "Send" },
  { key: "wait", label: "Wait" },
  { key: "receive", label: "Receive" },
];

function rawBodyText(body: BodyPayload | null | undefined): string {
  if (!body || body.kind === "none") return "";
  if (body.kind === "text") return body.data;
  if (body.kind === "truncated" && (body.encoding === "text" || body.encoding === null)) return body.data;
  return "[binary body omitted]";
}

function rawHeaders(headers: HeaderPair[]): string {
  return headers.map((h) => `${h.name}: ${h.value}`).join("\n");
}

const OverviewTab: Component<{ flow: Flow }> = (props) => {
  return (
    <div class="flow-detail__overview">
      <div class="flow-detail__url mono">{props.flow.url}</div>
      <div class="flow-detail__badges">
        <span class="flow-detail__badge mono" style={{ color: methodColor(props.flow.method) }}>
          {props.flow.method}
        </span>
        <span class="flow-detail__badge mono" style={{ color: statusColor(props.flow.status) }}>
          {props.flow.status ?? "-"} {props.flow.statusText ?? ""}
        </span>
        <span class="flow-detail__badge">{props.flow.resourceType}</span>
        <Show when={props.flow.fromCache}>
          <span class="flow-detail__badge flow-detail__badge--muted">from cache</span>
        </Show>
      </div>

      <TimingsBar timings={props.flow.timings} total={props.flow.durationMs ?? undefined} />

      <Show when={props.flow.tls}>
        {(tls) => (
          <div class="flow-detail__section">
            <h3 class="flow-detail__section-title">TLS</h3>
            <dl class="flow-detail__kv">
              <dt>Version</dt>
              <dd class="mono">{tls().version ?? "-"}</dd>
              <dt>Cipher suite</dt>
              <dd class="mono">{tls().cipherSuite ?? "-"}</dd>
              <dt>ALPN</dt>
              <dd class="mono">{tls().alpn ?? "-"}</dd>
              <dt>SNI</dt>
              <dd class="mono">{tls().sni ?? "-"}</dd>
              <dt>Certificate subject</dt>
              <dd class="mono">{tls().peerCertSubject ?? "-"}</dd>
              <dt>Certificate issuer</dt>
              <dd class="mono">{tls().peerCertIssuer ?? "-"}</dd>
              <dt>Valid</dt>
              <dd class="mono">
                {tls().notBefore !== null ? formatDateShort(tls().notBefore as number) : "-"}
                {" – "}
                {tls().notAfter !== null ? formatDateShort(tls().notAfter as number) : "-"}
              </dd>
            </dl>
          </div>
        )}
      </Show>

      <Show when={props.flow.matchedRules.length > 0}>
        <div class="flow-detail__section">
          <h3 class="flow-detail__section-title">Matched rules</h3>
          <div class="flow-detail__chips">
            <For each={props.flow.matchedRules}>{(rule) => <span class="flow-detail__chip">{rule}</span>}</For>
          </div>
        </div>
      </Show>

      <div class="flow-detail__section">
        <h3 class="flow-detail__section-title">Size</h3>
        <dl class="flow-detail__kv">
          <dt>Request</dt>
          <dd class="mono">{formatBytes(props.flow.requestSize)}</dd>
          <dt>Response</dt>
          <dd class="mono">{formatBytes(props.flow.responseSize)}</dd>
        </dl>
      </div>
    </div>
  );
};

const RequestTab: Component<{ flow: Flow }> = (props) => (
  <div class="flow-detail__tab-content">
    <HeadersTable headers={props.flow.request?.headers ?? []} title="Headers" />
    <HeadersTable headers={props.flow.request?.query ?? []} title="Query parameters" />
    <BodyViewer body={props.flow.request?.body} headers={props.flow.request?.headers} />
  </div>
);

const ResponseTab: Component<{ flow: Flow }> = (props) => (
  <div class="flow-detail__tab-content">
    <HeadersTable headers={props.flow.response?.headers ?? []} title="Headers" />
    <BodyViewer body={props.flow.response?.body} headers={props.flow.response?.headers} />
  </div>
);

const TimingsTab: Component<{ flow: Flow }> = (props) => (
  <div class="flow-detail__tab-content">
    <TimingsBar timings={props.flow.timings} total={props.flow.durationMs ?? undefined} />
    <table class="flow-detail__timings-table">
      <thead>
        <tr>
          <th>Phase</th>
          <th>Duration</th>
        </tr>
      </thead>
      <tbody>
        <For each={TIMING_PHASES}>
          {(phase) => (
            <tr>
              <td>{phase.label}</td>
              <td class="mono">{formatDuration(props.flow.timings[phase.key])}</td>
            </tr>
          )}
        </For>
      </tbody>
    </table>
  </div>
);

const RawTab: Component<{ flow: Flow }> = (props) => {
  const requestRaw = createMemo(() => {
    const req = props.flow.request;
    if (!req) return "(no request captured)";
    const line = `${req.method} ${req.url} HTTP/${req.httpVersion}`;
    return `${line}\n${rawHeaders(req.headers)}\n\n${rawBodyText(req.body)}`;
  });
  const responseRaw = createMemo(() => {
    const res = props.flow.response;
    if (!res) return "(no response captured)";
    const line = `HTTP/${res.httpVersion} ${res.status} ${res.statusText}`;
    return `${line}\n${rawHeaders(res.headers)}\n\n${rawBodyText(res.body)}`;
  });
  return (
    <div class="flow-detail__tab-content">
      <h3 class="flow-detail__section-title">Request</h3>
      <pre class="mono flow-detail__raw">{requestRaw()}</pre>
      <h3 class="flow-detail__section-title">Response</h3>
      <pre class="mono flow-detail__raw">{responseRaw()}</pre>
    </div>
  );
};

// Direction mapping (documented since WsMessage.direction is otherwise
// ambiguous): "send" = client -> server (outgoing from the proxied client's
// point of view), shown with an up arrow; "recv" = server -> client, shown
// with a down arrow.
const WebSocketTab: Component<{ flow: Flow }> = (props) => (
  <div class="flow-detail__tab-content">
    <Show
      when={props.flow.wsMessages.length > 0}
      fallback={<div class="flow-detail__note">No WebSocket messages captured.</div>}
    >
      <ul class="flow-detail__ws-list">
        <For each={props.flow.wsMessages}>
          {(msg) => {
            const parsed = createMemo<unknown | undefined>(() => {
              try {
                return JSON.parse(msg.data) as unknown;
              } catch {
                return undefined;
              }
            });
            return (
              <li class="flow-detail__ws-message">
                <div class="flow-detail__ws-meta">
                  <Icon name={msg.direction === "send" ? "arrow-up" : "arrow-down"} size={14} />
                  <span class="mono">{msg.opcode}</span>
                  <span class="mono">{formatTimestamp(msg.timestamp)}</span>
                  <span class="mono">{formatBytes(msg.size)}</span>
                </div>
                <Show when={parsed() !== undefined} fallback={<pre class="mono flow-detail__ws-data">{msg.data}</pre>}>
                  <JsonTree data={parsed()} />
                </Show>
              </li>
            );
          }}
        </For>
      </ul>
    </Show>
  </div>
);

const FlowDetail: Component<FlowDetailProps> = (props) => {
  const [active, setActive] = createSignal("overview");

  const tabs = createMemo<TabItem[]>(() => {
    const base: TabItem[] = [
      { id: "overview", label: "Overview" },
      { id: "request", label: "Request" },
      { id: "response", label: "Response" },
      { id: "timings", label: "Timings" },
      { id: "raw", label: "Raw" },
    ];
    if (props.flow?.websocket) base.push({ id: "websocket", label: "WebSocket", badge: props.flow.wsMessages.length });
    return base;
  });

  return (
    <Show when={props.flow} fallback={<div class="placeholder">Select a flow to inspect it</div>}>
      {(flow) => (
        <Tabs tabs={tabs()} active={active()} onChange={setActive}>
          <Switch>
            <Match when={active() === "overview"}>
              <OverviewTab flow={flow()} />
            </Match>
            <Match when={active() === "request"}>
              <RequestTab flow={flow()} />
            </Match>
            <Match when={active() === "response"}>
              <ResponseTab flow={flow()} />
            </Match>
            <Match when={active() === "timings"}>
              <TimingsTab flow={flow()} />
            </Match>
            <Match when={active() === "raw"}>
              <RawTab flow={flow()} />
            </Match>
            <Match when={active() === "websocket"}>
              <WebSocketTab flow={flow()} />
            </Match>
          </Switch>
        </Tabs>
      )}
    </Show>
  );
};

export default FlowDetail;
