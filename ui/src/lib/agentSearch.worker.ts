import { searchSession } from "./agentSearch";
import type { AgentSearchParams } from "./agentSearch";
import type { Flow } from "./types";

self.onmessage = (event: MessageEvent<{ flows: Flow[]; params: AgentSearchParams }>) => {
  try { self.postMessage(searchSession(event.data.flows, event.data.params)); }
  catch { self.postMessage({ error: "Invalid search arguments or regex. HAR search uses JavaScript regex syntax." }); }
};
