import type { Flow } from "./types";
import { searchHar } from "./harSearch";

let flows: Flow[] = [];
self.onmessage = (event: MessageEvent) => {
  const message = event.data;
  if (message.type === "init") {
    flows = message.flows;
    return;
  }
  const result = searchHar(flows, message.query, message.regex, message.caseSensitive, new Set(message.ids));
  self.postMessage({ id: message.id, ...result });
};
