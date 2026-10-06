import { expect, test } from 'bun:test';
import { fetchEvery } from './paginated';

test('the whole reader uses an explicit read port for every counted page', async () => {
  const calls: string[] = [];
  const result = await fetchEvery<{ id: string }>('/api/jobs?kind=runtime-roster-capture', {
    read: async (url: string) => {
      calls.push(url);
      return url.endsWith('offset=0')
        ? { data: [{ id: 'one' }], total: 2 }
        : { data: [{ id: 'two' }], total: 2 };
    },
  });
  expect(result).toEqual({ kind: 'ready', data: [{ id: 'one' }, { id: 'two' }], total: 2 });
  expect(calls).toEqual([
    '/api/jobs?kind=runtime-roster-capture&limit=1000&offset=0',
    '/api/jobs?kind=runtime-roster-capture&limit=1000&offset=1',
  ]);
});

test('an unreadable late page through the explicit port is a failed whole read', async () => {
  const result = await fetchEvery('/api/jobs?kind=runtime-roster-capture', {
    read: async (url: string) => {
      if (url.endsWith('offset=0')) return { data: [{ id: 'one' }], total: 2 };
      throw new Error('late page unavailable');
    },
  });
  expect(result).toEqual({ kind: 'failed', error: 'late page unavailable' });
});
