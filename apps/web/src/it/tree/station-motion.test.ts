import { describe, expect, test } from 'bun:test';
import { claimsRead, heldClaim, moveFrame, treeRead, workflowsRead } from '../../../tests/fixtures/station-tree';
import { parseMove } from './move-wire';
import { parseTree, type StationTree } from './station-tree';
import { parseHeldClaims } from './station-page';
import {
  MOVES_MAX, admitMove, deltaWords, feedState, parseProtocols, placeOf, resourceDelta, stationProtocols,
  type SessionMove,
} from './station-motion';

// WHAT THE STATION LINE READS OF MOTION (design c4854fd4 E3, decided by
// David 2026-11-07 23:38Z; backlog 87c5293f, car 3 of 3). The motion
// band's own line, cars and glide went with StationMotion.svelte
// (backlog 462fb22a); the station's protocols, where a step runs, what
// moved with a packet, the moves list and the feed are what
// StationLine.svelte still reads. REAL, CURRENT activity only (David,
// 2026-11-08 00:50Z): the moves list holds this page session's moves and
// nothing else — no history fold, no replay.

const tree = (): StationTree => parseTree(treeRead());
const protocols = () => parseProtocols(workflowsRead());
const proto = (kind: string) => {
  const p = protocols().find((x) => x.kind === kind);
  if (p === undefined) throw new Error(`no protocol ${kind}`);
  return p;
};

describe('the protocols a station owns', () => {
  test('a workflow row is read for its owner (a station id) and its steps, each with its placement keys', () => {
    const p = proto('ship-a-change');
    expect(p.owner).toBe('id-engineering');
    expect(p.steps.map((s) => s.slug)).toEqual(['opened', 'scope', 'build', 'gate', 'review', 'proven', 'landed-review', 'merged']);
    expect(p.steps.find((s) => s.slug === 'review')?.at).toBe('id-loading-dock');
    expect(p.steps.find((s) => s.slug === 'gate')?.executor).toBe('equipment:gates');
    expect(p.steps.find((s) => s.slug === 'landed-review')?.audienceStation).toBe('forge-ci');
    expect(proto('pr-train').steps.find((s) => s.slug === 'deploy')?.fulfilledBy).toBe('roll-a-train');
    expect(proto('backlog-item').owner).toBeNull();
    expect(() => parseProtocols({ data: [] })).toThrow('workflows read');
  });

  test('the chooser holds the protocols owned by the station or a station inside it, its own first', () => {
    expect(stationProtocols(tree(), 'engineering', protocols()).map((c) => [c.kind, c.owner])).toEqual([
      ['pr-train', 'engineering'],
      ['ship-a-change', 'engineering'],
      ['gate-run', 'gates'],
    ]);
    expect(stationProtocols(tree(), 'gates', protocols()).map((c) => c.kind)).toEqual(['gate-run']);
    // A row naming no owner (before T6 car A) belongs to no station's chooser.
    expect(stationProtocols(tree(), 'company', protocols()).map((c) => c.kind)).not.toContain('backlog-item');
    expect(stationProtocols(tree(), 'probe-shed', protocols())).toEqual([]);
  });
});

describe('where a step runs', () => {
  test('each step sits at the part that executes it, read from T6 placement, else its station spec or audience', () => {
    const t = tree();
    const steps = proto('ship-a-change').steps;
    const by = (slug: string) => placeOf(t, steps.find((s) => s.slug === slug)!);
    expect(by('review')).toEqual({ how: 'at', station: 'loading-dock' });
    expect(by('gate')).toEqual({ how: 'executor', station: 'gates' });
    expect(by('landed-review')).toEqual({ how: 'audience', station: 'forge-ci' });
    expect(by('scope')).toEqual({ how: 'not declared' });
    const deploy = proto('pr-train').steps.find((s) => s.slug === 'deploy')!;
    expect(placeOf(t, deploy)).toEqual({ how: 'hand-off', kind: 'roll-a-train' });
  });
});

describe('the constraint band moves with the car, when the claims read carries it', () => {
  const withAlloc = (job: string, station: string, amount: string) =>
    heldClaim(job, station, { allocations: [{ pool: 'host:w-1', version: 1, amount }] });

  test('the delta is what the packet’s claims hold after less what they held before, per pool and station', () => {
    const before = parseHeldClaims(claimsRead([withAlloc('j-1', 'engineering', '4'), withAlloc('j-9', 'gates', '4')]));
    const after = parseHeldClaims(claimsRead([withAlloc('j-1', 'gates', '4'), withAlloc('j-9', 'gates', '4')]));
    const d = resourceDelta(before, after, 'j-1');
    expect(d).toEqual([
      { pool: 'host:w-1', station: 'engineering', amount: -4 },
      { pool: 'host:w-1', station: 'gates', amount: 4 },
    ]);
    expect(deltaWords(d)).toBe('engineering releases 4 of host:w-1; gates takes 4 of host:w-1');
  });

  test('a claims read that carries no allocation for the packet says so, never a zero', () => {
    const none = parseHeldClaims(claimsRead([heldClaim('j-1', 'gates')]));
    expect(resourceDelta(none, none, 'j-1')).toBeNull();
    expect(deltaWords(null)).toBe('the claims read carries no allocation for it');
    const same = parseHeldClaims(claimsRead([withAlloc('j-1', 'gates', '4')]));
    expect(resourceDelta(same, same, 'j-1')).toEqual([]);
    expect(deltaWords([])).toBe('its claims hold what they held');
  });
});

describe('the list of moves: this page session’s, newest first, nothing replayed', () => {
  // An entry as StationLine.svelte builds one from a frame of the stream.
  const entry = (seq: number, packet = 'j-1'): SessionMove => {
    const mv = parseMove(JSON.stringify(moveFrame(seq, packet, `car ${packet}`, 'shop-floor', 'gates')))!;
    return {
      seq: mv.seq, at: mv.at, packet: mv.packet, label: mv.label, href: `/ux/jobs/${mv.packet}`, kind: mv.kind,
      route: 'build › gate', regions: `${mv.from ?? 'filed'} › ${mv.to ?? 'off the map'}`, delta: null,
    };
  };

  test('newest first, each seq once, and bounded', () => {
    let list: ReadonlyArray<SessionMove> = [];
    for (let i = 1; i <= MOVES_MAX + 3; i += 1) list = admitMove(list, entry(i));
    list = admitMove(list, entry(MOVES_MAX + 3));
    expect(list).toHaveLength(MOVES_MAX);
    expect(list[0]?.seq).toBe(MOVES_MAX + 3);
    expect(list.at(-1)?.seq).toBe(4);
  });
});

describe('stale looks stale: no live feed, no motion', () => {
  test('a live stream moves; a connecting one reads; a stream the browser gave up is stale, says why, and moves nothing', () => {
    expect(feedState('live', '')).toEqual({ plate: 'live', word: 'live', moves: true, why: null });
    expect(feedState('connecting', '')).toEqual({ plate: 'reading', word: 'connecting', moves: false, why: null });
    expect(feedState('down', 'HTTP 503')).toEqual({
      plate: 'stale', word: 'stale', moves: false,
      why: 'the moves stream is down (HTTP 503), so nothing on this line is drawn moving until it answers',
    });
  });
});
