/** Actual v1 Unix-socket API DTOs. Supply a Studio backend bridge as transport.
 * The frontend never accesses the socket, filesystem or shell directly.
 */
export type Request = { method: string; [key: string]: unknown };
export type Envelope<T> = { ok: true; result: T } | { ok: false; error: string };
export type Transport = (request: Request) => Promise<Envelope<unknown>>;
export interface Artifact {
  id: number; run_id: number; stage: string; kind: string;
  sha256: string; size: number; created_at: string;
  /** Local service metadata only; never use as a download request input. */
  path: string;
}
export interface Run {
  id: number; target: string; commit: string; plan_digest: string;
  status: string; context: unknown; cancellation_requested: boolean;
  error: string | null; created_at: string; started_at: string | null;
  finished_at: string | null;
  stages: Array<{ name: string; status: string; details: unknown; updated_at: string }>;
  artifacts: Artifact[];
}
export interface Event {
  id: number; run_id: number; stage: string | null;
  kind: string; payload: unknown; timestamp: string;
}
export function createCiClient(transport: Transport) {
  async function call<T>(request: Request): Promise<T> {
    const response = await transport(request);
    if (!response.ok) throw new Error(response.error);
    return response.result as T;
  }
  return {
    health: () => call<{status: string; protocol: number}>({method: "health"}),
    runs: () => call<Run[]>({method: "runs"}),
    run: (id: number) => call<Run>({method: "run", id}),
    plan: (target: string) => call<unknown>({method: "plan", target}),
    queue: (target: string, commit: string, key: string, context?: unknown) =>
      call<{run_id: number}>({method: "enqueue", target, commit, key, context}),
    cancel: (id: number) => call<Run>({method: "cancel", id}),
    events: (id: number, after = 0) => call<Event[]>({method: "events", id, after}),
    logs: (id: number, stage: string, offset = 0) =>
      call<{text: string; next_offset: number}>({method: "logs", id, stage, offset}),
    artifact: (id: number) => call<Artifact>({method: "artifact", id}),
    chunk: (id: number, offset = 0) =>
      call<{bytes: number[]; offset: number; next_offset: number; eof: boolean; sha256: string}>
      ({method: "artifact_chunk", id, offset}),
  };
}
