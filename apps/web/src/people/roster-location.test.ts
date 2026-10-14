import { describe, expect, it } from 'bun:test';
import { readRosterLocation } from './roster-location';

describe('recorded location lookup', () => {
  it('reads the individual identity, including children, and encodes the path', async () => {
    const paths: string[] = [];
    const result = await readRosterLocation('child/zone', async input => {
      paths.push(String(input));
      return Response.json({ id: 'child/zone', name: 'Recorded child', parent_id: 'root' });
    });
    expect(paths).toEqual(['/api/locations/child%2Fzone']);
    expect(result).toEqual({ kind: 'known', name: 'Recorded child' });
  });
  it('distinguishes actual absence from denied, unavailable and dark reads', async () => {
    expect(await readRosterLocation('child', async () => new Response('', { status: 404 }))).toEqual({ kind: 'absent' });
    for (const status of [401, 403, 500, 503]) {
      expect(await readRosterLocation('child', async () => new Response('', { status }))).toEqual({ kind: 'failed' });
    }
    expect(await readRosterLocation('child', async () => { throw new Error('dark'); })).toEqual({ kind: 'failed' });
  });
  it('requires the returned identity and a nonempty recorded name', async () => {
    for (const body of [null, [], {}, { id: 'other', name: 'Other place' },
      { id: 'child', name: 3 }, { id: 'child', name: '' }, { id: 'child', name: '   ' },
      { id: 'child', display_name: 'Invented field' }]) {
      expect(await readRosterLocation('child', async () => Response.json(body))).toEqual({ kind: 'failed' });
    }
    expect(await readRosterLocation('child', async () => new Response('{'))).toEqual({ kind: 'failed' });
  });
});
