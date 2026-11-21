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

/// The two halves of one instant: the viewer's local reading and the
/// stack's own (UTC). Kept apart so a surface can ink the UTC half as
/// secondary rather than parse a joined string back up.
export type LocalAndUtc = Readonly<{ local: string; utc: string }>;

export type LocalAndUtcOptions = DateTimeFormatOptions &
  Readonly<{
    /// `hh:mm:ss` rather than `hh:mm`.
    seconds?: boolean;
    /// Lead each half with its own calendar day (`Nov 5`), which can
    /// differ between the halves near midnight.
    date?: boolean;
  }>;

/// ISO timestamp → `{ local: '17:42 PT', utc: '01:42Z' }`; null for
/// input that is not an instant (a surface renders nothing rather than
/// an invented time).
///
/// David, user-feedback 0b1fd59f (2026-11-08): "We need to show UTC in
/// addition to local time since the factory runs on UTC." Design
/// b03ef2d4 decided the shape — `17:42 PT · 01:42Z` — and that this
/// module, the SPA's one date formatter, carries it rather than a
/// second. The zone name is Intl's `shortGeneric` (`PT`, not the
/// `PST`/`PDT` that flips twice a year); the UTC half is `Z`, the
/// spelling the record and the alarms already use.
export function formatLocalAndUtc(
  iso: string,
  opts: LocalAndUtcOptions = {},
): LocalAndUtc | null {
  if (iso === '') return null;
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return null;
  const read = (timeZone: string | undefined, zoneName: boolean): string | null => {
    const parts = new Intl.DateTimeFormat('en-US', {
      hour: '2-digit',
      minute: '2-digit',
      hourCycle: 'h23',
      ...(opts.seconds ? { second: '2-digit' } : {}),
      ...(opts.date ? { month: 'short', day: 'numeric' } : {}),
      ...(zoneName ? { timeZoneName: 'shortGeneric' } : {}),
      ...(timeZone ? { timeZone } : {}),
    } as const).formatToParts(at);
    const p = new Map(parts.map((x) => [x.type, x.value]));
    const hour = p.get('hour');
    const minute = p.get('minute');
    if (!hour || !minute) return null;
    const clock = `${hour}:${minute}${opts.seconds ? `:${p.get('second') ?? '00'}` : ''}`;
    const day = opts.date ? `${p.get('month') ?? ''} ${p.get('day') ?? ''} ` : '';
    const zone = p.get('timeZoneName');
    return zoneName ? `${day}${clock}${zone ? ` ${zone}` : ''}` : `${day}${clock}Z`;
  };
  const local = read(opts.timeZone, true);
  const utc = read('UTC', false);
  return local === null || utc === null ? null : { local, utc };
}

/// The pair as one line of text — `17:42 PT · 01:42Z` — for a surface
/// that cannot ink the halves apart (a title attribute, a sentence).
export const localAndUtcText = (t: LocalAndUtc): string => `${t.local} · ${t.utc}`;

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
