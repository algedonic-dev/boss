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
