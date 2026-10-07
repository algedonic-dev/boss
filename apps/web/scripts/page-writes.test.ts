import { expect, test } from 'bun:test';
import * as mocks from '../tests/mocked/_smokeMocks';

test('page-write accounting shares one exact shell method and path', () => {
  expect('isPageWrite' in mocks).toBe(true);
  const classify = Reflect.get(mocks, 'isPageWrite') as (method: string, path: string) => boolean;
  expect(classify('POST', '/api/surface-opens')).toBe(false);
  expect(classify('GET', '/api/jobs')).toBe(false);
  expect(classify('POST', '/api/jobs')).toBe(true);
  expect(classify('DELETE', '/api/surface-opens')).toBe(true);
  expect(classify('POST', '/api/surface-opens/other')).toBe(true);
  expect(classify('POST', '/api/not-surface-opens')).toBe(true);
  expect(classify('POST', '/assets/logo.svg')).toBe(false);
});

test('page writes retain the original request records without mutation', () => {
  expect('pageWrites' in mocks).toBe(true);
  const pageRequest = Object.freeze({ method: 'POST', path: '/api/jobs', ordinal: 2 });
  const requests = Object.freeze([
    Object.freeze({ method: 'POST', path: '/api/surface-opens', ordinal: 1 }),
    pageRequest,
    Object.freeze({ method: 'GET', path: '/api/jobs', ordinal: 3 }),
  ]);
  const select = Reflect.get(mocks, 'pageWrites') as (rows: typeof requests) => ReadonlyArray<typeof requests[number]>;
  expect(select(requests)).toEqual([pageRequest]);
  expect(select(requests)[0]).toBe(pageRequest);
  expect(requests).toHaveLength(3);
});
