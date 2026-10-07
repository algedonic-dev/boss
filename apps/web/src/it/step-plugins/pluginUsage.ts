export type Binding = Readonly<{ workflow: string; version: number; step: string; kind: string }>;
export type Usage<T> = Readonly<{kind: 'loading'} | {kind: 'known'; value: T} | {kind: 'unknown'; reason: string}>;

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

// The owning endpoint lists every active workflow in one array, without pagination.
export function readBindings(value: unknown): ReadonlyArray<Binding> {
  if (!Array.isArray(value)) throw new Error('Malformed active workflow list');
  const result: Binding[] = [];
  const identities = new Set<string>();
  for (const workflow of value) {
    if (!record(workflow) || typeof workflow.kind !== 'string' || !workflow.kind.trim()
      || !Number.isSafeInteger(workflow.version) || (workflow.version as number) < 1
      || workflow.status !== 'active' || !Array.isArray(workflow.steps)
      || identities.has(workflow.kind)) throw new Error('Malformed active workflow row');
    identities.add(workflow.kind);
    const steps = new Set<string>();
    for (const step of workflow.steps) {
      if (!record(step) || typeof step.kind !== 'string' || !step.kind.trim()
        || typeof step.title !== 'string' || !step.title.trim() || steps.has(step.title)) {
        throw new Error('Malformed workflow step binding');
      }
      steps.add(step.title);
      result.push({workflow: workflow.kind, version: workflow.version as number, step: step.title, kind: step.kind});
    }
  }
  return [...result].sort((a, b) => a.workflow.localeCompare(b.workflow) || a.step.localeCompare(b.step));
}

export function readCount(value: unknown, kind: string): number {
  if (!record(value) || value.kind !== kind || typeof value.in_flight !== 'number'
    || !Number.isSafeInteger(value.in_flight) || value.in_flight < 0) {
    throw new Error('Malformed native in-flight count');
  }
  return value.in_flight;
}
