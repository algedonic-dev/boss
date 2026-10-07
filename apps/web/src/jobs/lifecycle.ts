import type { JobStatus } from './types';
import type { LifecycleListedStep } from './listedEnvelope';

export function liveSteps<S extends LifecycleListedStep>(packet: Readonly<{ steps?: readonly S[] }>): readonly S[] {
  return (packet.steps ?? [])
    .filter(step => step.status === 'ready' || step.status === 'active')
    .sort((a, b) => a.sort_order - b.sort_order || a.id.localeCompare(b.id));
}

function calendarDay(value: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return null;
  const instant = Date.parse(`${value}T00:00:00Z`);
  return Number.isFinite(instant) && new Date(instant).toISOString().slice(0, 10) === value
    ? instant : null;
}

/** Calendar days, never invented sub-day precision for legacy date-only packets. */
export function openAge(packet: Readonly<{
  status: JobStatus; opened_on: string; opened_at?: string | null;
}>, today: string): string {
  if (packet.status === 'closed' || packet.status === 'cancelled') return '—';
  const stamp = packet.opened_at;
  const instant = stamp ? Date.parse(stamp) : null;
  if (stamp && (instant === null || !Number.isFinite(instant))) return 'Unknown';
  const opened = calendarDay(instant === null ? packet.opened_on : new Date(instant).toISOString().slice(0, 10));
  const current = calendarDay(today);
  if (opened === null || current === null) return 'Unknown';
  const days = (current - opened) / 86_400_000;
  if (days < 0) return 'Opened in the future';
  return `${days} ${days === 1 ? 'day' : 'days'}${stamp ? '' : ' (date only)'}`;
}
