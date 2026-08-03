// Type-specific field editors for every Action variant. `update` mutates the
// action IN PLACE (via solid-js/store's `produce`, wired by ActionsEditor)
// so the store keeps the same object reference across keystrokes — critical
// for <For> not to remount the row (and drop input focus) on every edit.

import type { Component, JSX } from "solid-js";
import { For, Match, Show, Switch } from "solid-js";
import type { Action, HeaderPair, JsonOp } from "../../lib/types";
import Select from "../../components/Select";
import TextInput from "../../components/TextInput";
import Toggle from "../../components/Toggle";
import Button from "../../components/Button";
import { prettyJson } from "../../lib/format";
import { HTTP_METHODS, THROTTLE_PRESETS } from "./actionDefaults";

export interface ActionFieldsProps {
  action: Action;
  update: (mutator: (draft: Action) => void) => void;
  /** Capture-group count from the rule's own URL regex, for redirect's hint. */
  matcherCaptureCount: number;
}

function captureHint(count: number): string {
  return Array.from({ length: count }, (_, i) => `$${i + 1}`).join(", ");
}

const Field: Component<{ label: string; children: JSX.Element }> = (props) => (
  <label class="field">
    <span class="field__label">{props.label}</span>
    {props.children}
  </label>
);

const FindReplaceFields: Component<{
  find: string;
  replace: string;
  regex: boolean;
  onFind: (v: string) => void;
  onReplace: (v: string) => void;
  onRegex: (v: boolean) => void;
}> = (props) => (
  <div class="action-fields">
    <Field label="Find">
      <TextInput value={props.find} onInput={props.onFind} mono />
    </Field>
    <Field label="Replace">
      <TextInput value={props.replace} onInput={props.onReplace} mono />
    </Field>
    <Toggle checked={props.regex} onChange={props.onRegex} label="Treat find as regex" />
  </div>
);

const NameValueFields: Component<{
  name: string;
  value?: string;
  onName: (v: string) => void;
  onValue?: (v: string) => void;
  nameLabel?: string;
}> = (props) => (
  <div class="action-fields">
    <Field label={props.nameLabel ?? "Name"}>
      <TextInput value={props.name} onInput={props.onName} mono list="flproxy-header-names" />
    </Field>
    <Show when={props.onValue}>
      <Field label="Value">
        <TextInput value={props.value ?? ""} onInput={(v) => props.onValue?.(v)} mono />
      </Field>
    </Show>
  </div>
);

const BodyFields: Component<{
  body: string;
  encoding: "text" | "base64";
  contentType: string | null;
  onBody: (v: string) => void;
  onEncoding: (v: "text" | "base64") => void;
  onContentType: (v: string) => void;
}> = (props) => (
  <div class="action-fields">
    <div class="action-fields__row">
      <div class="seg" role="group" aria-label="Encoding">
        <button type="button" class={`seg__btn${props.encoding === "text" ? " seg__btn--active" : ""}`} onClick={() => props.onEncoding("text")}>
          Text
        </button>
        <button type="button" class={`seg__btn${props.encoding === "base64" ? " seg__btn--active" : ""}`} onClick={() => props.onEncoding("base64")}>
          Base64
        </button>
      </div>
      <TextInput value={props.contentType ?? ""} onInput={props.onContentType} placeholder="Content-Type (optional)" mono class="action-fields__content-type" />
      <Button variant="ghost" size="sm" onClick={() => props.onBody(prettyJson(props.body))}>
        Format JSON
      </Button>
    </div>
    <textarea class="action-fields__textarea mono" rows={6} value={props.body} onInput={(e) => props.onBody(e.currentTarget.value)} aria-label="Body" />
  </div>
);

const JsonPatchOpsFields: Component<{ ops: JsonOp[]; onOps: (mutate: (ops: JsonOp[]) => void) => void }> = (props) => {
  return (
    <div class="action-fields">
      <For each={props.ops}>
        {(op, i) => (
          <div class="json-patch-row">
            <Select
              value={op.op}
              onChange={(v) => props.onOps((ops) => { const o = ops[i()]; if (o) o.op = v as JsonOp["op"]; })}
              options={[
                { value: "set", label: "set" },
                { value: "remove", label: "remove" },
                { value: "merge", label: "merge" },
                { value: "append", label: "append" },
              ]}
            />
            <TextInput
              value={op.path}
              onInput={(v) => props.onOps((ops) => { const o = ops[i()]; if (o) o.path = v; })}
              placeholder="data.items[0].name"
              mono
              aria-label="JSON path"
            />
            <Show when={op.op !== "remove"}>
              <TextInput
                value={op.value === undefined ? "" : JSON.stringify(op.value)}
                onInput={(v) => {
                  props.onOps((ops) => {
                    const o = ops[i()];
                    if (!o) return;
                    try {
                      o.value = v.trim() === "" ? null : JSON.parse(v);
                    } catch {
                      // Leave the last valid value in place until the JSON is fixed.
                    }
                  });
                }}
                placeholder="JSON value"
                mono
                aria-label="JSON value"
              />
            </Show>
            <Button
              variant="ghost"
              size="sm"
              icon="close"
              aria-label="Remove op"
              onClick={() => props.onOps((ops) => ops.splice(i(), 1))}
            />
          </div>
        )}
      </For>
      <Button variant="ghost" size="sm" icon="plus" onClick={() => props.onOps((ops) => ops.push({ op: "set", path: "", value: null }))}>
        Add op
      </Button>
    </div>
  );
};

const MockHeadersFields: Component<{ headers: HeaderPair[]; onHeaders: (mutate: (h: HeaderPair[]) => void) => void }> = (props) => (
  <div class="action-fields">
    <For each={props.headers}>
      {(h, i) => (
        <div class="mock-header-row">
          <TextInput
            value={h.name}
            onInput={(v) => props.onHeaders((hs) => { const row = hs[i()]; if (row) row.name = v; })}
            placeholder="Header name"
            mono
            list="flproxy-header-names"
            aria-label="Header name"
          />
          <TextInput
            value={h.value}
            onInput={(v) => props.onHeaders((hs) => { const row = hs[i()]; if (row) row.value = v; })}
            placeholder="Value"
            mono
            aria-label="Header value"
          />
          <Button variant="ghost" size="sm" icon="close" aria-label="Remove header" onClick={() => props.onHeaders((hs) => hs.splice(i(), 1))} />
        </div>
      )}
    </For>
    <Button variant="ghost" size="sm" icon="plus" onClick={() => props.onHeaders((hs) => hs.push({ name: "", value: "" }))}>
      Add header
    </Button>
  </div>
);

const ActionFields: Component<ActionFieldsProps> = (props) => {
  return (
    <Switch fallback={<div class="action-fields__note">Unknown action type</div>}>
      <Match when={props.action.type === "redirect"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "redirect" }>;
          return (
            <div class="action-fields">
              <Field label="Redirect to">
                <TextInput
                  value={a.to}
                  onInput={(v) => props.update((d) => { (d as Extract<Action, { type: "redirect" }>).to = v; })}
                  mono
                  placeholder="https://staging.example.com/$1"
                />
              </Field>
              <Show when={props.matcherCaptureCount > 0}>
                <div class="action-fields__hint">Use {captureHint(props.matcherCaptureCount)} from the URL regex above.</div>
              </Show>
            </div>
          );
        })()}
      </Match>

      <Match when={props.action.type === "rewriteUrl"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "rewriteUrl" }>;
          return (
            <FindReplaceFields
              find={a.find}
              replace={a.replace}
              regex={a.regex}
              onFind={(v) => props.update((d) => { (d as Extract<Action, { type: "rewriteUrl" }>).find = v; })}
              onReplace={(v) => props.update((d) => { (d as Extract<Action, { type: "rewriteUrl" }>).replace = v; })}
              onRegex={(v) => props.update((d) => { (d as Extract<Action, { type: "rewriteUrl" }>).regex = v; })}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "setQueryParam"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setQueryParam" }>;
          return (
            <NameValueFields
              name={a.name}
              value={a.value}
              onName={(v) => props.update((d) => { (d as Extract<Action, { type: "setQueryParam" }>).name = v; })}
              onValue={(v) => props.update((d) => { (d as Extract<Action, { type: "setQueryParam" }>).value = v; })}
              nameLabel="Param name"
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "removeQueryParam"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "removeQueryParam" }>;
          return (
            <NameValueFields
              name={a.name}
              onName={(v) => props.update((d) => { (d as Extract<Action, { type: "removeQueryParam" }>).name = v; })}
              nameLabel="Param name"
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "setRequestHeader"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setRequestHeader" }>;
          return (
            <NameValueFields
              name={a.name}
              value={a.value}
              onName={(v) => props.update((d) => { (d as Extract<Action, { type: "setRequestHeader" }>).name = v; })}
              onValue={(v) => props.update((d) => { (d as Extract<Action, { type: "setRequestHeader" }>).value = v; })}
              nameLabel="Header name"
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "removeRequestHeader"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "removeRequestHeader" }>;
          return (
            <NameValueFields
              name={a.name}
              onName={(v) => props.update((d) => { (d as Extract<Action, { type: "removeRequestHeader" }>).name = v; })}
              nameLabel="Header name"
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "setResponseHeader"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setResponseHeader" }>;
          return (
            <NameValueFields
              name={a.name}
              value={a.value}
              onName={(v) => props.update((d) => { (d as Extract<Action, { type: "setResponseHeader" }>).name = v; })}
              onValue={(v) => props.update((d) => { (d as Extract<Action, { type: "setResponseHeader" }>).value = v; })}
              nameLabel="Header name"
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "removeResponseHeader"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "removeResponseHeader" }>;
          return (
            <NameValueFields
              name={a.name}
              onName={(v) => props.update((d) => { (d as Extract<Action, { type: "removeResponseHeader" }>).name = v; })}
              nameLabel="Header name"
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "setRequestBody"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setRequestBody" }>;
          return (
            <BodyFields
              body={a.body}
              encoding={a.encoding}
              contentType={a.contentType}
              onBody={(v) => props.update((d) => { (d as Extract<Action, { type: "setRequestBody" }>).body = v; })}
              onEncoding={(v) => props.update((d) => { (d as Extract<Action, { type: "setRequestBody" }>).encoding = v; })}
              onContentType={(v) => props.update((d) => { (d as Extract<Action, { type: "setRequestBody" }>).contentType = v || null; })}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "setResponseBody"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setResponseBody" }>;
          return (
            <BodyFields
              body={a.body}
              encoding={a.encoding}
              contentType={a.contentType}
              onBody={(v) => props.update((d) => { (d as Extract<Action, { type: "setResponseBody" }>).body = v; })}
              onEncoding={(v) => props.update((d) => { (d as Extract<Action, { type: "setResponseBody" }>).encoding = v; })}
              onContentType={(v) => props.update((d) => { (d as Extract<Action, { type: "setResponseBody" }>).contentType = v || null; })}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "replaceInRequestBody"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "replaceInRequestBody" }>;
          return (
            <FindReplaceFields
              find={a.find}
              replace={a.replace}
              regex={a.regex}
              onFind={(v) => props.update((d) => { (d as Extract<Action, { type: "replaceInRequestBody" }>).find = v; })}
              onReplace={(v) => props.update((d) => { (d as Extract<Action, { type: "replaceInRequestBody" }>).replace = v; })}
              onRegex={(v) => props.update((d) => { (d as Extract<Action, { type: "replaceInRequestBody" }>).regex = v; })}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "replaceInResponseBody"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "replaceInResponseBody" }>;
          return (
            <FindReplaceFields
              find={a.find}
              replace={a.replace}
              regex={a.regex}
              onFind={(v) => props.update((d) => { (d as Extract<Action, { type: "replaceInResponseBody" }>).find = v; })}
              onReplace={(v) => props.update((d) => { (d as Extract<Action, { type: "replaceInResponseBody" }>).replace = v; })}
              onRegex={(v) => props.update((d) => { (d as Extract<Action, { type: "replaceInResponseBody" }>).regex = v; })}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "jsonPatchRequest"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "jsonPatchRequest" }>;
          return (
            <JsonPatchOpsFields
              ops={a.ops}
              onOps={(mutate) => props.update((d) => mutate((d as Extract<Action, { type: "jsonPatchRequest" }>).ops))}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "jsonPatchResponse"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "jsonPatchResponse" }>;
          return (
            <JsonPatchOpsFields
              ops={a.ops}
              onOps={(mutate) => props.update((d) => mutate((d as Extract<Action, { type: "jsonPatchResponse" }>).ops))}
            />
          );
        })()}
      </Match>

      <Match when={props.action.type === "mockResponse"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "mockResponse" }>;
          type Mock = Extract<Action, { type: "mockResponse" }>;
          const applyPreset = (contentType: string, sampleBody: string) => {
            props.update((d) => {
              const m = d as Mock;
              const existing = m.headers.find((h) => h.name.toLowerCase() === "content-type");
              if (existing) existing.value = contentType;
              else m.headers.push({ name: "Content-Type", value: contentType });
              if (!m.body.trim()) m.body = sampleBody;
            });
          };
          return (
            <div class="action-fields">
              <Field label="Status">
                <TextInput
                  type="number"
                  value={String(a.status)}
                  onInput={(v) => props.update((d) => { (d as Mock).status = Number(v) || 0; })}
                  mono
                  class="action-fields__number"
                />
              </Field>
              <MockHeadersFields headers={a.headers} onHeaders={(mutate) => props.update((d) => mutate((d as Mock).headers))} />
              <div class="action-fields__row">
                <Button variant="ghost" size="sm" onClick={() => applyPreset("application/json", "{}")}>
                  application/json preset
                </Button>
                <Button variant="ghost" size="sm" onClick={() => applyPreset("text/plain", "OK")}>
                  text/plain preset
                </Button>
              </div>
              <BodyFields
                body={a.body}
                encoding={a.encoding}
                contentType={null}
                onBody={(v) => props.update((d) => { (d as Mock).body = v; })}
                onEncoding={(v) => props.update((d) => { (d as Mock).encoding = v; })}
                onContentType={() => {}}
              />
              <Field label="Delay (ms)">
                <TextInput
                  type="number"
                  value={String(a.delayMs)}
                  onInput={(v) => props.update((d) => { (d as Mock).delayMs = Number(v) || 0; })}
                  mono
                  class="action-fields__number"
                />
              </Field>
            </div>
          );
        })()}
      </Match>

      <Match when={props.action.type === "setStatus"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setStatus" }>;
          return (
            <Field label="Status code">
              <TextInput
                type="number"
                value={String(a.status)}
                onInput={(v) => props.update((d) => { (d as Extract<Action, { type: "setStatus" }>).status = Number(v) || 0; })}
                mono
                class="action-fields__number"
              />
            </Field>
          );
        })()}
      </Match>

      <Match when={props.action.type === "block"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "block" }>;
          return (
            <Field label="Reason">
              <TextInput
                value={a.reason}
                onInput={(v) => props.update((d) => { (d as Extract<Action, { type: "block" }>).reason = v; })}
              />
            </Field>
          );
        })()}
      </Match>

      <Match when={props.action.type === "delay"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "delay" }>;
          return (
            <Field label="Delay (ms)">
              <TextInput
                type="number"
                value={String(a.ms)}
                onInput={(v) => props.update((d) => { (d as Extract<Action, { type: "delay" }>).ms = Number(v) || 0; })}
                mono
                class="action-fields__number"
              />
            </Field>
          );
        })()}
      </Match>

      <Match when={props.action.type === "throttle"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "throttle" }>;
          const presetValue = () => THROTTLE_PRESETS.find((p) => p.bytesPerSec === a.bytesPerSec)?.bytesPerSec ?? -1;
          return (
            <div class="action-fields">
              <Field label="Preset">
                <Select
                  value={String(presetValue())}
                  onChange={(v) => {
                    if (v === "-1") return;
                    props.update((d) => { (d as Extract<Action, { type: "throttle" }>).bytesPerSec = Number(v); });
                  }}
                  options={[
                    ...THROTTLE_PRESETS.map((p) => ({ value: String(p.bytesPerSec), label: p.label })),
                    { value: "-1", label: "Custom" },
                  ]}
                />
              </Field>
              <Field label="Bytes/sec">
                <TextInput
                  type="number"
                  value={String(a.bytesPerSec)}
                  onInput={(v) => props.update((d) => { (d as Extract<Action, { type: "throttle" }>).bytesPerSec = Number(v) || 0; })}
                  mono
                  class="action-fields__number"
                />
              </Field>
            </div>
          );
        })()}
      </Match>

      <Match when={props.action.type === "setMethod"}>
        {(() => {
          const a = props.action as Extract<Action, { type: "setMethod" }>;
          return (
            <Field label="Method">
              <Select
                value={a.method}
                onChange={(v) => props.update((d) => { (d as Extract<Action, { type: "setMethod" }>).method = v; })}
                options={HTTP_METHODS.map((m) => ({ value: m, label: m }))}
              />
            </Field>
          );
        })()}
      </Match>
    </Switch>
  );
};

export default ActionFields;
