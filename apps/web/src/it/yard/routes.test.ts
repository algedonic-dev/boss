// THE ROUTES READ — design e765b3fc, car R3. The page parses what
// `GET /api/yard/routes` serves and draws exactly that; these pin the
// parse (a wrong server is a failed read, never an empty map) and the
// words a route's title and panel say.
import { describe, expect, it } from 'bun:test';
import { ROUTES, routesPayload } from '../../../tests/fixtures/yard';
import { UNREAD, exitBranches, exitNames, movesOn, parseRoutes, sourceLines, type Route } from './routes';

describe('parseRoutes', () => {
  it('reads every route, its two ends — null for off the map — and its sources', () => {
    const r = parseRoutes(routesPayload());
    expect(r.routes).toHaveLength(ROUTES.length);
    expect(r.observed).toBe(true);
    expect(r.window_hours).toBe(24);
    const exit = r.routes.find((x) => x.from === 'dock' && x.to === null)!;
    expect(exit.sources.map((s) => (s.source === 'workflow' ? s.step : s.source))).toEqual(['settled', 'cancelled']);
    expect(r.routes.find((x) => x.from === null && x.to === 'receiving')).toBeDefined();
  });

  it('refuses a payload that is not the routes read — a failed read, never a map with no edges', () => {
    expect(() => parseRoutes([])).toThrow('yard routes: expected an object');
    expect(() => parseRoutes({ routes: 'x' })).toThrow('expected a routes list');
  });

  it('refuses a route served with no source — the server pins that none is, and the page will not draw one', () => {
    expect(() => parseRoutes({ routes: [{ from: 'a', to: 'b', declared: true, sources: [] }] })).toThrow('served with no source');
  });

  it('refuses a route with neither end on the map, and a source it cannot name', () => {
    expect(() => parseRoutes({ routes: [{ from: null, to: null, declared: true, sources: [{ source: 'observed', moves: 1 }] }] })).toThrow(
      'neither end',
    );
    expect(() => parseRoutes({ routes: [{ from: 'a', to: 'b', declared: true, sources: [{ source: 'rumour' }] }] })).toThrow('unknown kind');
  });
});

describe('a route in words', () => {
  const exit: Route = {
    from: 'marshalling',
    to: null,
    declared: true,
    sources: [
      { source: 'workflow', workflow: 'backlog-item', version: 4, step: 'duplicate', via: 'closed' },
      { source: 'workflow', workflow: 'user-feedback', version: 4, step: 'duplicate', via: 'closed' },
      { source: 'workflow', workflow: 'backlog-item', version: 4, step: 'stale', via: 'closed' },
      { source: 'hand-off', by: 'rule:refresh-publish-drift-daily', from: null, to: null, why: 'superseded', terminal: null },
      { source: 'observed', moves: 12, last_at: '', kind: null, terminal: null },
      { source: 'observed', moves: 3, last_at: '', kind: null, terminal: null },
    ],
  };

  it('an exit names each terminal once, and the hand-off by its maker', () => {
    expect(exitNames(exit)).toEqual(['duplicate', 'stale', 'refresh-publish-drift-daily']);
  });

  it('counts the moves the record observed, and none where it counted none', () => {
    expect(movesOn(exit)).toBe(15);
    expect(movesOn({ ...exit, sources: exit.sources.slice(0, 1) })).toBeNull();
  });

  it('parses the terminal a hand-off hands off, and the kind and terminal an exit move was counted by', () => {
    const r = parseRoutes({
      routes: [
        {
          from: 'garage',
          to: null,
          declared: true,
          sources: [
            { source: 'hand-off', by: 'verb:rerail', from: 'gate-run@green', to: null, why: 'absorbed', terminal: 'green' },
            { source: 'observed', moves: 5, last_at: '', kind: 'gate-run', terminal: 'green' },
            { source: 'observed', moves: 1, last_at: '' },
          ],
        },
      ],
    }).routes[0]!;
    expect(r.sources[0]).toMatchObject({ source: 'hand-off', terminal: 'green' });
    expect(r.sources[1]).toMatchObject({ source: 'observed', kind: 'gate-run', terminal: 'green' });
    expect(r.sources[2]).toMatchObject({ source: 'observed', kind: null, terminal: null });
  });

  it('says one line per source', () => {
    expect(sourceLines(exit, 24)).toHaveLength(exit.sources.length);
    expect(sourceLines(exit, 24)[0]).toBe('backlog-item v4, step duplicate (closed)');
    expect(sourceLines(exit, 24)[3]).toBe('rule:refresh-publish-drift-daily: superseded');
  });
});

// THE EXIT FAN'S BRANCHES (backlog 3c8b5653, design f0313eda, E2): one
// branch per terminal, in name order so a branch keeps its place as the
// counts change, each with the exits the record counted by it — read off
// the served routes, whose observed counts carry the kind and terminal
// each exit left by (backlog e23005c0).
describe('exitBranches — one branch per terminal, with its count', () => {
  const wf = (workflow: string, step: string) => ({ source: 'workflow' as const, workflow, version: 1, step, via: 'closed' });
  const seen = (kind: string | null, terminal: string | null, moves: number) =>
    ({ source: 'observed' as const, moves, last_at: '', kind, terminal });
  const marshalling: Route = {
    from: 'marshalling',
    to: null,
    declared: true,
    sources: [
      wf('backlog-item', 'stale'),
      wf('backlog-item', 'closed'),
      wf('ops-request', 'answered'),
      wf('backlog-item', 'duplicate'),
      seen('backlog-item', 'closed', 42),
      seen('ops-request', 'answered', 217),
      seen('backlog-item', 'stale', 2),
    ],
  };

  it('names each terminal once, sorted by name, whatever order the sources came in', () => {
    expect(exitBranches(marshalling).map((b) => b.name)).toEqual(['answered', 'closed', 'duplicate', 'stale']);
  });

  it('counts each branch from the observed exits, and a declared terminal nothing left by counts 0', () => {
    expect(exitBranches(marshalling).map((b) => [b.name, b.count, b.declared])).toEqual([
      ['answered', 217, true],
      ['closed', 42, true],
      ['duplicate', 0, true],
      ['stale', 2, true],
    ]);
  });

  it('the branch counts sum to the exits the route carries — every exit lands on exactly one branch', () => {
    const r: Route = { ...marshalling, sources: [...marshalling.sources, seen('agent-run', 'landed', 19), seen(null, null, 3)] };
    const sum = exitBranches(r).reduce((n, b) => n + b.count, 0);
    expect(sum).toBe(movesOn(r)!);
  });

  it('a terminal packets left by that no source names for their kind is its own branch, undeclared', () => {
    const r: Route = { ...marshalling, sources: [...marshalling.sources, seen('agent-run', 'landed', 19)] };
    expect(exitBranches(r).find((b) => b.name === 'landed')).toMatchObject({ count: 19, declared: false });
    // Same terminal name, another kind: its count stands apart from the
    // declared branch rather than borrowing its declaration.
    const clash: Route = { ...marshalling, sources: [...marshalling.sources, seen('agent-run', 'closed', 4)] };
    const closed = exitBranches(clash).filter((b) => b.name.startsWith('closed'));
    expect(closed.map((b) => [b.name, b.count, b.declared])).toEqual([
      ['closed', 42, true],
      ['closed · agent-run', 4, false],
    ]);
  });

  it('a hand-off declares the terminal it names, and one that takes the packet whatever it closes on declares its kind', () => {
    const garage: Route = {
      from: 'garage',
      to: null,
      declared: true,
      sources: [
        { source: 'hand-off', by: 'verb:rerail', from: 'gate-run@green', to: null, why: 'absorbed', terminal: 'green' },
        { source: 'hand-off', by: 'verb:gate', from: 'gate-run@failed', to: null, why: 'superseded', terminal: null },
        seen('gate-run', 'green', 5),
        seen('gate-run', 'failed', 2),
      ],
    };
    expect(exitBranches(garage).map((b) => [b.name, b.count, b.declared])).toEqual([
      ['failed', 2, true],
      ['gate', 0, true],
      ['green', 5, true],
    ]);
  });

  it('an exit whose packet the mover could not read back lands on one branch, judged by the route', () => {
    const r: Route = { ...marshalling, sources: [...marshalling.sources, seen(null, null, 3)] };
    expect(exitBranches(r).find((b) => b.name === UNREAD)).toMatchObject({ count: 3, declared: true });
  });
});
