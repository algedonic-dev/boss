// Live step-activity feed for FlowMotion (1fb51180 — David: "see the
// automation of the job processing more reliably and actively …
// animate the job flow much better").
//
// Rides the EXISTING operator-gated SSE stream
// (/api/events/stream?kind=step.) — the audit log pushed to the
// browser, 2s server cadence. Each step.{ready,done,assigned}.* event
// resolves to (job kind, step slug) via one cached job fetch, and the
// caller animates from there. Everything is defensive: a malformed
// frame, a 404'd job, or a dropped connection degrades to "no motion"
// — never a crash (the route-smoke crawl runs this page against an
// adversarial mock).

import { isHumanActor } from '../../data/actor';

export type FlowHit = Readonly<{
  jobKind: string;
  jobId: string;
  jobTitle: string;
  slug: string;
  phase: 'ready' | 'done' | 'assigned';
  stepKind: string;
  /** True when a non-human CPU did it — see {@link isMachineActor}. */
  actor: string;
  machine: boolean;
  at: number;
}>;

/**
 * Is this actor the machine at work rather than a person?
 *
 * True for both non-human arms of `ActorId`: named automations
 * (`automation:*`, and the legacy bare `rule:*` spelling) and agent
 * sessions (`<mode>:<model>`, e.g. `claude:opus-5`). An agent is a CPU
 * but not staff, so it animates as machine traffic — counting it as a
 * person would overstate how much of the flow humans are driving.
 *
 * The human/machine rule itself lives once, in `isHumanActor`. An
 * absent actor stays *unattributed* rather than becoming machine
 * traffic: this feed animates whatever the stream hands it, and a
 * malformed frame must not colour itself in.
 */
export function isMachineActor(actor: string): boolean {
  return actor.length > 0 && !isHumanActor(actor);
}

type JobLite = Readonly<{
  kind: string;
  title: string;
  steps: ReadonlyArray<{ id: string; spec_slug?: string | null; title?: string }>;
}>;

const PHASE_RE = /^step\.(ready|done|assigned)\./;

export function connectLiveFlow(onHit: (hit: FlowHit) => void): () => void {
  let source: EventSource | null = null;
  let closed = false;
  let retryMs = 2000;
  const jobCache = new Map<string, { at: number; job: JobLite | null }>();

  async function jobFor(jobId: string): Promise<JobLite | null> {
    const hit = jobCache.get(jobId);
    if (hit && Date.now() - hit.at < 60_000) return hit.job;
    try {
      const r = await fetch(`/api/jobs/${jobId}`);
      if (!r.ok) throw new Error(String(r.status));
      const j = (await r.json()) as unknown;
      const job =
        typeof j === 'object' && j !== null && Array.isArray((j as JobLite).steps)
          ? (j as JobLite)
          : null;
      jobCache.set(jobId, { at: Date.now(), job });
      return job;
    } catch {
      jobCache.set(jobId, { at: Date.now(), job: null });
      return null;
    }
  }

  async function handle(raw: string): Promise<void> {
    let entry: { kind?: string; payload?: Record<string, unknown> };
    try {
      entry = JSON.parse(raw) as typeof entry;
    } catch {
      return;
    }
    const kind = typeof entry.kind === 'string' ? entry.kind : '';
    const m = PHASE_RE.exec(kind);
    if (!m) return;
    const payload = entry.payload ?? {};
    const jobId = typeof payload.job_id === 'string' ? payload.job_id : null;
    const stepId = typeof payload.step_id === 'string' ? payload.step_id : null;
    if (!jobId || !stepId) return;
    const job = await jobFor(jobId);
    if (!job) return;
    const step = job.steps.find((s) => s.id === stepId);
    const slug = (step?.spec_slug || step?.title || '').toString();
    if (!slug) return;
    const actor = typeof payload._actor === 'string' ? payload._actor : '';
    onHit({
      jobKind: job.kind,
      jobId,
      jobTitle: typeof job.title === 'string' ? job.title : jobId,
      slug,
      phase: m[1] as FlowHit['phase'],
      stepKind: kind.slice(m[0].length),
      actor,
      machine: isMachineActor(actor),
      at: Date.now(),
    });
  }

  function open(): void {
    if (closed) return;
    try {
      source = new EventSource('/api/events/stream?kind=step.');
    } catch {
      scheduleRetry();
      return;
    }
    source.onmessage = (ev) => {
      retryMs = 2000;
      void handle(ev.data);
    };
    source.onerror = () => {
      source?.close();
      source = null;
      scheduleRetry();
    };
  }

  function scheduleRetry(): void {
    if (closed) return;
    const delay = retryMs;
    retryMs = Math.min(retryMs * 2, 60_000);
    setTimeout(() => open(), delay);
  }

  open();
  return () => {
    closed = true;
    source?.close();
    source = null;
  };
}
