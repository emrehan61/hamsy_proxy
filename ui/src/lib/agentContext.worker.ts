import { readContext, type ContextParams, type PreparedContext } from "./agentContext";
self.onmessage = (event: MessageEvent<{ prepared: PreparedContext; params: ContextParams }>) => {
  try { self.postMessage(readContext(event.data.prepared, event.data.params)); }
  catch { self.postMessage({ error: { code: "invalid_arguments" } }); }
};
