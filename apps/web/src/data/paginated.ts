// Paginated-response helpers.
//
// Every boss-* list endpoint that paginates returns the same shape:
//   { data: T[], total: number, limit: number, offset: number }
// where `total` is the DB-wide count of rows matching the filter
// (and the caller's policy scope), and `data` is the requested page.
//
// When SPA code does `body.data ?? []` and discards `total`, a
// silently-truncated list looks identical to a list shorter than
// the cap. That class of bug — capped rows presented as if they
// were the full set — is what these helpers stop. Use `fetchPaged`
// to get the envelope, `isCapped` to decide whether to render an
// overflow banner.
//
// The envelope is the only accepted shape. A bare-array response is
// a contract violation and normalises to an empty page, so the
// regression shows up as a visibly empty list rather than an
// unnoticed uncapped one.

export type Paged<T> = Readonly<{
  data: ReadonlyArray<T>;
  total: number;
  limit: number;
  offset: number;
}>;

/// The result of fetching one page — a discriminated union, because a
/// failed fetch is NOT an empty page (packet 3fba9c35). The old
/// `Paged<T> | null` contract let every caller `?? []` an outage into
/// the page's normal empty state.
export type PagedResult<T> =
  | { kind: 'ready'; page: Paged<T> }
  | { kind: 'failed'; error: string };

export async function fetchPaged<T>(url: string): Promise<PagedResult<T>> {
  try {
    const resp = await fetch(url);
    if (!resp.ok) return { kind: 'failed', error: `${url}: HTTP ${resp.status}` };
    const body = (await resp.json()) as unknown;
    return { kind: 'ready', page: normalise<T>(body) };
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    return { kind: 'failed', error: detail };
  }
}

export function normalise<T>(body: unknown): Paged<T> {
  if (body && typeof body === 'object' && !Array.isArray(body)) {
    const obj = body as {
      data?: unknown;
      total?: unknown;
      limit?: unknown;
      offset?: unknown;
    };
    const data = Array.isArray(obj.data) ? (obj.data as T[]) : [];
    const total =
      typeof obj.total === 'number' && Number.isFinite(obj.total)
        ? obj.total
        : data.length;
    const limit =
      typeof obj.limit === 'number' && Number.isFinite(obj.limit)
        ? obj.limit
        : data.length;
    const offset =
      typeof obj.offset === 'number' && Number.isFinite(obj.offset)
        ? obj.offset
        : 0;
    return { data, total, limit, offset };
  }
  return { data: [], total: 0, limit: 0, offset: 0 };
}

export function isCapped<T>(p: Paged<T> | null | undefined): boolean {
  if (!p) return false;
  return p.total > p.data.length;
}

// ---------------------------------------------------------------------
// EVERY ROW A LIST MATCHES — the one paged read (backlog b68a9dde).
// ---------------------------------------------------------------------
//
// `fetchPaged` answers ONE page and leaves its total for the caller to
// read. Seven pages read `/api/jobs?…&limit=200` and never did: two
// lost rows on 2026-09-27 (ship-a-change had 978 packets, open
// backlog-items 361), the other five were waiting for their kind to
// grow. `fetchEvery` is the reader that wants the whole set: it pages
// by offset until the rows it holds reach the server's `total`, and
// when it cannot — a ceiling reached, a page that came back empty or
// repeated what it already held — it answers `truncated` with how far
// it got, so a surface can say "N of M" or refuse, and never shows a
// part as the whole. The server-side twin is boss-jobs
// `list_every::list_every` (backlog f71d1e81).

/** The jobs API's own page cap (`MAX_LIMIT`, boss-jobs http/mod.rs): a
 *  larger `limit` is clamped to it, so asking for more buys nothing. */
export const EVERY_PAGE = 1000;

/** Rows one browser read will hold before it stops and says so. A
 *  surface that needs more than this needs a narrower query, not a
 *  bigger page. */
export const EVERY_CEILING = 10_000;

export type EveryResult<T> =
  | { kind: 'ready'; data: ReadonlyArray<T>; total: number }
  | { kind: 'truncated'; data: ReadonlyArray<T>; total: number; why: string }
  | { kind: 'failed'; error: string };

/**
 * Every row `url` matches, read `page` rows at a time until the rows
 * held reach the total. `url` carries the filter and must not name
 * `limit` or `offset` — this owns both. Rows carrying a string `id`
 * are kept once: offset paging over a list that moves can hand one row
 * to two pages.
 */
export async function fetchEvery<T>(
  url: string,
  opts: Readonly<{ page?: number; ceiling?: number }> = {},
): Promise<EveryResult<T>> {
  if (/[?&](limit|offset)=/.test(url)) {
    return { kind: 'failed', error: `${url}: fetchEvery owns limit and offset` };
  }
  const page = Math.max(1, opts.page ?? EVERY_PAGE);
  const ceiling = opts.ceiling ?? EVERY_CEILING;
  const sep = url.includes('?') ? '&' : '?';
  const seen = new Set<string>();
  const rows: T[] = [];
  let offset = 0;
  try {
    for (;;) {
      const resp = await fetch(`${url}${sep}limit=${page}&offset=${offset}`);
      if (!resp.ok) return { kind: 'failed', error: `${url}: HTTP ${resp.status}` };
      const body = (await resp.json()) as unknown;
      // An envelope under a wrong key answers instead of erroring — the
      // HR page read `payload.jobs` for weeks and showed an empty
      // department (hr-tasks.ts). An object with no `data` array is a
      // failure; a bare array keeps `normalise`'s rule above.
      if (
        body !== null &&
        typeof body === 'object' &&
        !Array.isArray(body) &&
        !Array.isArray((body as { data?: unknown }).data)
      ) {
        return { kind: 'failed', error: `${url}: expected the {data, total} envelope` };
      }
      const got = normalise<T>(body);
      const fresh = got.data.filter((row) => {
        const id = (row as { id?: unknown } | null)?.id;
        if (typeof id !== 'string') return true;
        if (seen.has(id)) return false;
        seen.add(id);
        return true;
      });
      rows.push(...fresh);
      offset += got.data.length;
      const held = { data: rows, total: got.total };
      if (offset >= got.total) return { kind: 'ready', ...held };
      if (got.data.length === 0) {
        return { kind: 'truncated', ...held, why: 'a page came back empty before the total' };
      }
      if (fresh.length === 0) {
        return { kind: 'truncated', ...held, why: 'a page repeated rows already read' };
      }
      if (offset >= ceiling) {
        return { kind: 'truncated', ...held, why: `stopped at the ${ceiling}-row ceiling` };
      }
    }
  } catch (e) {
    return { kind: 'failed', error: e instanceof Error ? e.message : String(e) };
  }
}

/** The rows of a whole read, or a thrown Error naming why there are
 *  none — for a surface whose error state is a caught throw. A
 *  truncated read throws too, naming how many of how many it holds. */
export function wholeOrThrow<T>(r: EveryResult<T>): ReadonlyArray<T> {
  if (r.kind === 'ready') return r.data;
  if (r.kind === 'failed') throw new Error(r.error);
  throw new Error(`read ${r.data.length} of ${r.total} rows and stopped: ${r.why}`);
}
