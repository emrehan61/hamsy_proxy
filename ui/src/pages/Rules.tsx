// Rules page: master-detail rule editor. `/rules` and `/rules/:id` share
// this component — the id in the URL is the single source of truth for
// selection, so refreshing or deep-linking to a specific rule works.

import type { Component } from "solid-js";
import { Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { useBeforeLeave, useNavigate, useParams } from "@solidjs/router";
import "../styles/rules.css";
import type { RuleInput } from "../lib/api";
import { pushToast } from "../stores/ui";
import { createRule, rules, rulesLoading } from "../stores/rules";
import SplitPane from "../components/SplitPane";
import EmptyState from "../components/EmptyState";
import RuleList from "./rules/RuleList";
import RuleEditor from "./rules/RuleEditor";
import TemplatePicker from "./rules/TemplatePicker";

const Rules: Component = () => {
  const params = useParams<{ id?: string }>();
  const navigate = useNavigate();

  const [templateOpen, setTemplateOpen] = createSignal(false);
  const [dirty, setDirty] = createSignal(false);

  const selectedId = () => params.id ?? null;
  const selectedRule = createMemo(() => {
    const id = selectedId();
    if (!id) return undefined;
    return rules().find((r) => r.id === id);
  });

  // Guards both in-app navigation (selecting another rule, clicking the
  // sidebar) and an actual tab close/reload while there are unsaved edits.
  useBeforeLeave((e) => {
    if (!dirty()) return;
    e.preventDefault();
    if (confirm("Discard unsaved changes to this rule?")) {
      e.retry(true);
    }
  });

  const onBeforeUnload = (e: BeforeUnloadEvent) => {
    if (!dirty()) return;
    e.preventDefault();
    e.returnValue = "";
  };

  onMount(() => {
    window.addEventListener("beforeunload", onBeforeUnload);
    onCleanup(() => window.removeEventListener("beforeunload", onBeforeUnload));
  });

  const handleSelect = (id: string) => navigate(`/rules/${id}`);

  const handleCreateFromTemplate = async (input: RuleInput) => {
    try {
      const created = await createRule(input);
      setTemplateOpen(false);
      pushToast({ level: "success", message: `Created "${created.name}"` });
      navigate(`/rules/${created.id}`);
    } catch {
      pushToast({ level: "error", message: "Failed to create rule" });
    }
  };

  return (
    <div class="rules-page">
      <SplitPane
        direction="horizontal"
        sizeKey="rules.split"
        min={280}
        initial={340}
        first={<RuleList selectedId={selectedId()} onSelect={handleSelect} onNew={() => setTemplateOpen(true)} />}
        second={
          <Show
            when={rulesLoading() || rules().length > 0}
            fallback={
              <EmptyState
                icon="filter"
                title="No rules yet"
                description="Create your first rule to redirect, mock, block, or modify traffic."
                action={{ label: "New rule", onClick: () => setTemplateOpen(true) }}
              />
            }
          >
            <Show
              when={selectedRule()}
              fallback={<EmptyState icon="chevron-right" title="Select a rule" description="Choose a rule from the list to view and edit it." />}
            >
              {(rule) => (
                <RuleEditor
                  rule={rule()}
                  onDirtyChange={setDirty}
                  onDeleted={() => navigate("/rules")}
                />
              )}
            </Show>
          </Show>
        }
      />
      <TemplatePicker open={templateOpen()} onClose={() => setTemplateOpen(false)} onPick={(input) => void handleCreateFromTemplate(input)} />
    </div>
  );
};

export default Rules;
