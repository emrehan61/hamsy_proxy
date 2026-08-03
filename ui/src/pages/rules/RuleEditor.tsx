// The right-hand editor pane: header (name/enabled/priority/group +
// save/revert/delete), a Visual/JSON tab strip, and the Match/Actions
// sections. Edits happen on a local `draft` store cloned from `props.rule`;
// nothing is persisted until Save. Cmd/Ctrl+S saves; unsaved changes are
// reported upward via `onDirtyChange` so the list/router can warn before
// switching away.

import type { Component } from "solid-js";
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import type { Rule } from "../../lib/types";
import TextInput from "../../components/TextInput";
import Toggle from "../../components/Toggle";
import Button from "../../components/Button";
import Tabs from "../../components/Tabs";
import { pushToast } from "../../stores/ui";
import { deleteRule as deleteRuleAction, rules, updateRule as updateRuleAction } from "../../stores/rules";
import MatchEditor from "./MatchEditor";
import ActionsEditor from "./ActionsEditor";
import { COMMON_HEADER_NAMES } from "./actionDefaults";

export interface RuleEditorProps {
  rule: Rule;
  onDirtyChange: (dirty: boolean) => void;
  onDeleted: () => void;
}

function cloneRule(r: Rule): Rule {
  return JSON.parse(JSON.stringify(r)) as Rule;
}

function isRuleLike(v: unknown): v is Rule {
  if (typeof v !== "object" || v === null) return false;
  const r = v as Partial<Rule>;
  return typeof r.name === "string" && typeof r.match === "object" && r.match !== null && Array.isArray(r.actions);
}

const RuleEditor: Component<RuleEditorProps> = (props) => {
  const [draft, setDraft] = createStore<Rule>(cloneRule(props.rule));
  const [tab, setTab] = createSignal<"visual" | "json">("visual");
  const [jsonText, setJsonText] = createSignal(JSON.stringify(props.rule, null, 2));
  const [jsonError, setJsonError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);

  // Reset the draft whenever the SELECTED rule changes (by id) — not on
  // every re-render of the same rule, so a live `rulesChanged` refetch
  // can't clobber in-progress edits to the rule currently open.
  createEffect(
    on(
      () => props.rule.id,
      () => {
        setDraft(reconcile(cloneRule(props.rule)));
        setJsonText(JSON.stringify(props.rule, null, 2));
        setJsonError(null);
        setTab("visual");
      },
    ),
  );

  const dirty = createMemo(() => JSON.stringify(unwrap(draft)) !== JSON.stringify(props.rule));

  createEffect(() => props.onDirtyChange(dirty()));
  onCleanup(() => props.onDirtyChange(false));

  const groups = createMemo(() => Array.from(new Set(rules().map((r) => r.group).filter((g): g is string => !!g))));

  const save = async () => {
    if (saving()) return;
    setSaving(true);
    try {
      const saved = await updateRuleAction(draft.id, unwrap(draft));
      setDraft(reconcile(cloneRule(saved)));
      setJsonText(JSON.stringify(saved, null, 2));
      pushToast({ level: "success", message: `Saved "${saved.name}"` });
    } catch {
      pushToast({ level: "error", message: "Failed to save rule" });
    } finally {
      setSaving(false);
    }
  };

  const revert = () => {
    setDraft(reconcile(cloneRule(props.rule)));
    setJsonText(JSON.stringify(props.rule, null, 2));
    setJsonError(null);
  };

  const doDelete = async () => {
    if (!confirm(`Delete rule "${draft.name}"? This cannot be undone.`)) return;
    try {
      await deleteRuleAction(draft.id);
      props.onDirtyChange(false);
      pushToast({ level: "success", message: "Rule deleted" });
      props.onDeleted();
    } catch {
      pushToast({ level: "error", message: "Failed to delete rule" });
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      if (dirty()) void save();
    }
  };

  onMount(() => {
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  const onJsonInput = (text: string) => {
    setJsonText(text);
    try {
      const parsed: unknown = JSON.parse(text);
      if (!isRuleLike(parsed)) {
        setJsonError("Must be a rule object with name, match, and actions");
        return;
      }
      setJsonError(null);
      // Preserve the rule's id even if the pasted JSON omits it — the JSON
      // tab is meant as a lossless body editor, not a way to fork the id.
      setDraft(reconcile({ ...parsed, id: parsed.id || draft.id }));
    } catch (err) {
      setJsonError(err instanceof Error ? err.message : "Invalid JSON");
    }
  };

  const onTabChange = (id: string) => {
    if (id === "json") {
      setJsonText(JSON.stringify(unwrap(draft), null, 2));
      setJsonError(null);
    }
    setTab(id as "visual" | "json");
  };

  return (
    <div class="rule-editor">
      <datalist id="rdproxy-header-names">
        <For each={COMMON_HEADER_NAMES}>{(n) => <option value={n} />}</For>
      </datalist>
      <datalist id="rdproxy-rule-groups">
        <For each={groups()}>{(g) => <option value={g} />}</For>
      </datalist>

      <div class="rule-editor__header">
        <TextInput value={draft.name} onInput={(v) => setDraft("name", v)} class="rule-editor__name" aria-label="Rule name" />
        <Toggle checked={draft.enabled} onChange={(v) => setDraft("enabled", v)} label="Enabled" />
        <label class="rule-editor__field">
          <span>Priority</span>
          <TextInput
            type="number"
            value={String(draft.priority)}
            onInput={(v) => setDraft("priority", Number(v) || 0)}
            mono
          />
        </label>
        <label class="rule-editor__field">
          <span>Group</span>
          <TextInput value={draft.group ?? ""} onInput={(v) => setDraft("group", v || null)} list="rdproxy-rule-groups" placeholder="none" />
        </label>
        <div class="rule-editor__spacer" />
        <Show when={dirty()}>
          <span class="rule-editor__dirty-dot" title="Unsaved changes" aria-label="Unsaved changes" />
        </Show>
        <Button variant="ghost" size="sm" onClick={revert} disabled={!dirty()}>
          Revert
        </Button>
        <Button variant="primary" size="sm" onClick={() => void save()} disabled={!dirty() || saving()}>
          Save
        </Button>
        <Button variant="danger" size="sm" icon="trash" aria-label="Delete rule" onClick={() => void doDelete()} />
      </div>

      <Tabs
        tabs={[
          { id: "visual", label: "Visual" },
          { id: "json", label: "JSON" },
        ]}
        active={tab()}
        onChange={onTabChange}
      >
        <>
          <Show when={tab() === "visual"}>
            <div class="rule-editor__body">
              <div class="rule-section">
                <div class="rule-section__title">Match</div>
                <MatchEditor match={draft.match} setRule={setDraft} />
              </div>
              <div class="rule-section">
                <div class="rule-section__title">Actions</div>
                <ActionsEditor actions={draft.actions} matcher={draft.match} setRule={setDraft} />
              </div>
            </div>
          </Show>
          <Show when={tab() === "json"}>
            <div class="rule-editor__json">
              <textarea
                class="rule-editor__json-textarea mono"
                value={jsonText()}
                onInput={(e) => onJsonInput(e.currentTarget.value)}
                spellcheck={false}
                aria-label="Rule JSON"
              />
              <Show when={jsonError()}>{(msg) => <div class="rule-editor__json-error">{msg()}</div>}</Show>
            </div>
          </Show>
        </>
      </Tabs>
    </div>
  );
};

export default RuleEditor;
