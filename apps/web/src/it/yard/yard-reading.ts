import type { YardRead, YardState } from './yard';

export type YardReading = Readonly<{
  request: number;
  acceptedRequest: number;
  data: YardState | null;
  observedAt: number | null;
  error: string | null;
}>;

export const emptyYardReading = (): YardReading => ({ request: 0, acceptedRequest: 0, data: null, observedAt: null, error: null });

export function beginYardReading(previous: YardReading): YardReading {
  return { ...previous, request: previous.request + 1 };
}

// A pending poll must not starve a valid slow response. Only an already
// accepted newer response supersedes it, including a refusal (a4de1e72).
export function finishYardReading(previous: YardReading, request: number, read: YardRead): YardReading {
  if (request <= previous.acceptedRequest || request > previous.request) return previous;
  return read.kind === 'ready'
    ? { ...previous, acceptedRequest: request, data: read.data, observedAt: read.observedAt, error: null }
    : { ...previous, acceptedRequest: request, error: read.error };
}
