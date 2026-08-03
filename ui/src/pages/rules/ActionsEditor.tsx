// The "Actions" section: an ordered list of action cards. Reordering,
// duplication, removal, and type-switching all replace the actions array
// wholesale (acceptable — they're deliberate, infrequent structural edits).
// Field-level edits within a card go through `produce` so the action object
// keeps its identity across keystrokes (see ActionFields.tsx's header
// comment for why that matters).
//
// Action type is "pinned" to whatever the first action's type is (i.e. the
// type chosen via the New Rule template): once a rule has actions, every
// card's type <Select> only offers that one type, so the template choice
// stays meaningful instead of being immediately overridable. A card whose
// own type differs from the pin (legacy/imported rules) keeps its current
// type as an extra option so the <select> is never rendered with a value
// outside its options. With zero actions nothing is pinned and the full
// type list is offered, matching the pre-pinning behaviour.

import type { Component } from "solid-js";
import { For, Show, createSignal } from "solid-js";
import { produce } from "solid-js/store";
import type { SetStoreFunction } from "solid-js/store";
import type { Action, Matcher, Rule } from "../../lib/types";
import Select from "../../components/Select";
import Button from "../../components/Button";
import Icon from "../../components/Icon";
import { actionPhaseWarning, countCaptureGroups } from "../../lib/matcher";
import { ACTION_TYPES, ACTION_TYPE_LABELS, defaultActionFor } from "./actionDefaults";
import ActionFields from "./ActionFields";

export interface ActionsEditorProps {
  actions: Action[];
  matcher: Matcher;
  setRule: SetStoreFunction<Rule>;
}

const ACTION_TYPE_OPTIONS = ACTION_TYPES.map((t) => ({ value: t, label: ACTION_TYPE_LABELS[t] }));

const ActionsEditor: Component<ActionsEditorProps> = (props) => {
  const [dragIndex, setDragIndex] = createSignal<number | null>(null);

  const matcherCaptureCount = () =>
    props.matcher.urlOp === "regex" ? countCaptureGroups(props.matcher.urlValue) : 0;

  // Undefined when the rule has no actions yet (nothing chosen to pin to).
  const pinnedType = () => props.actions[0]?.type;

  const optionsFor = (action: Action) => {
    const pinned = pinnedType();
    if (pinned === undefined) return ACTION_TYPE_OPTIONS;
    if (action.type === pinned) return ACTION_TYPE_OPTIONS.filter((o) => o.value === pinned);
    // Defensive: an imported/legacy rule may have a card whose type doesn't
    // match the pin — keep it selectable so the <select> always has its
    // current value among its options.
    return ACTION_TYPE_OPTIONS.filter((o) => o.value === pinned || o.value === action.type);
  };

  const move = (from: number, to: number) => {
    if (from === to) return;
    props.setRule("actions", (arr) => {
      const next = arr.slice();
      const [item] = next.splice(from, 1);
      if (item) next.splice(to, 0, item);
      return next;
    });
  };

  const duplicate = (index: number) => {
    props.setRule("actions", (arr) => {
      const source = arr[index];
      if (!source) return arr;
      const clone = JSON.parse(JSON.stringify(source)) as Action;
      const next = arr.slice();
      next.splice(index + 1, 0, clone);
      return next;
    });
  };

  const remove = (index: number) => {
    props.setRule("actions", (arr) => arr.filter((_, i) => i !== index));
  };

  const changeType = (index: number, nextType: Action["type"]) => {
    props.setRule("actions", index, defaultActionFor(nextType));
  };

  const add = () => {
    const pinned = pinnedType();
    props.setRule("actions", (arr) => [...arr, defaultActionFor(pinned ?? "setRequestHeader")]);
  };

  return (
    <div class="actions-editor">
      <Show when={props.actions.length === 0}>
        <div class="actions-editor__empty">No actions yet.</div>
      </Show>
      <For each={props.actions}>
        {(action, index) => {
          const warning = () => actionPhaseWarning(props.matcher, action.type);
          return (
            <div
              class={`action-card${dragIndex() === index() ? " action-card--dragging" : ""}`}
              draggable
              onDragStart={() => setDragIndex(index())}
              onDragOver={(e) => e.preventDefault()}
              onDrop={(e) => {
                e.preventDefault();
                const from = dragIndex();
                setDragIndex(null);
                if (from !== null) move(from, index());
              }}
              onDragEnd={() => setDragIndex(null)}
            >
              <div class="action-card__header">
                <Icon name="drag-handle" size={14} class="action-card__drag-handle" />
                <span class="action-card__index mono">{index() + 1}</span>
                <Select
                  value={action.type}
                  onChange={(v) => changeType(index(), v as Action["type"])}
                  options={optionsFor(action)}
                  disabled={optionsFor(action).length === 1}
                  class="action-card__type"
                />
                <Show when={warning()}>
                  {(msg) => (
                    <span class="action-card__warning" title={msg()} role="img" aria-label={msg()}>
                      <Icon name="alert-triangle" size={14} />
                    </span>
                  )}
                </Show>
                <div class="action-card__spacer" />
                <Button variant="ghost" size="sm" icon="copy" aria-label="Duplicate action" onClick={() => duplicate(index())} />
                <Button variant="ghost" size="sm" icon="trash" aria-label="Remove action" onClick={() => remove(index())} />
              </div>
              <Show when={warning()}>{(msg) => <div class="action-card__warning-text">{msg()}</div>}</Show>
              <div class="action-card__body">
                <ActionFields
                  action={action}
                  matcherCaptureCount={matcherCaptureCount()}
                  update={(mutator) => props.setRule("actions", index(), produce(mutator))}
                />
              </div>
            </div>
          );
        }}
      </For>
      <Button variant="default" size="sm" icon="plus" onClick={add}>
        Add action
      </Button>
      <Show when={pinnedType()}>
        <div class="actions-editor__hint">Action type is fixed by this rule's template — change it from the JSON tab.</div>
      </Show>
    </div>
  );
};

export default ActionsEditor;
