/** Chrono's UTC seconds plus nanosecond field. A leap second keeps its
 *  preceding second and a nanosecond field >=1e9, as DateTime does. */
export type ProofInstant = Readonly<{ seconds: bigint; nanos: bigint }>;
export type ProofNow = number | string | null;
const SECOND = 1_000_000_000n;
const DAY = 86_400n;
const calendarSecond = (year: number, month: number, day: number, hour: number, minute: number, second: number): bigint => {
  const date = new Date(0);
  date.setUTCFullYear(year, month - 1, day);
  date.setUTCHours(hour, minute, second, 0);
  return BigInt(date.getTime() / 1000);
};
// chrono::NaiveDate MIN/MAX; exact checked boundaries are in the shared corpus.
const MIN_SECOND = calendarSecond(-262143, 1, 1, 0, 0, 0);
const MAX_SECOND = calendarSecond(262142, 12, 31, 23, 59, 59);

export function proofInstant(value: unknown): ProofInstant | null {
  if (typeof value !== 'string') return null;
  const p = /^(\d{4})-(\d\d)-(\d\d)[Tt ](\d\d):(\d\d):(\d\d)(?:\.(\d+))?([Zz]|[+-]\d\d:\d\d)$/.exec(value);
  if (p === null) return null;
  const [year, month, day, hour, minute, second] = p.slice(1, 7).map(Number);
  const leap = year! % 4 === 0 && (year! % 100 !== 0 || year! % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (month! < 1 || month! > 12 || day! < 1 || day! > days[month! - 1]! || hour! > 23 || minute! > 59 || second! > 60) return null;
  const zone = p[8]!;
  if (zone.length > 1 && (Number(zone.slice(1, 3)) > 23 || Number(zone.slice(4, 6)) > 59)) return null;
  const offset = zone.length === 1 ? 0 : (zone[0] === '+' ? 1 : -1) * (Number(zone.slice(1, 3)) * 3600 + Number(zone.slice(4, 6)) * 60);
  const seconds = calendarSecond(year!, month!, day!, hour!, minute!, Math.min(second!, 59)) - BigInt(offset);
  const nanos = BigInt((p[7] ?? '').slice(0, 9).padEnd(9, '0')) + (second === 60 ? SECOND : 0n);
  return { seconds, nanos };
}

export function proofNow(value: ProofNow): ProofInstant | null {
  if (typeof value === 'string') return proofInstant(value);
  if (value === null || !Number.isSafeInteger(value) || !Number.isFinite(new Date(value).getTime())) return null;
  const ms = BigInt(value);
  const remainder = (ms % 1000n + 1000n) % 1000n;
  return { seconds: (ms - remainder) / 1000n, nanos: remainder * 1_000_000n };
}
export const compareInstants = (a: ProofInstant, b: ProofInstant): number => a.seconds === b.seconds
  ? a.nanos < b.nanos ? -1 : a.nanos > b.nanos ? 1 : 0 : a.seconds < b.seconds ? -1 : 1;

/** Chrono's signed duration counts each represented leap second exactly
 *  as NaiveTime::signed_duration_since, then truncates whole seconds. */
export function elapsedSeconds(a: ProofInstant, b: ProofInstant): bigint {
  let seconds = a.seconds - b.seconds;
  const tod = (s: bigint) => (s % DAY + DAY) % DAY;
  if (tod(a.seconds) > tod(b.seconds) && b.nanos >= SECOND) seconds++;
  else if (tod(a.seconds) < tod(b.seconds) && a.nanos >= SECOND) seconds--;
  return (seconds * SECOND + a.nanos - b.nanos) / SECOND;
}

export function checkedDeadline(begin: ProofInstant, duration: bigint): ProofInstant | null {
  // chrono::Duration::try_seconds and DateTime::checked_add_signed.
  if (duration <= 0n || duration > 9_223_372_036_854_775n) return null;
  const seconds = begin.seconds + duration;
  if (seconds < MIN_SECOND || seconds > MAX_SECOND) return null;
  return { seconds, nanos: begin.nanos >= SECOND ? begin.nanos - SECOND : begin.nanos };
}

export function clockText(at: ProofInstant): string {
  const date = new Date(Number(at.seconds) * 1000);
  const year = date.getUTCFullYear();
  const y = year >= 0 && year <= 9999 ? String(year).padStart(4, '0') : `${year < 0 ? '-' : '+'}${String(Math.abs(year)).padStart(4, '0')}`;
  const iso = date.toISOString();
  const time = iso.slice(iso.indexOf('T') + 1, -5);
  const fraction = at.nanos % SECOND;
  const digits = fraction % 1_000_000n === 0n ? 3 : fraction % 1000n === 0n ? 6 : 9;
  const nanos = fraction === 0n ? '' : `.${fraction.toString().padStart(9, '0').slice(0, digits)}`;
  return `${y}-${String(date.getUTCMonth() + 1).padStart(2, '0')}-${String(date.getUTCDate()).padStart(2, '0')} ${at.nanos >= SECOND ? `${time.slice(0, -2)}60` : time}${nanos} UTC`;
}
