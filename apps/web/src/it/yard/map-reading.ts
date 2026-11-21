import type { Remote } from '../../data/remote';

/** A cold owned build is progress, never an empty successful map or an outage. */
export async function fetchMapReading<T>(url: string, parse: (raw: unknown) => T): Promise<Remote<T>> {
  try {
    const response = await fetch(url);
    if (!response.ok) return { kind: 'failed', error: `${url}: HTTP ${response.status}` };
    const raw: unknown = await response.json();
    if (response.status === 202) {
      if (raw !== null && typeof raw === 'object' && 'state' in raw && raw.state === 'building'
        && 'map_as_of' in raw && raw.map_as_of === null && 'why' in raw && typeof raw.why === 'string' && raw.why.trim() !== '') {
        return { kind: 'loading' };
      }
      return { kind: 'failed', error: `${url}: invalid building response` };
    }
    return { kind: 'ready', data: parse(raw) };
  } catch (error) {
    return { kind: 'failed', error: error instanceof Error ? error.message : String(error) };
  }
}
