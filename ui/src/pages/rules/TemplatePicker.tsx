import type { Component } from "solid-js";
import { For } from "solid-js";
import type { RuleInput } from "../../lib/api";
import Modal from "../../components/Modal";
import { RULE_TEMPLATES } from "./templates";

export interface TemplatePickerProps {
  open: boolean;
  onClose: () => void;
  onPick: (input: RuleInput) => void;
}

const TemplatePicker: Component<TemplatePickerProps> = (props) => {
  return (
    <Modal open={props.open} onClose={props.onClose} title="New rule">
      <div class="template-picker">
        <For each={RULE_TEMPLATES}>
          {(template) => (
            <button type="button" class="template-picker__card" onClick={() => props.onPick(template.build())}>
              <span class="template-picker__label">{template.label}</span>
              <span class="template-picker__description">{template.description}</span>
            </button>
          )}
        </For>
      </div>
    </Modal>
  );
};

export default TemplatePicker;
