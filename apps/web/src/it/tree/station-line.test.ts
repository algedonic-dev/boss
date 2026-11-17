import { describe, expect, test } from 'bun:test';
import {
  TREE_ROWS, claimsRead, gateRunPacket, heldClaim, kindsRead, openPacket, packetsRead, treeRead, workflowRow, workflowsRead,
} from '../../../tests/fixtures/station-tree';
import { parseTree, type StationTree } from './station-tree';
import { parseHeldClaims, type HeldClaim } from './station-page';
import { stationProtocols } from './station-motion';
import {
  BUNDLE_ABOVE, SEATS_MAX, ago, boxGeometry, defaultProtocol, parseKindCounts, parseLinePackets, parseLineProtocols, protocolChips, stationLine,
  type LineStop, type StationLineModel,
} from './station-line';

// THE STATION LINE (design 08d1530e car B, approved by David 2026-11-12;
// backlog 9d0b0944). The engineering page body is the busiest protocol's
// line: each stop a station box with its admission column, its seats as
// cells holding named packets with dwell, and every path between
// them. Nothing is placed by guessing: a packet whose step names no
// station stands in the tray, counted by step and pinned version.

const NOW = Date.parse('2026-11-08T12:00:00Z');

/** The fixture tree plus a `build` station that names no step spec but
 *  whose admission predicate names ship-a-change's build step. */
const ROWS = [
  ...TREE_ROWS,
  { name: 'build', parent: 'engineering', capacity: 3, predicate: { kind: 'ship-a-change', status: 'open', step: { slug: 'build', status_in: ['ready', 'active'] } } },
];
const tree = (): StationTree => parseTree(treeRead('company', ROWS));
const read = () => parseLineProtocols(workflowsRead());
const proto = (kind: string) => {
  const p = read().protocols.find((x) => x.kind === kind);
  if (p === undefined) throw new Error(`no protocol ${kind}`);
  return p;
};

/** A packet standing at `at`, its earlier steps completed at `done`. */
const packet = (id: string, title: string, at: string, over: Readonly<Record<string, unknown>> = {}, done = '2026-11-08T09:00:00Z', kind = 'ship-a-change') => {
  const p = openPacket(id, title, at, kind);
  return {
    ...p,
    workflow_version: 1,
    steps: p.steps.map((s) => ({ ...s, ...(s.status === 'completed' ? { completed_at: done } : {}) })),
    ...over,
  };
};
/** Step `at` of packet `p` set to `status`. */
const statused = (p: ReturnType<typeof packet>, at: string, status: string) => ({
  ...p, steps: p.steps.map((s) => (s.spec_slug === at ? { ...s, status } : s)),
});

const claims = (rows: ReadonlyArray<Readonly<Record<string, unknown>>>): ReadonlyArray<HeldClaim> => parseHeldClaims(claimsRead(rows));

const line = (kind: string, rows: ReadonlyArray<unknown>, held: ReadonlyArray<HeldClaim> | null = [], total?: number): StationLineModel => {
  const m = stationLine({
    tree: tree(), station: 'engineering', protocol: proto(kind), version: 1,
    packets: parseLinePackets(packetsRead(rows as never, total)), claims: held, now: NOW,
  });
  if (m === null) throw new Error('no line');
  return m;
};
const stop = (m: StationLineModel, slug: string): LineStop => {
  const s = m.stops.find((x) => x.slug === slug);
  if (s === undefined) throw new Error(`no stop ${slug}`);
  return s;
};

describe('the protocol chosen, and the others as count chips', () => {
  const choices = () => stationProtocols(tree(), 'engineering', read().protocols);
  const counts = () => parseKindCounts(kindsRead([
    { kind: 'ship-a-change', open: 89, byVersion: { '15': 51, '12': 38 } },
    { kind: 'pr-train', open: 1 },
    { kind: 'gate-run', open: 4 },
  ]));

  test('the default is the owned protocol with the most open packets, not the first by name', () => {
    expect(choices()[0]?.kind).toBe('pr-train');
    expect(defaultProtocol(choices(), counts())).toBe('ship-a-change');
  });

  test('the chips are the owned protocols with open packets, most first, and the rest are counted as none open', () => {
    const extra = parseLineProtocols([...workflowsRead(), workflowRow('quiet-one', 'engineering', [{ slug: 'x' }])]).protocols;
    const c = protocolChips(stationProtocols(tree(), 'engineering', extra), counts(), 'ship-a-change');
    expect(c.chips.map((x) => [x.kind, x.open])).toEqual([['ship-a-change', 89], ['gate-run', 4], ['pr-train', 1]]);
    expect(c.none).toBe(1);
  });

  test('an unread count chooses the first owned protocol and draws every chip with no number', () => {
    expect(defaultProtocol(choices(), null)).toBe('pr-train');
    expect(protocolChips(choices(), null, 'pr-train').chips.every((x) => x.open === null)).toBe(true);
  });

  test('a kinds read of the wrong shape throws, never an empty chooser', () => {
    expect(() => parseKindCounts({ data: [] })).toThrow('kinds read');
    expect(parseKindCounts(kindsRead([{ kind: 'k', open: 2, byVersion: { '3': 2 } }])).get('k')).toEqual({ kind: 'k', open: 2, byVersion: { '3': 2 } });
  });

  test('the protocol row is read with its version', () => {
    expect(read().versions.get('ship-a-change')).toBe(1);
  });
});

describe('each stop is the station its step names, and only a station the record names', () => {
  test('a step spec places a stop; a step that names none is placed by the one station whose admission predicate names it', () => {
    const m = line('ship-a-change', []);
    expect(m.stops.map((s) => [s.slug, s.station, s.placedBy])).toEqual([
      ['scope', null, null],
      ['build', 'build', 'admission predicate'],
      ['gate', 'gates', 'executor'],
      ['review', 'loading-dock', 'at'],
      ['proven', 'probe-shed', 'executor'],
      ['landed-review', 'forge-ci', 'audience'],
    ]);
    expect(stop(m, 'scope').note).toBe('its step names no station');
    expect(stop(m, 'gate').machines).toEqual(['automation:gate-runner']);
    expect(stop(m, 'gate').seats).toBe(2);
    expect(stop(m, 'build').seats).toBe(3);
  });

  test('a predicate with a further condition admits only some packets at the step, so it places none of them, and the tray says why', () => {
    const rows = [...ROWS, { name: 'scoping', parent: 'engineering', predicate: { kind: 'ship-a-change', status: 'open', metadata_present: ['branch'], step: { slug: 'scope' } } }];
    const m = stationLine({
      tree: parseTree(treeRead('company', rows)), station: 'engineering', protocol: proto('ship-a-change'), version: 1,
      packets: parseLinePackets(packetsRead([packet('j-1', 'Scoping', 'scope')] as never)), claims: [], now: NOW,
    });
    const s = m?.stops.find((x) => x.slug === 'scope');
    expect(s?.station).toBeNull();
    const why = 'its step names no station, and scoping admits only some of them, by a predicate with more conditions';
    expect(s?.note).toBe(why);
    expect(m?.tray.map((r) => [r.reason, r.count])).toEqual([[why, 1]]);
  });

  test('a hand-off stays the last stop and holds no box', () => {
    const m = line('pr-train', []);
    expect(m.stops.map((s) => [s.slug, s.station, s.handoff])).toEqual([['ci', 'forge-ci', null], ['deploy', null, 'roll-a-train']]);
  });
});

describe('inside a station: the admission column, then its seats', () => {
  test('a ready step with no open claim has arrived and is not admitted; its age runs from the step before it completed', () => {
    const m = line('ship-a-change', [packet('j-1', 'A seat names its lease', 'build', {}, '2026-11-08T09:00:00Z')].map((p) => statused(p, 'build', 'ready')));
    const b = stop(m, 'build');
    expect(b.in.count).toBe(1);
    expect(b.in.tags.map((t) => t.title)).toEqual(['A seat names its lease']);
    expect(b.in.oldest).toBe('3 h');
    expect(b.cells.filter((c) => c.packet !== null)).toHaveLength(0);
  });

  test('a claimed step holds a seat named for its packet, marked by the claim executor_kind, with the dwell since the claim', () => {
    const rows = [
      statused(packet('j-1', 'Ready but claimed', 'build'), 'build', 'ready'),
      packet('j-2', 'Active and claimed by a person', 'build'),
      packet('j-3', 'Active with no claim', 'build'),
    ];
    const held = claims([
      heldClaim('j-1', 'a.platform-admin.opus-5-5', { step_id: 'j-1-build', since: '2026-11-08T11:30:00Z' }),
      heldClaim('j-2', 'build', { step_id: 'j-2-build', executor_kind: 'person', holder: 'emp-david', since: '2026-11-08T10:00:00Z' }),
    ]);
    const b = stop(line('ship-a-change', rows, held), 'build');
    expect(b.in.count).toBe(0);
    const held3 = b.cells.filter((c) => c.packet !== null).map((c) => [c.packet?.id, c.mark, c.holder, c.dwell]);
    expect(held3).toEqual([
      ['j-2', '●', 'emp-david', '2 h'],
      ['j-1', '◆', 'agent-claude', '30 m'],
      ['j-3', null, 'no claim recorded', null],
    ]);
    expect(b.cells.filter((c) => c.packet === null)).toHaveLength(0);
  });

  test('a declared seat no packet holds is drawn free, and a station with a queue and every seat free is idle with a queue', () => {
    const b = stop(line('ship-a-change', [statused(packet('j-1', 'Waiting', 'build'), 'build', 'ready')]), 'build');
    expect(b.cells.map((c) => c.free)).toEqual([true, true, true]);
    expect(b.idle).toBe(true);
  });

  test('an unread claims read keeps every ready step in admission and says a claimed one cannot be told apart', () => {
    const b = stop(line('ship-a-change', [statused(packet('j-1', 'Waiting', 'build'), 'build', 'ready')], null), 'build');
    expect(b.in.count).toBe(1);
    expect(b.notes).toContain('the claims are unread, so a claimed ready step is not told apart from a waiting one');
  });

  test('past the seats the box draws, the rest are counted', () => {
    const rows = Array.from({ length: SEATS_MAX + 2 }, (_, i) => packet(`j-${i}`, `Packet ${i}`, 'build'));
    const b = stop(line('ship-a-change', rows), 'build');
    expect(b.cells).toHaveLength(SEATS_MAX);
    expect(b.more).toBe(2);
    expect(b.notes).toContain(`${SEATS_MAX + 2} seats held, more than the 3 the row declares`);
  });
});

describe('the gates: seats from the packet metadata, because the step and its claims do not record one', () => {
  const gates = (rows: ReadonlyArray<unknown>) => stop(line('gate-run', rows), 'record-verdict');

  test('a gate_bay object seats a packet, a reservation not yet consumed is the machine queue, and neither is admission', () => {
    const b = gates([
      gateRunPacket('g-1', 'Gate: feat/a', 'lane-1', '2026-11-08T11:20:00Z'),
      gateRunPacket('g-2', 'Gate: feat/b', 'queued', '2026-11-08T11:49:00Z'),
      gateRunPacket('g-3', 'Gate: feat/c', null),
    ]);
    expect(b.station).toBe('gates');
    expect(b.seats).toBe(2);
    expect(b.cells.map((c) => [c.label, c.packet?.id ?? null, c.dwell, c.free])).toEqual([
      ['seated', 'g-1', '40 m', false],
      ['a seat', null, null, true],
    ]);
    expect(b.queue?.count).toBe(1);
    expect(b.queue?.tags.map((t) => t.id)).toEqual(['g-2']);
    expect(b.queue?.oldest).toBe('11 m');
    expect(b.in.count).toBe(1);
    expect(b.in.tags.map((t) => t.id)).toEqual(['g-3']);
    expect(b.notes).toContain('a seat here is read from packet metadata (gate_bay, gate_reservation): the step stays ready and unclaimed, and no claim names the seat, so its dwell is counted from when the packet was queued');
  });

  test('a station whose packets carry no seat mark has no machine queue', () => {
    expect(stop(line('ship-a-change', [packet('j-1', 'x', 'build')]), 'build').queue).toBeNull();
  });
});

describe('the honest tray: never placed by guessing', () => {
  test('a packet at a step that names no station is counted by step and pinned version, not drawn on the line', () => {
    const m = line('ship-a-change', [packet('j-1', 'Scoping', 'scope'), packet('j-2', 'Also scoping', 'scope', { workflow_version: 1 })]);
    expect(m.tray).toEqual([{ reason: 'its step names no station', step: 'scope', version: 1, count: 2, sample: [
      { id: 'j-1', title: 'Scoping', href: '/ux/jobs/j-1' }, { id: 'j-2', title: 'Also scoping', href: '/ux/jobs/j-2' },
    ] }]);
    expect(stop(m, 'scope').tray).toBe(2);
  });

  test('a packet pinned to an older version is drawn only where the station admits the step whatever the version', () => {
    const m = line('ship-a-change', [
      packet('j-1', 'Old build', 'build', { workflow_version: 0 }),
      packet('j-2', 'Old gate', 'gate', { workflow_version: 0 }),
    ]);
    expect(stop(m, 'build').cells.filter((c) => c.packet !== null).map((c) => c.packet?.id)).toEqual(['j-1']);
    expect(m.tray.map((r) => [r.reason, r.step, r.version, r.count])).toEqual([
      ['pinned to v0, and this line places that step by v1’s row', 'gate', 0, 1],
    ]);
  });

  test('a packet at no step of the line is in the tray, and the rows the page did not read are counted', () => {
    const m = line('ship-a-change', [packet('j-1', 'Closed out', 'merged')], [], 5);
    expect(m.tray.map((r) => [r.reason, r.count])).toEqual([['at no step of this line', 1]]);
    expect(m.read).toEqual({ shown: 1, total: 5, unread: 4 });
  });
});

describe('every internal path is drawn, bundled above four in parallel', () => {
  const g = (rows: ReadonlyArray<unknown>, slug: string, kind = 'ship-a-change') => boxGeometry(stop(line(kind, rows), slug));

  test('three seats are three paths in and three out, each its own line, with no bundle', () => {
    const geo = g([packet('j-1', 'x', 'build')], 'build');
    expect(geo.cells).toHaveLength(3);
    expect(geo.paths.filter((p) => p.part === 'fan-in')).toHaveLength(3);
    expect(geo.paths.filter((p) => p.part === 'fan-out')).toHaveLength(3);
    expect(geo.bundles).toEqual([]);
    // The two free seats' paths are drawn idle.
    expect(geo.paths.filter((p) => p.part === 'fan-in' && p.idle)).toHaveLength(2);
  });

  test(`more than ${BUNDLE_ABOVE} parallel paths are one counted line that fans out at the parts`, () => {
    const rows = Array.from({ length: 6 }, (_, i) => packet(`j-${i}`, `P${i}`, 'build'));
    const geo = g(rows, 'build');
    expect(geo.cells).toHaveLength(6);
    expect(geo.bundles.map((b) => b.count)).toEqual([6, 6]);
    expect(geo.paths.filter((p) => p.part === 'fan-in')).toHaveLength(6);
  });

  test('a machine queue is a node of its own between admission and the seats', () => {
    const geo = g([gateRunPacket('g-1', 'Gate: a', 'queued')], 'record-verdict', 'gate-run');
    expect(geo.queueNode).not.toBeNull();
    expect(geo.paths.some((p) => p.part === 'admit')).toBe(true);
  });
});

test('an age reads in minutes, then hours, then days', () => {
  expect([ago(4 * 60_000), ago(5 * 3_600_000), ago(3 * 86_400_000), ago(-5)]).toEqual(['4 m', '5 h', '3 d', '0 m']);
});
