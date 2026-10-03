// The inbox read's parse, for fetchRemote (backlog e2679b23 (c)), and
// the read's path and counts (backlog 74da899d).
//
// Only the inbox envelope is an inbox. The parse this replaced coerced
// every other 200 to [], so a changed response shape — an envelope where
// the list was due, a null — painted "Nothing is waiting on you": the
// false-empty class 3fba9c35 swept out of the fetch's status and catch,
// still alive one layer down in its parse. Throwing makes it a failed
// read, which the page renders as the failure line and Retry.
//
// The read was every row as a bare list until 74da899d (page audit
// 5477d9eb GAP 3: 5,129 rows to one recipient, filtered in the browser).
// It is now one page of the server-narrowed inbox in the shared
// `{data, total, limit, offset}` envelope (data/shape.ts), plus `kinds`:
// the whole inbox counted per kind, which the header and the filter
// buttons need whatever page or filter is shown.

import { readEnvelope } from '../data/shape';
import type { InboxPage, KindCount } from './types';

/// The parse for a read of `path`; the error names the path the way
/// fetchRemote names a refused status, so the two failures read alike.
export function parseInbox(path: string): (raw: unknown) => InboxPage {
  return (raw) => {
    const { body } = readEnvelope(path, raw);
    if (typeof body.total !== 'number') {
      throw new Error(`${path}: HTTP 200, but the body has no total`);
    }
    if (!Array.isArray(body.kinds)) {
      throw new Error(`${path}: HTTP 200, but the body has no kinds list`);
    }
    return body as unknown as InboxPage;
  };
}

/// The page's five views. `needs-you` — unread directs — is the default.
export type KindFilter = 'needs-you' | 'all' | 'unread' | 'direct' | 'signal';

/// What each view asks the server to narrow to.
const NARROWING: Readonly<Record<KindFilter, Readonly<{ kind?: string; unread?: true }>>> = {
  'needs-you': { kind: 'direct', unread: true },
  all: {},
  unread: { unread: true },
  direct: { kind: 'direct' },
  signal: { kind: 'signal' },
};

/// The read of one page of `user`'s inbox under `filter`. The server
/// narrows and pages; the browser no longer holds every row to filter.
export function inboxPath(user: string, filter: KindFilter, offset: number, limit: number): string {
  const { kind, unread } = NARROWING[filter];
  const params = new URLSearchParams();
  if (kind) params.set('kind', kind);
  if (unread) params.set('unread', 'true');
  params.set('limit', String(limit));
  if (offset > 0) params.set('offset', String(offset));
  return `/api/messages/inbox/${encodeURIComponent(user)}?${params}`;
}

export type InboxCounts = Readonly<{
  all: number;
  unread: number;
  direct: number;
  signal: number;
  /// Unread directs: what is waiting on the viewer.
  needsYou: number;
}>;

/// The whole inbox's numbers, from the read's per-kind counts.
export function countsOf(kinds: ReadonlyArray<KindCount>): InboxCounts {
  const of = (k: string): KindCount | undefined => kinds.find((c) => c.kind === k);
  return {
    all: kinds.reduce((n, c) => n + c.all, 0),
    unread: kinds.reduce((n, c) => n + c.unread, 0),
    direct: of('direct')?.all ?? 0,
    signal: of('signal')?.all ?? 0,
    needsYou: of('direct')?.unread ?? 0,
  };
}
