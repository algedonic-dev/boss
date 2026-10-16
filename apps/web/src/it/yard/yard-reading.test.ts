import { expect, test } from 'bun:test';
import { assembleYard, readYard } from './yard';
import { beginYardReading, emptyYardReading, finishYardReading } from './yard-reading';

const data = assembleYard([], []);

test('consistently slow reads settle while a newer poll remains pending', () => {
  let reading = beginYardReading(emptyYardReading());
  for (let request = 1; request <= 3; request += 1) {
    reading = beginYardReading(reading);
    reading = finishYardReading(reading, request, { kind: 'ready', data, observedAt: request * 100 });
    expect(reading.data).toBe(data);
    expect(reading.observedAt).toBe(request * 100);
    expect(reading.request).toBe(request + 1);
  }
});

test('failure keeps the successful original and time, recovery replaces both', () => {
  const first = beginYardReading(emptyYardReading());
  const good = finishYardReading(first, first.request, { kind: 'ready', data, observedAt: 100 });
  const next = beginYardReading(good);
  const refused = finishYardReading(next, next.request, { kind: 'failed', error: 'HTTP 403' });
  expect(refused.data).toBe(data);
  expect(refused.observedAt).toBe(100);
  expect(refused.error).toBe('HTTP 403');
  const recovering = beginYardReading(refused);
  const replacement = assembleYard([], []);
  expect(finishYardReading(recovering, recovering.request, { kind: 'ready', data: replacement, observedAt: 200 }))
    .toEqual({ request: 3, acceptedRequest: 3, data: replacement, observedAt: 200, error: null });
});

test('older success and refusal cannot overwrite the latest accepted response', () => {
  const first = beginYardReading(emptyYardReading());
  const second = beginYardReading(first);
  const accepted = finishYardReading(second, second.request, { kind: 'ready', data, observedAt: 200 });
  expect(finishYardReading(accepted, first.request, { kind: 'failed', error: 'old refusal' })).toBe(accepted);
  expect(finishYardReading(accepted, first.request, { kind: 'ready', data: assembleYard([], []), observedAt: 100 })).toBe(accepted);
  const failed = finishYardReading(second, second.request, { kind: 'failed', error: 'new refusal' });
  expect(finishYardReading(failed, first.request, { kind: 'ready', data, observedAt: 100 })).toBe(failed);
});

test('an initial failure has no invented successful reading', () => {
  const pending = beginYardReading(emptyYardReading());
  expect(finishYardReading(pending, pending.request, { kind: 'failed', error: 'offline' }))
    .toEqual({ request: 1, acceptedRequest: 1, data: null, observedAt: null, error: 'offline' });
});

test('a pending newer request preserves a slow refusal and accepts its later recovery only once', () => {
  const first = beginYardReading(emptyYardReading());
  const second = beginYardReading(first);
  const refused = finishYardReading(second, first.request, { kind: 'failed', error: 'slow refusal' });
  expect(refused.error).toBe('slow refusal');
  expect(refused.request).toBe(2);
  expect(refused.acceptedRequest).toBe(1);
  expect(finishYardReading(refused, 1, { kind: 'ready', data, observedAt: 1 })).toBe(refused);
  expect(finishYardReading(refused, 3, { kind: 'ready', data, observedAt: 3 })).toBe(refused);
  const recovered = finishYardReading(refused, 2, { kind: 'ready', data, observedAt: 2 });
  expect(recovered.error).toBeNull();
  expect(recovered.data).toBe(data);
  expect(recovered.acceptedRequest).toBe(2);
});

test('transport and unreadable JSON are explicit failed reads', async () => {
  const original = globalThis.fetch;
  try {
    globalThis.fetch = Object.assign(async (_input: Parameters<typeof fetch>[0]): Promise<Response> => {
      throw new Error('network lost');
    }, { preconnect: original.preconnect });
    expect(await readYard()).toEqual({ kind: 'failed', error: 'network lost' });
    globalThis.fetch = (async input => String(input).includes('kind=pr-train&limit=40')
      ? new Response('{broken', { status: 200 })
      : Response.json({ data: [], total: 0 })) as typeof fetch;
    expect((await readYard()).kind).toBe('failed');
  } finally {
    globalThis.fetch = original;
  }
});

test('a train response without its data envelope cannot report an empty yard', async () => {
  const original = globalThis.fetch;
  try {
    globalThis.fetch = (async input => Response.json(String(input).includes('kind=pr-train&limit=40')
      ? { wrong: [] } : { data: [], total: 0 })) as typeof fetch;
    expect((await readYard()).kind).toBe('failed');
  } finally {
    globalThis.fetch = original;
  }
});
