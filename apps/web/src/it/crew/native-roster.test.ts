import { describe, expect, test } from 'bun:test';
import { parseCapture, projectRoster, readRoster } from './native-roster';

const now = '2026-10-02T19:30:00.000Z';
function packet(id = 'one', ended = '2026-10-02T19:29:00.000Z', complete = true) {
  const step = {
    spec_slug: 'capture', status: 'completed', completed_by: 'agent-codex',
    metadata: { snapshot: { capture_id: id, namespace: 'native-session', source: 'codex.collaboration.list_agents',
      collected_from: ended, collected_to: ended, received_at: ended, complete, result: complete ? 'captured' : 'failed',
      rows: complete ? [{ path: '/root/worker', status: 'running', current_task: null, native_id: null, observed_model: null }] : [],
    } },
  };
  return { id, kind: 'runtime-roster-capture', partition: 'real', metadata: {}, steps: [step] as [typeof step] };
}

describe('bounded native roster observation', () => {
  test('fresh observation remains separate from current registered task and model', () => {
    const p = projectRoster([parseCapture(packet())], now)[0]!;
    expect(p.state).toBe('fresh');
    expect(p.rows[0]!.status).toBe('running');
    expect(p.rows[0]!.current_task).toBeNull();
    expect(p.rows[0]!.native_id).toBeNull();
  });
  test('five-minute expiry is stale even when the packet was received later', () => {
    const v = packet('old', '2026-10-02T19:24:59Z');
    v.steps[0].metadata.snapshot.received_at = now;
    expect(projectRoster([parseCapture(v)], now)[0]!.state).toBe('stale');
  });
  test('arrival order and equal-time ties cannot change projection', () => {
    const a = parseCapture(packet('a'));
    const z = parseCapture(packet('z'));
    expect(projectRoster([a, z], now)).toEqual(projectRoster([z, a], now));
    expect(projectRoster([z, a], now)[0]!.captureId).toBe('z');
    expect(projectRoster([a, parseCapture(packet('older', '2026-10-02T19:20:00Z'))], now)[0]!.captureId).toBe('a');
  });
  test('failed capture leaves last complete evidence and cannot look like empty success', () => {
    const p = projectRoster([parseCapture(packet()), parseCapture(packet('failed', now, false))], now)[0]!;
    expect(p.state).toBe('unknown');
    expect(p.rows).toEqual([]);
    expect(p.lastComplete?.captureId).toBe('one');
    const empty = packet(); empty.steps[0].metadata.snapshot.rows = [];
    expect(projectRoster([parseCapture(empty)], now)[0]!.state).toBe('fresh');
  });
  test('submillisecond newer failure wins in either arrival order and offset alias', () => {
    const older = parseCapture(packet('z', '2026-10-02T19:29:00.100100Z'));
    const newer = parseCapture(packet('a', '2026-10-02T12:29:00.100900-07:00', false));
    for (const captures of [[older, newer], [newer, older]]) {
      const result = projectRoster(captures, now)[0]!;
      expect(result.state).toBe('unknown');
      expect(result.captureId).toBe('a');
      expect(result.rows).toEqual([]);
    }
    const alias = parseCapture(packet('z', '2026-10-02T12:29:00.100100-07:00'));
    expect(projectRoster([alias, older], now)[0]!.captureId).toBe('z');
  });
  test('fractional interval and future/expiry boundaries never round to fresh', () => {
    const inverted = packet('bad', '2026-10-02T19:29:00.100100Z');
    inverted.steps[0].metadata.snapshot.collected_from = '2026-10-02T19:29:00.100900Z';
    expect(() => parseCapture(inverted)).toThrow();
    expect(projectRoster([parseCapture(packet('future', '2026-10-02T19:30:00.000001Z'))], now)[0]!.state).toBe('unknown');
    expect(projectRoster([parseCapture(packet('almost', '2026-10-02T19:25:00.000001Z'))], now)[0]!.state).toBe('fresh');
    expect(projectRoster([parseCapture(packet('expired', '2026-10-02T19:25:00.000000Z'))], now)[0]!.state).toBe('stale');
  });
  test('path reuse across different namespaces is never joined', () => {
    const other = packet('other'); other.steps[0].metadata.snapshot.namespace = 'different-session';
    expect(projectRoster([parseCapture(packet()), parseCapture(other)], now)).toHaveLength(2);
  });
  test('unfinished, simulated, contradictory, duplicate-path or task-inferred captures refuse', () => {
    for (const change of [
      (v: ReturnType<typeof packet>) => { v.steps[0].status = 'active'; },
      (v: ReturnType<typeof packet>) => { v.partition = 'sim'; },
      (v: ReturnType<typeof packet>) => { v.steps[0].metadata.snapshot.collected_from = '2999-01-01T00:00:00Z'; },
      (v: ReturnType<typeof packet>) => { v.steps[0].metadata.snapshot.rows.push(v.steps[0].metadata.snapshot.rows[0]!); },
    ]) {
      const v = packet(); change(v);
      expect(() => parseCapture(v)).toThrow();
    }
  });
  test('complete counted pages and details are required, including late-page errors', async () => {
    const calls: string[] = [];
    const fake = async (url: string): Promise<unknown> => {
      calls.push(url);
      if (url.includes('offset=0')) return { data: [packet()], total: 2 };
      if (url.includes('offset=1')) return { data: [packet('two')], total: 2 };
      if (url.endsWith('/one')) return packet();
      return packet('two');
    };
    expect(await readRoster(fake)).toHaveLength(2);
    expect(calls.some((c) => c.includes('offset=1'))).toBe(true);
    await expect(readRoster(async (url) => url.includes('offset=0') ? { data: [packet()], total: 2 } : Promise.reject(new Error('unreadable')))).rejects.toThrow();
  });
  test('missing or ignored kind filter never becomes a healthy empty read', async () => {
    await expect(readRoster(async () => ({ data: [], total: null }))).rejects.toThrow();
    await expect(readRoster(async () => ({ data: [{ ...packet(), kind: 'agent-run' }], total: 1 }))).rejects.toThrow();
  });
  test('moving totals, duplicate pages and over-counts refuse before detail reads', async () => {
    for (const next of [
      { data: [packet('two')], total: 3 },
      { data: [packet()], total: 2 },
      { data: [packet('two'), packet('three')], total: 2 },
      { data: [], total: 2 },
    ]) {
      const calls: string[] = [];
      await expect(readRoster(async (url) => {
        calls.push(url);
        return url.includes('offset=0') ? { data: [packet()], total: 2 } : next;
      })).rejects.toThrow();
      expect(calls.every((url) => url.startsWith('/api/jobs?'))).toBe(true);
    }
  });
});
