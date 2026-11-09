import { ApiError, getFlow } from "./api";
import type { Flow } from "./types";

/** Load bodies without flooding the API or populating the selected-flow cache. */
export async function loadSearchSnapshot(ids: string[], signal: AbortSignal): Promise<Flow[]> {
  const controller = new AbortController();
  const abort = () => controller.abort();
  signal.addEventListener("abort", abort, { once: true });
  if (signal.aborted) abort();
  const flows: (Flow | undefined)[] = new Array(ids.length);
  let next = 0;
  try {
    controller.signal.throwIfAborted();
    await Promise.all(Array.from({ length: Math.min(6, ids.length) }, async () => {
      while (next < ids.length) {
        controller.signal.throwIfAborted();
        const index = next++;
        try {
          flows[index] = await getFlow(ids[index]!, controller.signal);
        } catch (error) {
          // A flow may have been evicted while this snapshot was loading.
          if (error instanceof ApiError && error.status === 404) continue;
          controller.abort();
          throw error;
        }
      }
    }));
    controller.signal.throwIfAborted();
    return flows.filter((flow): flow is Flow => flow !== undefined);
  } finally {
    signal.removeEventListener("abort", abort);
  }
}
