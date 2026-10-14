import { describe, expect, test } from 'bun:test';
import { parseStationQueue } from './stationQueue';

const packet = (id: string) => ({ id, kind: 'backlog-item', title: id, opened_on: '2026-09-23', metadata: {}, tags: [] });
const envelope = { station: 'q.platform-admin.task', discipline: ['priority', 'age'], total: 3, data: [packet('urgent-new'), packet('standard-old')], over_limit: false, wip_limit: null };

describe('a selected station queue (fdc0ea0b)', () => {
  test('preserves server order, discipline and full total without claiming a truncated answer empty', () => {
    const result = parseStationQueue(envelope, envelope.station);
    expect(result.cards.map((card) => card.id)).toEqual(['urgent-new', 'standard-old']);
    expect(result.discipline).toBe('priority, then age');
    expect(result.total).toBe(3);
  });
  test('an empty measured queue is valid', () => {
    expect(parseStationQueue({ ...envelope, total: 0, data: [] }, envelope.station).cards).toEqual([]);
  });
  test('refuses unrelated, malformed and missing evidence rather than substituting an empty queue', () => {
    for (const body of [null, [], {}, { ...envelope, station: 'other' }, { ...envelope, total: null }, { ...envelope, total: 1 }, { ...envelope, discipline: null }, { ...envelope, data: [{}] }, { ...envelope, data: [packet('duplicate'), packet('duplicate')] }]) {
      expect(() => parseStationQueue(body, envelope.station)).toThrow();
    }
  });
});
