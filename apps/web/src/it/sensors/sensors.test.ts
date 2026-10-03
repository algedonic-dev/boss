import { expect, test } from 'bun:test';
import { readSensors, groupSensors } from './sensors';

const at = '2026-10-02T20:00:00.000Z';
const since = '2026-10-01T20:00:00.000Z';
const sensor = { id: 'mail-support', source: 'mail', selector: 'support@example.test', credential: 'mail-account', opens_kind: 'receive-a-message', every_minutes: 5, enabled: true, last_polled_at: '2026-10-02T19:55:00Z' };
const window = { sensor_id: sensor.id, since, until: at, arrived: 3, stamped: 2, unstamped: 1, packets: ['packet-a', 'packet-b'] };
const read = async (url: string): Promise<unknown> => url === '/api/sensors' ? { data: [sensor], total: 1 } : window;

test('registry is the sole row list; every count is the exact bounded readings response', async () => {
  const calls: string[] = [];
  const result = await readSensors(async (url) => { calls.push(url); return read(url); }, at);
  expect(result.rows).toHaveLength(1);
  expect(result.rows[0]?.sensor).toEqual(sensor);
  expect(result.rows[0]?.readings).toEqual(window);
  expect(calls[1]).toContain(`since=${encodeURIComponent(since)}`);
  expect(calls[1]).toContain(`until=${encodeURIComponent(at)}`);
});
test('missing/short/duplicate registry cannot announce healthy empty', async () => {
  for (const data of [{ data: [], total: 1 }, { data: [sensor, sensor], total: 2 }, { total: 0 }, { data: [], total: null }]) {
    await expect(readSensors(async () => data, at)).rejects.toThrow();
  }
  expect((await readSensors(async () => ({ data: [], total: 0 }), at)).rows).toEqual([]);
});
test('wrong sensor, window, false counts and broken conservation make a row unavailable', async () => {
  for (const bad of [{ ...window, sensor_id: 'other' }, { ...window, since: '2026-10-01T19:00:00Z' }, { ...window, arrived: false }, { ...window, arrived: 4 }, { ...window, packets: ['same', 'same'] }]) {
    const result = await readSensors(async (url) => url === '/api/sensors' ? { data: [sensor], total: 1 } : bad, at);
    expect(result.rows[0]?.readings).toBeNull();
    expect(result.rows[0]?.error).toBeTruthy();
  }
});
test('HTTP refusal is unavailable rather than zero, and registry failures stay failures', async () => {
  await expect(readSensors(async () => { throw new Error('403 unavailable'); }, at)).rejects.toThrow('403');
  const result = await readSensors(async (url) => url === '/api/sensors' ? { data: [sensor], total: 1 } : Promise.reject(new Error('readings failed')), at);
  expect(result.rows[0]?.readings).toBeNull();
  expect(result.rows[0]?.error).toBe('readings failed');
});
test('groups use credential registry identity, including new rows and credential-free sources', async () => {
  const other = { ...sensor, id: 'mail-finance', selector: 'finance@example.test' };
  const push = { ...sensor, id: 'site-visits', source: 'site', credential: '', every_minutes: 0, selector: null };
  const result = await readSensors(async (url) => url === '/api/sensors' ? { data: [other, push, sensor], total: 3 } : { ...window, sensor_id: decodeURIComponent(url.split('/')[3] ?? '') }, at);
  expect(groupSensors(result.rows).map((g) => [g.credential, g.rows.map((r) => r.sensor.id)])).toEqual([['', ['site-visits']], ['mail-account', ['mail-finance', 'mail-support']]]);
});
test('the existing sensor API omits an absent optional selector', async () => {
  const { selector: _selector, ...withoutSelector } = sensor;
  const result = await readSensors(async (url) => url === '/api/sensors' ? { data: [withoutSelector], total: 1 } : window, at);
  expect(result.rows[0]?.sensor.selector).toBeNull();
});
