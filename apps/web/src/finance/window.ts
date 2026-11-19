// The trailing-year window the Finance headline reads the income
// statement over. It lived in exec/exec.ts, beside the Exec page's
// revenue card that read the same window, until that page went with step
// 1 of design 01fa5bdf. A module of its own, importing nothing, because
// the mocked Finance spec reads it too and cannot load ledger.ts's rune
// imports outside a Svelte build.

/// The year ending `today` (YYYY-MM-DD): the same date a year earlier
/// through today. A date it cannot read is a one-day window on it.
export function trailingYear(today: string): Readonly<{ from: string; to: string }> {
  const d = new Date(`${today}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return { from: today, to: today };
  d.setUTCFullYear(d.getUTCFullYear() - 1);
  return { from: d.toISOString().slice(0, 10), to: today };
}
