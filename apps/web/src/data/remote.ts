// Remote data as a discriminated union (packet 3fba9c35).
//
// The false-empty class this kills: a component fetches its primary
// dataset with `if (r.ok) { data = ... }` and a silent catch, so an
// outage renders the page's normal empty state — "No messages", "No
// invoices found" — indistinguishable from "loaded fine and truly
// empty". A surface that stores a Remote<T> has to branch on `failed`
// to render at all, so the failure is visible by construction.
//
// House style (CLAUDE.md §TypeScript): discriminated unions for
// state; parsing happens once, at the fetch call site, via `parse`.

export type Remote<T> =
  | { kind: 'loading' }
  | { kind: 'ready'; data: T }
  | { kind: 'failed'; error: string };

/// One GET, resolved to ready-or-failed (never loading — the caller
/// owns when to show a spinner). Non-ok statuses, thrown network
/// errors, and a `parse` that throws all land in `failed` with a
/// message fit for inline rendering.
export async function fetchRemote<T>(
  url: string,
  parse: (raw: unknown) => T,
): Promise<Exclude<Remote<T>, { kind: 'loading' }>> {
  try {
    const r = await fetch(url);
    if (!r.ok) return { kind: 'failed', error: `${url}: HTTP ${r.status}` };
    return { kind: 'ready', data: parse(await r.json()) };
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    return { kind: 'failed', error: detail };
  }
}
