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

/// A failed read is `refused` when the server answered 401 or 403: it
/// is up and said no to this session, which is a missing grant to
/// chase, not a dead service (backlog 7abb184f). Absent on every other
/// failure, so a failed arm built by hand stays an ordinary failure.
export type Remote<T> =
  | { kind: 'loading' }
  | { kind: 'ready'; data: T }
  | { kind: 'failed'; error: string; refused?: true };

/// How much of a refusal's body a page line carries. The server's
/// reason is one sentence (`… may read no packets, so no station can be
/// evaluated for them`); a proxy's HTML refusal page is not, and the cut
/// says how much it left out rather than ending mid-word in silence.
const REASON_CHARS = 400;

function reasonOf(body: string): string {
  const text = body.trim().replace(/\s+/g, ' ');
  return text.length <= REASON_CHARS
    ? text
    : `${text.slice(0, REASON_CHARS)}… (${text.length - REASON_CHARS} more characters)`;
}

/// One GET, resolved to ready-or-failed (never loading — the caller
/// owns when to show a spinner). Non-ok statuses, thrown network
/// errors, and a `parse` that throws all land in `failed` with a
/// message fit for inline rendering.
///
/// A 401/403 keeps the body, because that is where the server says who
/// was refused and why — `/api/stations/load` names the caller and the
/// role it reads as. Until 7abb184f this kept only the status, so every
/// page reading the load said the read failed, and the one sentence that
/// would have sent the reader to the grant was dropped. The error keeps
/// the house shape `<url>: HTTP <status>`, followed by `: <reason>` when
/// there is one (as `failedWithReason` in readState.ts does).
export async function fetchRemote<T>(
  url: string,
  parse: (raw: unknown) => T,
): Promise<Exclude<Remote<T>, { kind: 'loading' }>> {
  try {
    const r = await fetch(url);
    if (r.status === 401 || r.status === 403) {
      const reason = reasonOf(await r.text().catch(() => ''));
      const error = `${url}: HTTP ${r.status}${reason === '' ? '' : `: ${reason}`}`;
      return { kind: 'failed', error, refused: true };
    }
    if (!r.ok) return { kind: 'failed', error: `${url}: HTTP ${r.status}` };
    return { kind: 'ready', data: parse(await r.json()) };
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    return { kind: 'failed', error: detail };
  }
}

/// The verb a page puts before a failed read's error: "the load read
/// failed: …" or "the load read was refused: …". One spelling, so the
/// pages that say it cannot drift apart.
export function readFailed(r: Readonly<{ kind: 'failed'; error: string; refused?: true }>): 'failed' | 'was refused' {
  return r.refused ? 'was refused' : 'failed';
}
