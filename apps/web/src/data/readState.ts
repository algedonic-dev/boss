// Did this read work, and what is the page therefore allowed to say?
//
// THE CLASS THIS EXISTS FOR (packet 7a7bfc88, measured 2026-09-20). One
// line, live once per page in six pages beyond /ux/support:
//
//   const pBody = pResp.ok ? await pResp.json() : [];            // 5 pages
//   devicesPage = dPaged.kind === 'ready' ? dPaged.page : null;  // AccountsList
//
// A non-2xx becomes an empty list, or a `failed` arm is dropped on the
// floor, and the page paints an outage as its normal empty state: no
// banner, no alert, nothing for an operator to notice. Silence is the
// one forbidden failure mode and each of those lines manufactures it.
//
// WHY A MODULE IN data/ RATHER THAN SIX MORE FIXES. The support page
// fixed exactly this in apps/web/src/support/reads.ts and the fix
// works, but it is one idea, and six copies of one idea is the drift
// CLAUDE.md 9a is about. The general shape is small: a ReadState per
// read, and a view that refuses to let a failure wear the empty state.
// Lifted here it is one definition with one test, and adopting a page
// is mechanical — `readStateOfResponse` / `readStateOf` at the fetch,
// `listView` at the empty state, `blankMeaning` at a blank cell.
//
// WHY NOT Remote<T> (data/remote.ts). Remote carries the DATA of a read
// that is still in flight; ReadState is the residue of a read whose
// data has already been folded into derived rows. The pages in this
// class keep the rows and forget the outcome, which is precisely the
// bug, so the outcome needs a name of its own that survives beside the
// rows. Use Remote when the component holds the payload; use ReadState
// when it holds what the payload became.

/// One read's outcome. A failure carries its reason, because the
/// operator reading the page is the person who has to act on it.
/// `loading` is a read that has not answered yet: a page that starts
/// its reads as `okRead` claims an answer it does not have, and its
/// empty state then speaks for the loading window (backlog 20410830,
/// the /ux/warehouse audit).
export type ReadState =
  | { kind: 'loading' }
  | { kind: 'ok' }
  | { kind: 'failed'; error: string };

export const loadingRead: ReadState = { kind: 'loading' };

export const okRead: ReadState = { kind: 'ok' };

export function failedRead(error: string): ReadState {
  return { kind: 'failed', error };
}

/// A page that tracks its read as `loading` + `loadFailed` flags, as
/// /ux/people and /ux/parts do. Each had built this as a string state of
/// its own ('loading' | 'failed' | 'read') beside this type, which says
/// the same thing and keeps the reason (backlog a97d4cf2, CLAUDE.md 9a).
export function readStateOfLoad(loading: boolean, loadFailed: string | null): ReadState {
  if (loading) return loadingRead;
  return loadFailed === null ? okRead : failedRead(loadFailed);
}

/// A bare `fetch` Response, reduced to whether the read worked. The
/// five `pResp.ok ? await pResp.json() : []` sites fetch this way, so
/// this is the adapter they need; the error text matches `fetchPaged`
/// and `fetchRemote` so one banner can render any of the three.
export function readStateOfResponse(
  url: string,
  resp: Pick<Response, 'ok' | 'status'>,
): ReadState {
  return resp.ok ? okRead : failedRead(`${url}: HTTP ${resp.status}`);
}

/// A non-2xx whose body says why, keeping the why. Some servers answer a
/// refusal with its reason as text — boss-inventory's warehouse-status
/// said 503 "… not configured" or 502 naming the failing leg — and a
/// status alone cannot tell "not configured" from "shipping is down"
/// (backlog 0dcb0200; since 89cf07d8 a shipping outage is one leg of a
/// 200, not a refusal). The caller reads the body (`await r.text()`);
/// this stays pure so it has one test.
export function failedWithReason(status: number, body: string): ReadState {
  const reason = body.trim();
  return failedRead(reason ? `HTTP ${status}: ${reason}` : `HTTP ${status}`);
}

/// Anything whose failed arm carries an error — a `PagedResult`, a
/// `Remote` — collapsed to that arm alone. `loading` is not a failure:
/// a read still in flight has not said anything yet.
export type Fallible =
  | { readonly kind: 'failed'; readonly error: string }
  | { readonly kind: 'ready' | 'loading' | 'ok' };

export function readStateOf(result: Fallible): ReadState {
  return result.kind === 'failed' ? failedRead(result.error) : okRead;
}

/// A read named by what it fetched, so a banner can say which one
/// failed rather than "something went wrong".
export type NamedRead = Readonly<{ source: string; state: ReadState }>;

/// What a list surface renders.
export type ListView =
  | { kind: 'failed'; source: string; error: string }
  | { kind: 'loading'; source: string }
  | { kind: 'empty' }
  | { kind: 'rows' };

/// The reads are checked BEFORE the row count, deliberately, and in the
/// order the page declares them — its primary dataset first, so the
/// banner names the read an operator should chase. Three separate rules:
/// zero rows because a read failed must not look like zero rows because
/// there are none; a failed read is still reported when rows happened
/// to build from a partial join or a previous render, because reporting
/// those numbers as complete is the quieter half of the bug; and zero
/// rows because a read has not answered yet is neither — it is loading.
/// A failure outranks a pending read: it is the one thing already known.
export function listView(
  reads: ReadonlyArray<NamedRead>,
  rowCount: number,
): ListView {
  const failure = reads.find((r) => r.state.kind === 'failed');
  if (failure && failure.state.kind === 'failed') {
    return {
      kind: 'failed',
      source: failure.source,
      error: failure.state.error,
    };
  }
  const pending = reads.find((r) => r.state.kind === 'loading');
  if (pending) return { kind: 'loading', source: pending.source };
  return rowCount === 0 ? { kind: 'empty' } : { kind: 'rows' };
}

/// A filter label with its count — only when the read behind the count
/// answered. "All (0)" beside a failure alert, or while the read is in
/// flight, states a zero the data never said (backlog 82674b2b); with no
/// answer the label carries no number at all.
export function countLabel(label: string, read: ReadState, count: number): string {
  return read.kind === 'ok' ? `${label} (${count})` : label;
}

/// What a blank cell means. With the failed arm discarded there is no
/// answer to this: `isCapped(null)` is false, so the overflow banner
/// cannot fire either, and the page has no surface at all that could
/// report the read failed — an em-dash reads as "this row genuinely has
/// none" whichever it was. A read that has not answered cannot say
/// "absent" either.
export function blankMeaning(read: ReadState): 'absent' | 'unknown' {
  return read.kind === 'ok' ? 'absent' : 'unknown';
}

// ---------------------------------------------------------------------
// WHAT AN EMPTY LIST SAYS (backlog 0ef5e008).
// ---------------------------------------------------------------------
//
// Seven list pages had one empty branch, `visible.length === 0` →
// "No X match those filters.", which blamed the operator's filters for
// a source that was simply empty. VendorsList (35aeb30d), WatchlistPage
// (9289e682) and MarketingAssetsList had each grown the missing branch
// by hand — three copies of one idea, and seven pages still without it
// (CLAUDE.md 9a). An empty list is one of three facts, and they are
// answered here once: the read FAILED (and says which read), NOTHING
// exists, or the FILTERS hid everything that does. ListEmpty.svelte
// renders the answer; no page writes a "match those filters" line of its
// own, and a pin (an-empty-list-says-why.test.ts) holds the tree to it.

/// What a filtered list's empty surface is. `rows` is the one answer
/// with no empty line: there is something to show.
export type EmptyState =
  | { kind: 'loading' }
  | { kind: 'failed'; error: string }
  | { kind: 'none' }
  | { kind: 'filtered' }
  | { kind: 'rows' };

/// A failure names the read it came from. `fetchPaged` and the shape
/// checks already put the path in the error; a thrown network error
/// ("Failed to fetch") does not, and a line that cannot say WHICH read
/// failed sends the operator to find out. An error that already names an
/// API path is left as it is — a list built from several reads names the
/// one that failed, which is more than the list's own source can.
function naming(source: string, error: string): string {
  return error.includes(source) || error.includes('/api/') ? error : `${source}: ${error}`;
}

/// The list's reads (primary first, as `listView` takes them), how many
/// rows they built, and how many the filters left. The reads are judged
/// before either count, for `listView`'s reasons.
export function emptyState(
  reads: ReadonlyArray<NamedRead>,
  sourceCount: number,
  visibleCount: number,
): EmptyState {
  const view = listView(reads, sourceCount);
  switch (view.kind) {
    case 'failed':
      return { kind: 'failed', error: naming(view.source, view.error) };
    case 'loading':
      return { kind: 'loading' };
    case 'empty':
      return { kind: 'none' };
    case 'rows':
      return visibleCount === 0 ? { kind: 'filtered' } : { kind: 'rows' };
  }
}

/// A page's words for its empty list. `what` completes "Couldn't load
/// …" ("the roster", "your inbox"); `noun` is the rows' plural; `none`
/// replaces "No <noun> yet." where the page knows why there are none.
export type EmptyWords = Readonly<{ what: string; noun: string; none?: string }>;

/// The line an empty list shows, or null when there are rows. `alert`
/// marks the failure line, which pages render as `role="alert"`.
export function emptyLine(
  view: EmptyState,
  words: EmptyWords,
): Readonly<{ text: string; alert: boolean }> | null {
  switch (view.kind) {
    case 'failed':
      return { text: `Couldn't load ${words.what} — ${view.error}`, alert: true };
    case 'loading':
      return { text: 'Loading…', alert: false };
    case 'none':
      return { text: words.none ?? `No ${words.noun} yet.`, alert: false };
    case 'filtered':
      return { text: `No ${words.noun} match those filters.`, alert: false };
    case 'rows':
      return null;
  }
}
