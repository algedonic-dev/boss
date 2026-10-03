// The inbox read's parse (backlog e2679b23 (c)). It used to be
// `Array.isArray(raw) ? raw : []`, so a 200 whose body was anything but
// a list — an envelope, null, an error string — became an EMPTY inbox
// and the page said "Nothing is waiting on you". A parse that throws is
// a failed read under fetchRemote, which the page renders with Retry.
//
// Since backlog 74da899d the read is paged, so the one body that is an
// inbox is the `{data, total, limit, offset, kinds}` envelope, and the
// bare list the read used to answer is now the shape it refuses.

import { describe, expect, test } from 'bun:test';
import { countsOf, inboxPath, parseInbox } from './read';

const PATH = '/api/messages/inbox/emp-001';

const page = (over: Record<string, unknown> = {}) => ({
  data: [{ id: 'm1' }, { id: 'm2' }],
  total: 7,
  limit: 2,
  offset: 0,
  kinds: [{ kind: 'direct', all: 7, unread: 3 }],
  ...over,
});

describe('parseInbox', () => {
  test('the envelope is the inbox page, as given', () => {
    expect(parseInbox(PATH)(page())).toEqual(page() as never);
  });

  test('an empty page is an empty inbox — the one body that may say so', () => {
    const empty = page({ data: [], total: 0, kinds: [] });
    expect(parseInbox(PATH)(empty)).toEqual(empty as never);
  });

  test.each([
    [[], 'a list'],
    [null, 'null'],
    ['message store down', 'a string'],
    [0, 'a number'],
    [{}, 'an object with no data list'],
  ] as const)('%p is refused, naming the path and what came back', (body, what) => {
    expect(() => parseInbox(PATH)(body)).toThrow(
      `${PATH}: HTTP 200, but the body is ${what}, not a {data: [...]} envelope`,
    );
  });

  // An envelope missing its total or its kinds is not the inbox either:
  // the page would say "N of M" or head the list with counts nobody read.
  test.each([
    [{ total: undefined }, 'has no total'],
    [{ total: '7' }, 'has no total'],
    [{ kinds: undefined }, 'has no kinds list'],
  ] as const)('an envelope with %p is refused: it %s', (over, what) => {
    expect(() => parseInbox(PATH)(page(over))).toThrow(`${PATH}: HTTP 200, but the body ${what}`);
  });
});

describe('inboxPath', () => {
  test('each filter asks the server for its own narrowing, one page at a time', () => {
    expect(inboxPath('emp-001', 'needs-you', 0, 100)).toBe(
      '/api/messages/inbox/emp-001?kind=direct&unread=true&limit=100',
    );
    expect(inboxPath('emp-001', 'all', 0, 100)).toBe('/api/messages/inbox/emp-001?limit=100');
    expect(inboxPath('emp-001', 'unread', 0, 100)).toBe('/api/messages/inbox/emp-001?unread=true&limit=100');
    expect(inboxPath('emp-001', 'direct', 200, 100)).toBe(
      '/api/messages/inbox/emp-001?kind=direct&limit=100&offset=200',
    );
    expect(inboxPath('emp-001', 'signal', 0, 100)).toBe('/api/messages/inbox/emp-001?kind=signal&limit=100');
    expect(inboxPath('a b', 'all', 0, 5)).toBe('/api/messages/inbox/a%20b?limit=5');
  });
});

describe('countsOf', () => {
  test('the header and filter counts are the whole inbox, from the kinds', () => {
    expect(
      countsOf([
        { kind: 'direct', all: 201, unread: 90 },
        { kind: 'signal', all: 4928, unread: 1980 },
        { kind: 'other', all: 2, unread: 1 },
      ]),
    ).toEqual({ all: 5131, unread: 2071, direct: 201, signal: 4928, needsYou: 90 });
    expect(countsOf([])).toEqual({ all: 0, unread: 0, direct: 0, signal: 0, needsYou: 0 });
  });
});
