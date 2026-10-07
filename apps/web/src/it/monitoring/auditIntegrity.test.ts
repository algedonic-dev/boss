import { describe, expect, test } from 'bun:test';
import { ageText, nativeTime, observation, readIntegrity } from './auditIntegrity';

const ID = '65dd7f6f-4c09-45f7-9065-7b4702707dcd';
const NEXT = '235ae1ee-77fc-4e16-9713-be6a3c167bc6';
const at = '2026-10-03T03:30:02.100000001Z';
const report = { version: 1, checked_at: '2026-10-03T03:30:23.551906213Z',
  chain: { state: 'intact', total_rows: 1127907, chain_break_count: 0, regression_count: 0,
    dangling_ref_count: 0, gap_count: 3, missing_ids: 45, gap_reading: 'sequence-burn' },
  drift: { state: 'covered', kinds: [] } };
const row = (id = ID, opened_at = at) => ({ id, opened_at, kind: 'maintenance-audit-integrity',
  partition: 'real', simulated: false, status: 'closed' });
const detail = (value: unknown = report) => ({ ...row(), steps: [{ spec_slug: 'run',
  completed_at: '2026-10-03T03:30:23.936011Z', metadata: { result: 'ok', exit_status: '0', audit_integrity: value } }] });
const response = (value: unknown, status = 200) => Promise.resolve(new Response(JSON.stringify(value), { status }));

describe('independent recorded integrity observations', () => {
  test('sequence burns with an intact chain do not become chain breaks', () => {
    expect(observation(report)?.chain).toBe('intact');
    expect(observation(report)?.gaps).toBe(3);
  });
  test('warning drift and unavailable queries remain independent of overall success', () => {
    expect(observation({ ...report, drift: { state: 'undeclared', kinds: ['new.kind'] } })?.drift)
      .toEqual({ kind: 'undeclared', kinds: ['new.kind'] });
    expect(observation({ ...report, drift: { state: 'unavailable', error: 'read failed' } })?.drift)
      .toEqual({ kind: 'unavailable', error: 'read failed' });
  });
  test('missing, unknown version, typed-count errors and contradictory coverage stay unknown', () => {
    for (const value of [null, {}, { ...report, version: '1' },
      { ...report, chain: { ...report.chain, total_rows: '7' } },
      { ...report, chain: { ...report.chain, chain_break_count: 1 } },
      { ...report, chain: { ...report.chain, gap_reading: ['sequence-burn'] } },
      { ...report, drift: { state: 'covered', kinds: ['unregistered'] } },
      { ...report, drift: { state: 'unavailable', error: '' } }]) expect(observation(value)).toBeNull();
  });
  test('native chronology retains nanoseconds and refuses malformed input', () => {
    expect(nativeTime(at)! < nativeTime('2026-10-03T03:30:02.100000002Z')!).toBe(true);
    for (const value of [null, '', 'yesterday', '2026-10-03', '2026-02-31T03:30:23Z']) expect(nativeTime(value)).toBeNull();
  });
  test('age is measured from the check timestamp and never invents a future age', () => {
    expect(ageText('2026-10-03T03:00:00Z', Date.parse('2026-10-03T04:00:00Z'))).toBe('1 h ago');
    expect(ageText('2026-10-04T03:00:00Z', Date.parse('2026-10-03T04:00:00Z'))).toBe('age unavailable');
  });
});

describe('the counted owning packet read', () => {
  test('uses full newest detail rather than slim list fields', async () => {
    const urls: string[] = [];
    const actual = await readIntegrity((url) => {
      urls.push(url);
      return response(url.includes('?') ? { total: 1, data: [row()] } : detail());
    });
    expect(actual.kind).toBe('ready');
    expect(urls[0]).toContain('partition=real');
    expect(urls[1]).toBe(`/api/jobs/${ID}`);
  });
  test('the newest admission wins even when the previous check had a clean report', async () => {
    const newer = row(NEXT, '2026-10-03T03:30:02.100000002Z');
    const actual = await readIntegrity((url) => response(url.includes('?')
      ? { total: 2, data: [row(), newer] }
      : { ...newer, status: 'open', steps: [{ spec_slug: 'run', metadata: {} }] }));
    expect(actual.kind === 'ready' && actual.id).toBe(NEXT);
    expect(actual.kind === 'ready' && actual.observation).toBeNull();
  });
  test('ties refuse rather than choose arbitrary result by row order', async () => {
    for (const rows of [[row(), row(NEXT)], [row(NEXT), row()]]) {
      const actual = await readIntegrity(() => response({ total: 2, data: rows }));
      expect(actual.kind).toBe('unavailable');
    }
  });
  test('counted empty is distinct from refused, malformed and incomplete reads', async () => {
    expect((await readIntegrity(() => response({ total: 0, data: [] }))).kind).toBe('empty');
    for (const value of [{}, { total: 2, data: [] }, { total: '0', data: [] }, { total: 1, data: [row(), row()] }]) {
      expect((await readIntegrity(() => response(value))).kind).toBe('unavailable');
    }
    expect(await readIntegrity(() => response({}, 403))).toEqual({ kind: 'unavailable', message: 'HTTP 403' });
  });
  test('pagination verifies stable totals and unique population identities', async () => {
    const actual = await readIntegrity((url) => {
      const offset = new URL(url, 'http://fixture').searchParams.get('offset');
      return response(offset === '0' ? { total: 2, data: [row()] }
        : { total: 2, data: [row()] });
    });
    expect(actual.kind).toBe('unavailable');
  });
  test('historical prose and out-of-packet observation timestamps cannot manufacture a report', async () => {
    for (const value of [null, { ...report, checked_at: '2026-09-03T03:30:23Z' },
      { ...report, checked_at: '2026-11-03T03:30:23Z' }]) {
      const actual = await readIntegrity((url) => response(url.includes('?') ? { total: 1, data: [row()] } : detail(value)));
      expect(actual.kind === 'ready' && actual.observation).toBeNull();
    }
  });
});
