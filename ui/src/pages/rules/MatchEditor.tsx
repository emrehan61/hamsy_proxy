// The "Match" section of the rule editor: URL condition (with regex
// validation + capture-group hints), method/resource-type chips, host:port
// and status-code repeatable lists, header/body conditions, and a live
// match tester that runs src/lib/matcher.ts against a URL/method/status the
// user types in (prefilled from the currently selected Traffic flow, if
// any).

import type { Component } from "solid-js";
import { For, Show, createMemo, createSignal } from "solid-js";
import type { SetStoreFunction } from "solid-js/store";
import type { HeaderCond, HeaderOp, Matcher, ResourceType, Rule, UrlOp } from "../../lib/types";
import Select from "../../components/Select";
import TextInput from "../../components/TextInput";
import Button from "../../components/Button";
import RepeatableInputList from "../../components/RepeatableInputList";
import { countCaptureGroups, isValidStatusPattern, testMatcher, validateRegex } from "../../lib/matcher";
import { getFlow as getFlowSummary, selectedId as trafficSelectedFlowId } from "../../stores/flows";
import { HTTP_METHODS } from "./actionDefaults";

const URL_OP_OPTIONS: { value: UrlOp; label: string }[] = [
  { value: "any", label: "any" },
  { value: "contains", label: "contains" },
  { value: "equals", label: "equals" },
  { value: "startsWith", label: "starts with" },
  { value: "endsWith", label: "ends with" },
  { value: "regex", label: "regex" },
  { value: "wildcard", label: "wildcard" },
];

const HEADER_OP_OPTIONS: { value: HeaderOp; label: string }[] = [
  { value: "exists", label: "exists" },
  { value: "absent", label: "absent" },
  { value: "equals", label: "equals" },
  { value: "contains", label: "contains" },
  { value: "regex", label: "regex" },
];

const BODY_OP_OPTIONS = [
  { value: "contains", label: "contains" },
  { value: "regex", label: "regex" },
  { value: "equals", label: "equals" },
];

const RESOURCE_TYPES: ResourceType[] = [
  "document",
  "stylesheet",
  "script",
  "image",
  "font",
  "xhr",
  "json",
  "media",
  "webSocket",
  "other",
];

function toggleValue<T>(list: T[], value: T): T[] {
  return list.includes(value) ? list.filter((v) => v !== value) : [...list, value];
}

export interface MatchEditorProps {
  match: Matcher;
  setRule: SetStoreFunction<Rule>;
}

const MatchEditor: Component<MatchEditorProps> = (props) => {
  const regexError = createMemo(() => (props.match.urlOp === "regex" ? validateRegex(props.match.urlValue) : null));
  const captureCount = createMemo(() =>
    props.match.urlOp === "regex" && !regexError() ? countCaptureGroups(props.match.urlValue) : 0,
  );

  const renderHeaderConds = (list: HeaderCond[], key: "requestHeaders" | "responseHeaders") => {
    const addRow = () => props.setRule("match", key, (arr) => [...arr, { name: "", op: "exists" as HeaderOp, value: null }]);
    const removeRow = (i: number) => props.setRule("match", key, (arr) => arr.filter((_, idx) => idx !== i));
    return (
      <div class="match-editor__cond-list">
        <For each={list}>
          {(cond, i) => {
            const valueDisabled = () => cond.op === "exists" || cond.op === "absent";
            return (
              <div class="match-editor__cond-row">
                <TextInput
                  value={cond.name}
                  onInput={(v) => props.setRule("match", key, i(), "name", v)}
                  placeholder="Header name"
                  mono
                  list="hamsy-header-names"
                  aria-label="Header name"
                />
                <Select
                  value={cond.op}
                  onChange={(v) => props.setRule("match", key, i(), "op", v as HeaderOp)}
                  options={HEADER_OP_OPTIONS}
                />
                <TextInput
                  value={cond.value ?? ""}
                  onInput={(v) => props.setRule("match", key, i(), "value", v)}
                  placeholder="Value"
                  mono
                  disabled={valueDisabled()}
                  aria-label="Header value"
                />
                <Button variant="ghost" size="sm" icon="close" aria-label="Remove condition" onClick={() => removeRow(i())} />
              </div>
            );
          }}
        </For>
        <Button variant="ghost" size="sm" icon="plus" onClick={addRow}>
          Add condition
        </Button>
      </div>
    );
  };

  const renderBodyCond = (kind: "requestBody" | "responseBody") => {
    const cond = () => props.match[kind];
    return (
      <Show
        when={cond()}
        fallback={
          <Button
            variant="ghost"
            size="sm"
            icon="plus"
            onClick={() => props.setRule("match", kind, { op: "contains", value: "" })}
          >
            Add condition
          </Button>
        }
      >
        {(c) => (
          <div class="match-editor__body-cond">
            <Select
              value={c().op}
              onChange={(v) => props.setRule("match", kind, "op", v as "contains" | "regex" | "equals")}
              options={BODY_OP_OPTIONS}
            />
            <textarea
              class="match-editor__body-textarea mono"
              value={c().value}
              onInput={(e) => props.setRule("match", kind, "value", e.currentTarget.value)}
              rows={3}
              aria-label={`${kind} value`}
            />
            <Button variant="ghost" size="sm" icon="close" onClick={() => props.setRule("match", kind, null)}>
              Clear
            </Button>
          </div>
        )}
      </Show>
    );
  };

  // ---- live match tester (URL/method/status only — see matcher.ts) ----
  const initialFlow = () => {
    const id = trafficSelectedFlowId();
    return id ? getFlowSummary(id) : undefined;
  };
  const [testUrl, setTestUrl] = createSignal(initialFlow()?.url ?? "");
  const [testMethod, setTestMethod] = createSignal(initialFlow()?.method ?? "GET");
  const [testStatus, setTestStatus] = createSignal(
    initialFlow()?.status !== undefined && initialFlow()?.status !== null ? String(initialFlow()?.status) : "",
  );

  const checks = createMemo(() =>
    testMatcher(props.match, {
      url: testUrl(),
      method: testMethod(),
      status: testStatus().trim() === "" ? null : Number(testStatus()),
    }),
  );

  return (
    <div class="match-editor">
      <div class="rule-section">
        <div class="rule-section__title">URL</div>
        <div class="match-editor__url-row">
          <Select
            value={props.match.urlOp}
            onChange={(v) => props.setRule("match", "urlOp", v as UrlOp)}
            options={URL_OP_OPTIONS}
            class="match-editor__url-op"
          />
          <TextInput
            value={props.match.urlValue}
            onInput={(v) => props.setRule("match", "urlValue", v)}
            placeholder="https://api.example.com/"
            mono
            disabled={props.match.urlOp === "any"}
          />
        </div>
        <Show when={regexError()}>{(msg) => <div class="match-editor__error">{msg()}</div>}</Show>
        <Show when={captureCount() > 0}>
          <div class="match-editor__hint">
            Capture groups available downstream:{" "}
            {Array.from({ length: captureCount() }, (_, i) => `$${i + 1}`).join(", ")}
          </div>
        </Show>
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Methods</div>
        <div class="chip-row" role="group" aria-label="Methods">
          <For each={HTTP_METHODS}>
            {(m) => (
              <button
                type="button"
                class={`chip${props.match.methods.includes(m) ? " chip--active" : ""}`}
                aria-pressed={props.match.methods.includes(m)}
                onClick={() => props.setRule("match", "methods", toggleValue(props.match.methods, m))}
              >
                {m}
              </button>
            )}
          </For>
        </div>
        <div class="match-editor__note">No methods selected matches any method.</div>
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Host:port globs</div>
        <RepeatableInputList
          values={props.match.hostPorts}
          onChange={(v) => props.setRule("match", "hostPorts", v)}
          placeholder="*.example.com:443"
          mono
          addLabel="Add host:port"
          aria-label="Host:port glob"
        />
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Status codes</div>
        <RepeatableInputList
          values={props.match.statusCodes}
          onChange={(v) => props.setRule("match", "statusCodes", v)}
          placeholder="200, 4xx, 500-599"
          mono
          addLabel="Add status pattern"
          aria-label="Status pattern"
          validate={(v) => (isValidStatusPattern(v) ? null : "Use 200, 4xx, or 500-599")}
        />
        <div class="match-editor__note">Only evaluated once the response is known.</div>
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Resource types</div>
        <div class="chip-row" role="group" aria-label="Resource types">
          <For each={RESOURCE_TYPES}>
            {(rt) => (
              <button
                type="button"
                class={`chip${props.match.resourceTypes.includes(rt) ? " chip--active" : ""}`}
                aria-pressed={props.match.resourceTypes.includes(rt)}
                onClick={() => props.setRule("match", "resourceTypes", toggleValue(props.match.resourceTypes, rt))}
              >
                {rt}
              </button>
            )}
          </For>
        </div>
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Request headers</div>
        {renderHeaderConds(props.match.requestHeaders, "requestHeaders")}
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Response headers</div>
        {renderHeaderConds(props.match.responseHeaders, "responseHeaders")}
        <div class="match-editor__note">Only evaluated once the response is known.</div>
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Request body</div>
        {renderBodyCond("requestBody")}
      </div>

      <div class="rule-section">
        <div class="rule-section__title">Response body</div>
        {renderBodyCond("responseBody")}
        <div class="match-editor__note">Only evaluated once the response is known.</div>
      </div>

      <div class="rule-section match-tester">
        <div class="rule-section__title">Live match tester</div>
        <div class="match-tester__inputs">
          <TextInput value={testUrl()} onInput={setTestUrl} placeholder="Test URL" mono class="match-tester__url" />
          <Select value={testMethod()} onChange={setTestMethod} options={HTTP_METHODS.map((m) => ({ value: m, label: m }))} />
          <TextInput value={testStatus()} onInput={setTestStatus} placeholder="Status (optional)" mono class="match-tester__status" />
        </div>
        <Show when={checks().length > 0} fallback={<div class="match-editor__note">No URL/method/status conditions to test.</div>}>
          <ul class="match-tester__results">
            <For each={checks()}>
              {(c) => (
                <li class={`match-tester__result${c.passed ? " match-tester__result--pass" : " match-tester__result--fail"}`}>
                  <span class="match-tester__icon" aria-hidden="true">
                    {c.passed ? "✓" : "✗"}
                  </span>
                  <span>{c.label}</span>
                  <Show when={c.detail}>{(d) => <span class="match-tester__detail">{d()}</span>}</Show>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </div>
    </div>
  );
};

export default MatchEditor;
