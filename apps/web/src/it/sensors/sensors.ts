// Registry rows select the sources; this surface reads their existing receipts.
export type Sensor = Readonly<{ id: string; source: string; credential: string; opens_kind: string; every_minutes: number; enabled: boolean; selector: string | null; last_polled_at: string | null }>;
export type Readings = Readonly<{ sensor_id: string; since: string; until: string; arrived: number; stamped: number; unstamped: number; packets: ReadonlyArray<string> }>;
export type SensorReading = Readonly<{ sensor: Sensor; readings: Readings | null; error: string | null }>;
export type SensorsView = Readonly<{ since: string; until: string; rows: ReadonlyArray<SensorReading> }>;

function object(v: unknown): Record<string, unknown> {
  if (!v || typeof v !== 'object' || Array.isArray(v)) throw new Error('Sensor response is not an object');
  return v as Record<string, unknown>;
}
function text(v: unknown): string {
  if (typeof v !== 'string' || v.trim() === '') throw new Error('Sensor identity is unavailable');
  return v;
}
function count(v: unknown): number {
  if (typeof v !== 'number' || !Number.isSafeInteger(v) || v < 0) throw new Error('Sensor count is unavailable');
  return v;
}
function instant(v: unknown): bigint {
  const t = text(v), m = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/.exec(t);
  if (!m) throw new Error('Sensor time is unavailable');
  const ms = Date.parse(`${m[1]}${m[3]}`);
  if (!Number.isFinite(ms)) throw new Error('Sensor time is unavailable');
  return BigInt(ms) * 1_000_000n + BigInt((m[2] ?? '').padEnd(9, '0'));
}
function sensor(raw: unknown): Sensor {
  const v = object(raw);
  const selector = v.selector ?? null;
  if (typeof v.credential !== 'string' || typeof v.enabled !== 'boolean' || !(selector === null || typeof selector === 'string') || !(v.last_polled_at === null || typeof v.last_polled_at === 'string')) throw new Error('Sensor registry row is incomplete');
  if (v.last_polled_at !== null) instant(v.last_polled_at);
  return { id: text(v.id), source: text(v.source), credential: v.credential, opens_kind: text(v.opens_kind), every_minutes: count(v.every_minutes), enabled: v.enabled, selector, last_polled_at: v.last_polled_at };
}
function readings(raw: unknown, id: string, since: string, until: string): Readings {
  const v = object(raw);
  const arrived = count(v.arrived), stamped = count(v.stamped), unstamped = count(v.unstamped);
  if (v.sensor_id !== id || instant(v.since) !== instant(since) || instant(v.until) !== instant(until) || stamped + unstamped !== arrived || !Array.isArray(v.packets)) throw new Error('Sensor window or counts contradict the request');
  const packets = v.packets.map(text);
  if (new Set(packets).size !== packets.length || packets.length > stamped) throw new Error('Sensor packet receipts are ambiguous');
  return { sensor_id: id, since: text(v.since), until: text(v.until), arrived, stamped, unstamped, packets };
}
export async function readSensors(read: (url: string) => Promise<unknown>, at: string): Promise<SensorsView> {
  instant(at);
  const since = new Date(Date.parse(at) - 24 * 60 * 60_000).toISOString();
  const body = object(await read('/api/sensors'));
  if (!Array.isArray(body.data) || count(body.total) !== body.data.length) throw new Error('Sensor registry is incomplete');
  const sensors = body.data.map(sensor);
  if (new Set(sensors.map((s) => s.id)).size !== sensors.length) throw new Error('Sensor registry identity is ambiguous');
  const rows = await Promise.all(sensors.map(async (s): Promise<SensorReading> => {
    try {
      const url = `/api/sensors/${encodeURIComponent(s.id)}/readings?since=${encodeURIComponent(since)}&until=${encodeURIComponent(at)}`;
      return { sensor: s, readings: readings(await read(url), s.id, since, at), error: null };
    } catch (e) {
      return { sensor: s, readings: null, error: e instanceof Error ? e.message : String(e) };
    }
  }));
  return { since, until: at, rows };
}
export function groupSensors(rows: ReadonlyArray<SensorReading>): ReadonlyArray<Readonly<{ credential: string; rows: ReadonlyArray<SensorReading> }>> {
  return [...new Set(rows.map((r) => r.sensor.credential))].sort().map((credential) => ({ credential, rows: rows.filter((r) => r.sensor.credential === credential).sort((a, b) => a.sensor.id.localeCompare(b.sensor.id)) }));
}
