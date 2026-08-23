// Date formatters shared across the SPA. Mirrors the money.ts deal:
// every surface that displays a date should route through these so
// the style is consistent and easy to change once. Ad-hoc
// `new Date(iso).toLocaleDateString()` call sites are a smell to fix
// on sight — they render differently per machine locale AND can
// shift the calendar day under the viewer's timezone offset.
//
// Locale-stable: output is pinned to the en-US short form the app
// already uses everywhere ("Aug 22, 2026"), independent of the
// runtime locale.
//
// `formatRelative` deliberately takes `now` as an explicit argument
// instead of defaulting to wallclock: pages must pass `appNow()`
// (from @boss/web-kit/sim-clock) so sim-dated data renders relative
// to sim time — the v1.0.5 sweep swapped 35 of 41 `new Date()` sites
// to `appNow()` for exactly this reason, and a hidden wallclock
// default here would quietly reintroduce the bug. (Importing
// sim-clock from this module is not an option: it's a runes module,
// and this one stays pure so it runs under plain `bun test`.)

const MONTHS = [
  'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
  'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
] as const;

const DAYS_IN_MONTH = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31] as const;

/// Parse the leading `YYYY-MM-DD` of an ISO date or timestamp.
/// Returns null when the input doesn't carry a plausible date.
function parseIsoDate(
  iso: string,
): { year: number; month: number; day: number } | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})(?:[T ]|$)/.exec(iso);
  if (!m) return null;
  const year = Number(m[1]);
  const month = Number(m[2]);
  const day = Number(m[3]);
  if (month < 1 || month > 12) return null;
  if (day < 1 || day > (DAYS_IN_MONTH[month - 1] ?? 31)) return null;
  return { year, month, day };
}

/// `"2026-08-22"` (or any ISO timestamp) → `"Aug 22, 2026"`.
///
/// String-parsed, never Date-parsed: the calendar date the wire
/// carries is the calendar date rendered, regardless of the
/// viewer's timezone. Non-date input passes through unchanged —
/// rendering the raw value is honest; inventing a date is not.
export function formatDate(iso: string): string {
  const d = parseIsoDate(iso);
  if (!d) return iso;
  return `${MONTHS[d.month - 1]} ${d.day}, ${d.year}`;
}

export type DateTimeFormatOptions = Readonly<{
  /// IANA zone for rendering (e.g. `'UTC'`). Defaults to the
  /// viewer's local zone — wallclock timestamps (audit rows, "last
  /// seen" columns) read naturally there. Tests pin `'UTC'`.
  timeZone?: string;
}>;

/// ISO timestamp → `"Aug 22, 2026, 14:30"` (en-US date, 24h time).
/// Non-timestamp input passes through unchanged.
///
/// Assembled from `formatToParts` rather than `toLocaleString`,
/// because the joiner between date and time ("," vs " at ") drifts
/// across ICU versions and locale-stable means stable.
export function formatDateTime(
  iso: string,
  opts: DateTimeFormatOptions = {},
): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return iso;
  const parts = new Intl.DateTimeFormat('en-US', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
    ...(opts.timeZone ? { timeZone: opts.timeZone } : {}),
  }).formatToParts(new Date(t));
  const p = new Map(parts.map((x) => [x.type, x.value]));
  const month = p.get('month');
  const day = p.get('day');
  const year = p.get('year');
  const hour = p.get('hour');
  const minute = p.get('minute');
  if (!month || !day || !year || !hour || !minute) return iso;
  return `${month} ${day}, ${year}, ${hour}:${minute}`;
}

/// Compact "how long ago" — `'today'`, `'3d'`, `'2mo'`, `'1y'`.
///
/// Semantics absorbed from the two byte-identical `daysAgo` helpers
/// in apps/web/src/accounts/{NotesPanel,ActivityTimeline}.svelte:
/// whole-day buckets, months are floor(d/30), years floor(d/365),
/// and anything under one day — including future dates — is
/// `'today'`. No "ago" suffix: the call site's copy decides that
/// ("3d" in a timeline column, "3d ago" in prose).
///
/// `now` is required — pass `appNow()`, not `new Date()` (see the
/// module comment).
export function formatRelative(iso: string, now: Date): string {
  const then = new Date(iso).getTime();
  const d = Math.floor((now.getTime() - then) / 86_400_000);
  if (d < 1) return 'today';
  if (d === 1) return '1d';
  if (d < 30) return `${d}d`;
  if (d < 365) return `${Math.floor(d / 30)}mo`;
  return `${Math.floor(d / 365)}y`;
}
