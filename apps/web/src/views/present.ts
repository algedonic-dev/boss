// What /ux/views says, as pure functions (page audit d2e86594,
// 2026-09-27). Each one is a sentence the page used to get wrong or
// leave out; ViewsPage.svelte renders them and present.test.ts pins
// them without a browser.

import { entityHref } from '@boss/web-kit/ui/entity-href';
import { href } from '@boss/web-kit/nav';
import type { SessionState, ViewerRead } from '@boss/web-kit/session/classify';
import { subjectPath } from '../jobs/types';
import { ceilingLabel } from './sources';
import type { ViewResults, ViewSource, ViewSources } from './types';

/// The list's answer when the session resolved to nobody. `load()`
/// returns early without a viewer, and `loading` used to start true
/// and stay true, so an unauthenticated or unresolved session read
/// "Loading views…" for good — silence dressed as progress (backlog
/// 04754a40).
export const NO_VIEWER_LINE = 'No signed-in viewer, so no Views to list.';

export function hasNoViewer(kind: SessionState['kind']): boolean {
  return kind !== 'loading' && kind !== 'ready';
}

export type WriteOutcome = { kind: 'done' } | { kind: 'refused'; reason: string };

type Fetch = (url: string, init?: RequestInit) => Promise<Response>;

/// One View write — POST, PUT or DELETE — resolved to done or
/// refused-with-a-reason, never a throw. A refusal names the status and
/// the server's own body ("HTTP 422, invalid filter: …"), so the author
/// can fix a filter typo without guessing; a fetch that never reached
/// the server is refused with what was thrown.
export async function sendWrite(url: string, init: RequestInit, fetchFn: Fetch = (u, i) => fetch(u, i)): Promise<WriteOutcome> {
  try {
    const r = await fetchFn(url, init);
    if (r.ok) return { kind: 'done' };
    const said = (await r.text().catch(() => '')).trim();
    return { kind: 'refused', reason: said ? `HTTP ${r.status}, ${said}` : `HTTP ${r.status}` };
  } catch (e) {
    return { kind: 'refused', reason: e instanceof Error ? e.message : String(e) };
  }
}

/// A write refusal, said as the write's (backlog 0148c461). Delete used
/// to write the PAGE's error, and the list branch then replaced every
/// View with a bare "HTTP 403" — a refused write rendered as a failed
/// read.
export function refusedLine(verb: 'delete' | 'save', reason: string): string {
  return `${verb} refused: ${reason}`;
}

/// The rows a weak scan examined, named by the View's own source
/// (backlog 3d8178e1): the line said "events" for jobs, steps and
/// subjects Views too, sending the reader to the wrong table.
///
/// The ceiling is the one `GET /api/views/sources` serves (backlog
/// 4a8939b5) — this file held a '5,000' copy of query.rs SCAN_CEILING
/// until the two /ux/views cars met. Unread, the line names the source
/// and no number, rather than a number the server never said.
export function scannedRows(served: ViewSources | null, source: ViewSource): string {
  return served ? `newest ${ceilingLabel(served)} ${source}` : `newest ${source}`;
}

/// How many rows a result shows of how many matched, and when it was
/// read (backlog 22d5bfcd). The page asks limit=100 and `matched`
/// counts to the scan ceiling, so "4000 matches" over 100 rows said
/// nothing between them; and a result stays up, unrefreshed, until the
/// next Run. Local time, because it is the viewer's clock they compare
/// it against.
export function shownLine(res: ViewResults, readAt: Date): string {
  const hh = String(readAt.getHours()).padStart(2, '0');
  const mm = String(readAt.getMinutes()).padStart(2, '0');
  return `showing ${res.rows.length} of ${res.matched} · read ${hh}:${mm}`;
}

function text(v: unknown): string | null {
  return typeof v === 'string' && v.length > 0 ? v : null;
}

/// Where a result cell points, or null for a cell that names nothing
/// (backlog 00ab7ddd). A View that finds packets but cannot open them
/// sends the operator to copy ids by hand — the spreadsheet this page
/// exists to replace. A step's page needs its job, so a step id links
/// only when the row carries `job_id`; a subject's needs its kind.
export function cellHref(source: ViewSource, column: string, row: Readonly<Record<string, unknown>>): string | null {
  const value = text(row[column]);
  if (value === null) return null;
  if (source === 'jobs' && column === 'id') return entityHref('job', value);
  if (source === 'steps' && column === 'job_id') return entityHref('job', value);
  if (source === 'steps' && column === 'id') {
    const job = text(row['job_id']);
    if (job === null) return null;
    const back = new URLSearchParams({ from: '/ux/views', from_label: 'Views' });
    return href(`/ux/jobs/${encodeURIComponent(job)}/steps/${encodeURIComponent(value)}?${back}`);
  }
  if (source === 'subjects' && column === 'id') {
    const kind = text(row['kind']);
    return kind === null ? null : href(subjectPath({ subject_kind: kind, id: value }));
  }
  return null;
}

/// Who shared a View, from their people row. An owner with no row (an
/// agent, a guest) is its id; a row that could not be read says so,
/// rather than passing the id off as the answer.
export function sharerName(ownerId: string, read: ViewerRead | undefined): string {
  if (read?.kind === 'found') return read.employee.name;
  if (read?.kind === 'failed') return `${ownerId} (name unread: ${read.error})`;
  return ownerId;
}
