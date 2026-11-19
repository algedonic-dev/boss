import { describe, expect, it } from 'bun:test';
import { parseMove, parseResync, type Move } from './move-wire';

let seq = 0;
const move = (over: Partial<Move> = {}): Move => ({
  seq: ++seq,
  at: '2026-09-26T03:18:00Z',
  packet: `p-${seq}`,
  kind: 'ship-a-change',
  label: 'feat/a-thing',
  from: 'a',
  to: 'b',
  declared: true,
  cause_event_id: '00000000-0000-0000-0000-000000000001',
  cause_seq: 101,
  cause_kind: 'jobs.step.updated',
  handoff_from: null,
  lineage: null,
  aboard: [],
  ...over,
});

describe('the wire (crates/core/boss-jobs/src/moves.rs Recorded, flattened)', () => {
  it('reads a move frame as the stream sends it', () => {
    const m = parseMove(
      JSON.stringify({
        seq: 7, at: '2026-09-26T03:18:00Z', packet: 'car-1', kind: 'ship-a-change', label: 'fix/x',
        from: 'gates', to: 'dock', declared: true, cause_event_id: 'e', cause_seq: 104, cause_kind: 'jobs.job.created',
        handoff_from: 'gate-1', lineage: 'branch', aboard: [],
      }),
    );
    expect(m?.seq).toBe(7);
    expect(m?.handoff_from).toBe('gate-1');
  });
  it('an off-ramp is to: null, an unjudged route is declared: null', () => {
    const m = parseMove(JSON.stringify({ ...move(), to: null, declared: null }));
    expect(m?.to).toBeNull();
    expect(m?.declared).toBeNull();
  });
  it('refuses a frame that is not a move rather than drawing half of one', () => {
    for (const bad of ['', 'not json', '[]', JSON.stringify({ seq: 'x' }), JSON.stringify({ ...move(), aboard: 'x' })]) {
      expect(parseMove(bad)).toBeNull();
    }
  });
  it('reads the resync frame', () => {
    expect(parseResync(JSON.stringify({ frame: 'resync', seq: 9, reason: 'connected' }))).toEqual({ seq: 9, reason: 'connected' });
    expect(parseResync('{}')).toBeNull();
  });
});
